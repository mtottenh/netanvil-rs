//! Agent server: a remotely controllable load test node.
//!
//! Wraps the existing `ControlServer` with lifecycle management.
//! Accepts `POST /test/start` to begin a test, `GET /info` for node info,
//! plus all existing control endpoints (`GET /metrics`, `PUT /rate`, `POST /stop`).
//!
//! Lifecycle: idle → POST /test/start → running → (test ends or POST /stop) → idle

use std::sync::{Arc, Mutex};

use netanvil_core::{run_test_with_api, TestResult};
use netanvil_http::HttpExecutor;
use netanvil_types::{NodeId, NodeInfo, NodeState, TestConfig, WorkerCommand};

use crate::handlers;
use crate::types::*;

/// Internal state for agent lifecycle.
#[derive(Debug)]
struct AgentInner {
    node_id: NodeId,
    cores: usize,
    state: NodeState,
    shared_state: SharedState,
    /// Command sender for the running test (None when idle).
    command_tx: Option<flume::Sender<WorkerCommand>>,
    /// Latest test result (available after completion).
    last_result: Option<TestResult>,
}

/// Agent server: long-lived HTTP server that accepts tests on demand.
pub struct AgentServer {
    server: tiny_http::Server,
    inner: Arc<Mutex<AgentInner>>,
}

impl AgentServer {
    pub fn new(port: u16, cores: usize) -> std::io::Result<Self> {
        let addr = format!("0.0.0.0:{port}");
        let server = tiny_http::Server::http(&addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e.to_string()))?;

        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".into());

        let inner = Arc::new(Mutex::new(AgentInner {
            node_id: NodeId(format!("{hostname}:{port}")),
            cores,
            state: NodeState::Idle,
            shared_state: SharedState::new(),
            command_tx: None,
            last_result: None,
        }));

        Ok(Self { server, inner })
    }

    /// Run the agent server (blocking). Call this from the main thread.
    pub fn run(&self) {
        tracing::info!("Agent listening on {}", self.server.server_addr());
        loop {
            let request = match self.server.recv() {
                Ok(req) => req,
                Err(_) => break,
            };
            self.handle_request(request);
        }
    }

    fn handle_request(&self, request: tiny_http::Request) {
        let method = request.method().as_str().to_uppercase();
        let path = request.url().to_string();

        tracing::debug!("{method} {path}");

        match (method.as_str(), path.as_str()) {
            // Agent-specific endpoints
            ("POST", "/test/start") => self.handle_start_test(request),
            ("GET", "/info") => self.handle_get_info(request),

            // Standard control endpoints (work when a test is running)
            ("GET", "/status") => {
                let inner = self.inner.lock().unwrap();
                handlers::handle_get_status(request, &inner.shared_state);
            }
            ("GET", "/metrics") => {
                let inner = self.inner.lock().unwrap();
                handlers::handle_get_metrics(request, &inner.shared_state);
            }
            ("GET", "/metrics/prometheus") => {
                let inner = self.inner.lock().unwrap();
                handlers::handle_get_metrics_prometheus(request, &inner.shared_state);
            }
            ("PUT", "/rate") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_put_rate(request, tx);
                } else {
                    handlers::respond_json(
                        request,
                        409,
                        &ApiResponse::error("no test running"),
                    );
                }
            }
            ("PUT", "/targets") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_put_targets(request, tx);
                } else {
                    handlers::respond_json(
                        request,
                        409,
                        &ApiResponse::error("no test running"),
                    );
                }
            }
            ("PUT", "/headers") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_put_headers(request, tx);
                } else {
                    handlers::respond_json(
                        request,
                        409,
                        &ApiResponse::error("no test running"),
                    );
                }
            }
            ("POST", "/stop") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_post_stop(request, tx);
                } else {
                    handlers::respond_json(
                        request,
                        409,
                        &ApiResponse::error("no test running"),
                    );
                }
            }
            _ => handlers::handle_not_found(request),
        }
    }

    fn handle_start_test(&self, request: tiny_http::Request) {
        // Read and parse TestConfig from body
        let config = {
            let mut body = String::new();
            let mut request = request;
            if let Err(e) = request.as_reader().read_to_string(&mut body) {
                handlers::respond_json(
                    request,
                    400,
                    &ApiResponse::error(format!("read error: {e}")),
                );
                return;
            }
            let config: TestConfig = match serde_json::from_str(&body) {
                Ok(c) => c,
                Err(e) => {
                    handlers::respond_json(
                        request,
                        400,
                        &ApiResponse::error(format!("invalid TestConfig JSON: {e}")),
                    );
                    return;
                }
            };

            // Check if already running
            let mut inner = self.inner.lock().unwrap();
            if inner.state == NodeState::Running {
                handlers::respond_json(
                    request,
                    409,
                    &ApiResponse::error("test already running"),
                );
                return;
            }

            // Set up for new test
            let (cmd_tx, cmd_rx) = flume::unbounded();
            inner.state = NodeState::Running;
            inner.shared_state = SharedState::new();
            inner.command_tx = Some(cmd_tx);
            inner.last_result = None;

            handlers::respond_json(request, 200, &ApiResponse::success());

            (config, cmd_rx, inner.shared_state.clone())
        };

        let (test_config, cmd_rx, shared_state) = config;

        // Spawn test on a background thread
        let inner = self.inner.clone();
        let request_timeout = test_config.connections.request_timeout;

        std::thread::Builder::new()
            .name("netanvil-agent-test".into())
            .spawn(move || {
                let progress_state = shared_state.clone();
                let result = run_test_with_api(
                    test_config,
                    move || HttpExecutor::with_timeout(request_timeout),
                    move |update| {
                        progress_state.update_from_progress(update);
                    },
                    cmd_rx,
                );

                // Update agent state
                let mut agent = inner.lock().unwrap();
                agent.state = NodeState::Idle;
                agent.command_tx = None;
                match result {
                    Ok(r) => {
                        tracing::info!(
                            total_requests = r.total_requests,
                            total_errors = r.total_errors,
                            "test completed"
                        );
                        agent.last_result = Some(r);
                    }
                    Err(e) => {
                        tracing::error!("test failed: {e}");
                    }
                }
            })
            .expect("spawn agent test thread");
    }

    fn handle_get_info(&self, request: tiny_http::Request) {
        let inner = self.inner.lock().unwrap();
        let info = NodeInfo {
            id: inner.node_id.clone(),
            addr: self.server.server_addr().to_string(),
            cores: inner.cores,
            state: inner.state,
        };
        handlers::respond_json(request, 200, &info);
    }
}
