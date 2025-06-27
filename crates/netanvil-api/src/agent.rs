//! Agent server: a remotely controllable load test node.
//!
//! Wraps the existing `ControlServer` with lifecycle management.
//! Accepts `POST /test/start` to begin a test, `GET /info` for node info,
//! plus all existing control endpoints (`GET /metrics`, `PUT /rate`, `POST /stop`).
//!
//! Supports two modes:
//! - **Plain HTTP** (default): uses tiny_http, no authentication
//! - **mTLS**: uses custom TLS server with client certificate verification
//!
//! Lifecycle: idle → POST /test/start → running → (test ends or POST /stop) → idle

use std::sync::{Arc, Mutex};

use netanvil_core::{TestBuilder, TestResult};
use netanvil_http::HttpExecutor;
use netanvil_types::{
    NodeId, NodeInfo, NodeState, PluginType, RequestGenerator, TestConfig, TlsConfig, WorkerCommand,
};

use crate::handlers;
use crate::tls::{HttpRequest, HttpResponse, MtlsServer};
use crate::types::*;

/// Internal state for agent lifecycle.
#[derive(Debug)]
struct AgentInner {
    node_id: NodeId,
    listen_addr: String,
    cores: usize,
    state: NodeState,
    shared_state: SharedState,
    /// Command sender for the running test (None when idle).
    command_tx: Option<flume::Sender<WorkerCommand>>,
    /// Latest test result (available after completion).
    last_result: Option<TestResult>,
}

/// Agent server: long-lived HTTP server that accepts tests on demand.
///
/// Create with `new()` for plain HTTP or `with_tls()` for mTLS.
pub struct AgentServer {
    /// Plain HTTP server (used when no TLS configured).
    http_server: Option<tiny_http::Server>,
    /// mTLS server (used when TLS configured).
    mtls_server: Option<MtlsServer>,
    inner: Arc<Mutex<AgentInner>>,
}

impl AgentServer {
    /// Create a plain HTTP agent (no TLS).
    pub fn new(port: u16, cores: usize) -> std::io::Result<Self> {
        let addr = format!("0.0.0.0:{port}");
        let server = tiny_http::Server::http(&addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e.to_string()))?;

        let inner = make_inner(port, &addr, cores);

        Ok(Self {
            http_server: Some(server),
            mtls_server: None,
            inner,
        })
    }

    /// Create an mTLS agent with client certificate verification.
    pub fn with_tls(port: u16, cores: usize, tls: &TlsConfig) -> std::io::Result<Self> {
        let addr = format!("0.0.0.0:{port}");
        let server = MtlsServer::new(&addr, tls)
            .map_err(|e| std::io::Error::other(format!("mTLS setup: {e}")))?;

        let inner = make_inner(port, &addr, cores);

        Ok(Self {
            http_server: None,
            mtls_server: Some(server),
            inner,
        })
    }

    /// Run the agent server (blocking). Call from the main thread.
    pub fn run(&self) {
        if let Some(ref http) = self.http_server {
            tracing::info!("agent listening on {} (plain HTTP)", http.server_addr());
            self.run_http(http);
        } else if let Some(ref mtls) = self.mtls_server {
            tracing::info!("agent listening (mTLS)");
            self.run_mtls(mtls);
        }
    }

    // --- Plain HTTP mode (tiny_http) ----------------------------------------

    fn run_http(&self, server: &tiny_http::Server) {
        loop {
            let request = match server.recv() {
                Ok(req) => req,
                Err(_) => break,
            };
            self.handle_tiny_http(request);
        }
    }

    fn handle_tiny_http(&self, request: tiny_http::Request) {
        let method = request.method().as_str().to_uppercase();
        let path = request.url().to_string();

        tracing::debug!("{method} {path}");

        match (method.as_str(), path.as_str()) {
            ("POST", "/test/start") => self.handle_start_test_http(request),
            ("GET", "/info") => self.handle_get_info_http(request),
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
                    handlers::respond_json(request, 409, &ApiResponse::error("no test running"));
                }
            }
            ("PUT", "/targets") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_put_targets(request, tx);
                } else {
                    handlers::respond_json(request, 409, &ApiResponse::error("no test running"));
                }
            }
            ("PUT", "/headers") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_put_headers(request, tx);
                } else {
                    handlers::respond_json(request, 409, &ApiResponse::error("no test running"));
                }
            }
            ("PUT", "/signal") => {
                let inner = self.inner.lock().unwrap();
                handlers::handle_put_signal(request, &inner.shared_state);
            }
            ("POST", "/stop") => {
                let inner = self.inner.lock().unwrap();
                if let Some(ref tx) = inner.command_tx {
                    handlers::handle_post_stop(request, tx);
                } else {
                    handlers::respond_json(request, 409, &ApiResponse::error("no test running"));
                }
            }
            _ => handlers::handle_not_found(request),
        }
    }

    fn handle_start_test_http(&self, request: tiny_http::Request) {
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
        let test_config: TestConfig = match serde_json::from_str(&body) {
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

        match self.start_test(test_config) {
            Ok(()) => handlers::respond_json(request, 200, &ApiResponse::success()),
            Err(msg) => handlers::respond_json(request, 409, &ApiResponse::error(msg)),
        }
    }

    fn handle_get_info_http(&self, request: tiny_http::Request) {
        let info = self.node_info();
        handlers::respond_json(request, 200, &info);
    }

    // --- mTLS mode ----------------------------------------------------------

    fn run_mtls(&self, server: &MtlsServer) {
        let inner = self.inner.clone();
        server.serve(move |req| {
            let method = req.method.as_str();
            let path = req.path.as_str();
            tracing::debug!("{method} {path} (mTLS)");

            match (method, path) {
                ("POST", "/test/start") => Self::handle_start_test_mtls(&inner, &req),
                ("GET", "/info") => {
                    let info = Self::node_info_from(&inner);
                    json_response(200, &info)
                }
                ("GET", "/status") => {
                    let inner = inner.lock().unwrap();
                    let view = handlers::build_status_view(&inner.shared_state);
                    json_response(200, &view)
                }
                ("GET", "/metrics") => {
                    let inner = inner.lock().unwrap();
                    let view = handlers::build_metrics_view(&inner.shared_state);
                    json_response(200, &view)
                }
                ("PUT", "/rate") => Self::handle_command_mtls(&inner, &req.body, |tx, body| {
                    let rate: serde_json::Value =
                        serde_json::from_slice(body).map_err(|e| format!("bad JSON: {e}"))?;
                    let rps = rate["rps"].as_f64().ok_or("missing 'rps' field")?;
                    tx.send(WorkerCommand::UpdateRate(rps))
                        .map_err(|e| format!("send: {e}"))?;
                    Ok(())
                }),
                ("PUT", "/targets") => Self::handle_command_mtls(&inner, &req.body, |tx, body| {
                    #[derive(serde::Deserialize)]
                    struct R {
                        targets: Vec<String>,
                    }
                    let r: R =
                        serde_json::from_slice(body).map_err(|e| format!("bad JSON: {e}"))?;
                    tx.send(WorkerCommand::UpdateTargets(r.targets))
                        .map_err(|e| format!("send: {e}"))?;
                    Ok(())
                }),
                ("PUT", "/headers") => Self::handle_command_mtls(&inner, &req.body, |tx, body| {
                    #[derive(serde::Deserialize)]
                    struct R {
                        headers: Vec<(String, String)>,
                    }
                    let r: R =
                        serde_json::from_slice(body).map_err(|e| format!("bad JSON: {e}"))?;
                    tx.send(WorkerCommand::UpdateHeaders(r.headers))
                        .map_err(|e| format!("send: {e}"))?;
                    Ok(())
                }),
                ("POST", "/stop") => {
                    let inner_guard = inner.lock().unwrap();
                    if let Some(ref tx) = inner_guard.command_tx {
                        let _ = tx.send(WorkerCommand::Stop);
                        json_response(200, &ApiResponse::success())
                    } else {
                        json_response(409, &ApiResponse::error("no test running"))
                    }
                }
                _ => HttpResponse::error(404, r#"{"error":"not found"}"#),
            }
        });
    }

    fn handle_start_test_mtls(inner: &Arc<Mutex<AgentInner>>, req: &HttpRequest) -> HttpResponse {
        let test_config: TestConfig = match serde_json::from_slice(&req.body) {
            Ok(c) => c,
            Err(e) => return json_response(400, &ApiResponse::error(format!("bad JSON: {e}"))),
        };

        match start_test_inner(inner, test_config) {
            Ok(()) => json_response(200, &ApiResponse::success()),
            Err(msg) => json_response(409, &ApiResponse::error(msg)),
        }
    }

    fn handle_command_mtls(
        inner: &Arc<Mutex<AgentInner>>,
        body: &[u8],
        f: impl FnOnce(&flume::Sender<WorkerCommand>, &[u8]) -> Result<(), String>,
    ) -> HttpResponse {
        let inner_guard = inner.lock().unwrap();
        if let Some(ref tx) = inner_guard.command_tx {
            match f(tx, body) {
                Ok(()) => json_response(200, &ApiResponse::success()),
                Err(e) => json_response(400, &ApiResponse::error(e)),
            }
        } else {
            json_response(409, &ApiResponse::error("no test running"))
        }
    }

    // --- Shared logic -------------------------------------------------------

    /// Start a test (shared between HTTP and mTLS modes).
    fn start_test(&self, test_config: TestConfig) -> Result<(), String> {
        start_test_inner(&self.inner, test_config)
    }

    fn node_info(&self) -> NodeInfo {
        Self::node_info_from(&self.inner)
    }

    fn node_info_from(inner: &Arc<Mutex<AgentInner>>) -> NodeInfo {
        let inner = inner.lock().unwrap();
        NodeInfo {
            id: inner.node_id.clone(),
            addr: inner.listen_addr.clone(),
            cores: inner.cores,
            state: inner.state,
        }
    }
}

fn make_inner(port: u16, addr: &str, cores: usize) -> Arc<Mutex<AgentInner>> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    Arc::new(Mutex::new(AgentInner {
        node_id: NodeId(format!("{hostname}:{port}")),
        listen_addr: addr.to_string(),
        cores,
        state: NodeState::Idle,
        shared_state: SharedState::new(),
        command_tx: None,
        last_result: None,
    }))
}

/// Start a test on a background thread (shared between HTTP and mTLS modes).
fn start_test_inner(inner: &Arc<Mutex<AgentInner>>, test_config: TestConfig) -> Result<(), String> {
    let (cmd_rx, shared_state) = {
        let mut agent = inner.lock().unwrap();
        if agent.state == NodeState::Running {
            return Err("test already running".into());
        }

        let (cmd_tx, cmd_rx) = flume::unbounded();
        agent.state = NodeState::Running;
        agent.shared_state = SharedState::new();
        agent.command_tx = Some(cmd_tx);
        agent.last_result = None;

        (cmd_rx, agent.shared_state.clone())
    };

    // Build plugin generator factory if config includes a plugin
    let plugin_factory = test_config.plugin.as_ref().and_then(|pc| {
        match build_plugin_factory(pc, &test_config.targets) {
            Ok(factory) => {
                tracing::info!(plugin_type = ?pc.plugin_type, "loaded plugin generator");
                Some(factory)
            }
            Err(e) => {
                tracing::error!("failed to load plugin: {e}");
                None
            }
        }
    });

    let inner = inner.clone();
    let request_timeout = test_config.connections.request_timeout;

    std::thread::Builder::new()
        .name("netanvil-agent-test".into())
        .spawn(move || {
            let progress_state = shared_state.clone();
            let mut builder = TestBuilder::new(test_config, move || {
                HttpExecutor::with_timeout(request_timeout)
            })
            .on_progress(move |update| {
                progress_state.update_from_progress(update);
            })
            .external_commands(cmd_rx);

            if let Some(factory) = plugin_factory {
                builder = builder.generator_factory(factory);
            }

            let result = builder.run();

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
        .map_err(|e| format!("spawn test thread: {e}"))?;

    Ok(())
}

fn json_response(status: u16, value: &impl serde::Serialize) -> HttpResponse {
    let body = serde_json::to_string(value).unwrap_or_else(|_| r#"{"error":"serialize"}"#.into());
    HttpResponse { status, body }
}

/// Build a generator factory from an embedded PluginConfig.
fn build_plugin_factory(
    pc: &netanvil_types::PluginConfig,
    targets: &[String],
) -> Result<netanvil_core::GeneratorFactory, String> {
    match pc.plugin_type {
        PluginType::Hybrid => {
            let script = String::from_utf8(pc.source.clone())
                .map_err(|e| format!("hybrid plugin not UTF-8: {e}"))?;
            let config = netanvil_plugin_luajit::config_from_lua(&script)
                .map_err(|e| format!("hybrid config: {e}"))?;
            Ok(Box::new(move |_core_id| {
                Box::new(netanvil_plugin::HybridGenerator::new(config.clone()))
                    as Box<dyn RequestGenerator>
            }))
        }
        PluginType::Lua => {
            let script = String::from_utf8(pc.source.clone())
                .map_err(|e| format!("lua plugin not UTF-8: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::new(&script, &targets)
                        .expect("LuaJIT init failed"),
                ) as Box<dyn RequestGenerator>
            }))
        }
        PluginType::Wasm => {
            let (engine, module) = netanvil_plugin::compile_wasm_module(&pc.source)
                .map_err(|e| format!("WASM compile: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::new(&engine, &module, &targets)
                        .expect("WASM init failed"),
                ) as Box<dyn RequestGenerator>
            }))
        }
    }
}
