//! Agent server: a remotely controllable load test node.
//!
//! Wraps the existing control endpoints with lifecycle management.
//! Accepts `POST /test/start` to begin a test, `GET /info` for node info,
//! plus all control endpoints (`GET /metrics`, `PUT /rate`, `POST /stop`).
//!
//! Supports two modes via a unified axum router:
//! - **Plain HTTP** (default): standard TCP listener
//! - **TLS/mTLS**: axum-server with rustls, client certificate verification
//!
//! Lifecycle: idle → POST /test/start → running → (test ends or POST /stop) → idle

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use netanvil_core::{TestBuilder, TestResult};
use netanvil_http::HttpExecutor;
use netanvil_types::{
    ControllerView, HoldCommand, NodeId, NodeInfo, NodeState, PluginType, RequestGenerator,
    TestConfig, TlsConfig, WorkerCommand,
};

use crate::handlers;
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

/// Shared state for agent axum handlers.
#[derive(Clone)]
pub struct AgentState {
    inner: Arc<Mutex<AgentInner>>,
}

/// Agent server: long-lived HTTP server that accepts tests on demand.
///
/// Create with `new()` for plain HTTP or `with_tls()` for TLS/mTLS.
/// When running TLS, an optional plain HTTP metrics-only listener can be
/// started on a separate port so Prometheus can scrape without client certs.
pub struct AgentServer {
    bind_addr: String,
    /// TLS server config (None = plain HTTP).
    tls_config: Option<Arc<rustls::ServerConfig>>,
    /// Optional plain HTTP port for Prometheus metrics when running TLS.
    metrics_port: Option<u16>,
    /// Trusted SANs from leader certificates. When non-empty, the SAN
    /// verification middleware rejects requests from certs not in this list.
    trusted_sans: Vec<String>,
    inner: Arc<Mutex<AgentInner>>,
}

impl AgentServer {
    /// Create a plain HTTP agent (no TLS).
    ///
    /// `bind_addr` is the address to listen on (e.g. "0.0.0.0:9090" or "10.0.0.2:9090").
    /// `node_id` is an optional human-readable identifier for metrics/logging.
    pub fn new(bind_addr: &str, cores: usize, node_id: Option<String>) -> std::io::Result<Self> {
        let inner = make_inner(bind_addr, cores, node_id);

        Ok(Self {
            bind_addr: bind_addr.to_string(),
            tls_config: None,
            metrics_port: None,
            trusted_sans: Vec::new(),
            inner,
        })
    }

    /// Create a TLS/mTLS agent with client certificate verification.
    ///
    /// `bind_addr` is the address to listen on (e.g. "0.0.0.0:9090" or "10.0.0.2:9090").
    /// `node_id` is an optional human-readable identifier for metrics/logging.
    pub fn with_tls(
        bind_addr: &str,
        cores: usize,
        node_id: Option<String>,
        tls: &TlsConfig,
    ) -> std::io::Result<Self> {
        let tls_config = crate::tls::build_server_config(tls)
            .map_err(|e| std::io::Error::other(format!("TLS setup: {e}")))?;

        let inner = make_inner(bind_addr, cores, node_id);

        Ok(Self {
            bind_addr: bind_addr.to_string(),
            tls_config: Some(tls_config),
            metrics_port: None,
            trusted_sans: Vec::new(),
            inner,
        })
    }

    /// Set a plain HTTP port for Prometheus metrics scraping.
    /// Only meaningful when running in TLS mode — in plain HTTP mode,
    /// `/metrics/prometheus` is already available on the main port.
    pub fn set_metrics_port(&mut self, port: u16) {
        self.metrics_port = Some(port);
    }

    /// Set trusted SANs for client certificate verification.
    /// When non-empty, only clients whose certificate SAN matches
    /// one of these values are allowed to make requests.
    /// Only effective when TLS is enabled.
    pub fn set_trusted_sans(&mut self, sans: Vec<String>) {
        self.trusted_sans = sans;
    }

    /// Run the agent server (blocking). Call from the main thread.
    ///
    /// Creates a single tokio `current_thread` runtime for all control-plane
    /// HTTP serving. When a metrics-only port is configured, both the main
    /// server and the metrics server run cooperatively on the same runtime.
    pub fn run(&self) {
        // Set node_id on shared state so Prometheus metrics include it.
        {
            let agent = self.inner.lock().unwrap();
            agent.shared_state.set_node_id(agent.node_id.0.clone());
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current_thread runtime for agent");

        let state = AgentState {
            inner: self.inner.clone(),
        };
        let router = agent_router(state.clone());
        let bind_addr = self.bind_addr.clone();
        let tls_config = self.tls_config.clone();
        let metrics_port = self.metrics_port;
        let trusted_sans = self.trusted_sans.clone();

        rt.block_on(async move {
            // Build the main server future.
            let main_server = async {
                if let Some(tls_config) = tls_config {
                    // When TLS is enabled, apply SAN verification if trusted SANs are configured.
                    let router = if trusted_sans.is_empty() {
                        tracing::info!("SAN verification disabled (no --trusted-san configured)");
                        router
                    } else {
                        tracing::info!(
                            trusted_sans = ?trusted_sans,
                            "SAN verification enabled"
                        );
                        router.layer(crate::identity::SanVerifierLayer::new(trusted_sans))
                    };

                    let rustls_config =
                        axum_server::tls_rustls::RustlsConfig::from_config(tls_config);
                    let acceptor =
                        crate::identity::CertExtractingAcceptor::new(rustls_config);
                    tracing::info!("agent listening on {bind_addr} (TLS with cert extraction)");
                    axum_server::bind(bind_addr.parse().expect("parse bind addr"))
                        .acceptor(acceptor)
                        .serve(router.into_make_service())
                        .await
                        .expect("agent TLS serve");
                } else {
                    let listener = tokio::net::TcpListener::bind(&bind_addr)
                        .await
                        .expect("bind agent");
                    tracing::info!("agent listening on {bind_addr} (plain HTTP)");
                    axum::serve(listener, router).await.expect("agent serve");
                }
            };

            // Optionally serve metrics on a separate plain HTTP port
            // (for Prometheus to scrape without TLS client certs).
            if let Some(port) = metrics_port {
                let metrics_app = Router::new()
                    .route("/metrics/prometheus", get(get_metrics_prometheus))
                    .route("/metrics", get(get_metrics))
                    .with_state(state);
                let metrics_addr = format!("0.0.0.0:{port}");
                let metrics_listener = tokio::net::TcpListener::bind(&metrics_addr)
                    .await
                    .expect("bind metrics server");
                tracing::info!(port, "metrics-only server listening (plain HTTP)");
                let metrics_server = axum::serve(metrics_listener, metrics_app);

                tokio::select! {
                    _ = main_server => {},
                    _ = metrics_server => {},
                }
            } else {
                main_server.await;
            }
        });
    }
}

/// Build the agent API router.
fn agent_router(state: AgentState) -> Router {
    Router::new()
        .route("/test/start", post(post_test_start))
        .route("/info", get(get_info))
        .route("/status", get(get_status))
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_metrics_prometheus))
        .route("/rate", put(put_rate))
        .route("/hold", put(put_hold).delete(delete_hold))
        .route("/controller", get(get_controller).put(put_controller))
        .route("/targets", put(put_targets))
        .route("/headers", put(put_headers))
        .route("/test/result", get(get_test_result))
        .route("/signal", put(put_signal))
        .route("/stop", post(post_stop))
        .fallback(handle_not_found)
        .with_state(state)
}

async fn handle_not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(ApiResponse::error("not found")))
}

/// Extract JSON body, returning our ApiResponse format on parse failure.
fn json_or_error<T: serde::de::DeserializeOwned>(
    result: Result<Json<T>, JsonRejection>,
) -> Result<T, (StatusCode, Json<ApiResponse>)> {
    match result {
        Ok(Json(v)) => Ok(v),
        Err(rejection) => Err((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error(format!("invalid JSON: {rejection}"))),
        )),
    }
}

/// Helper: get command_tx from inner, or 409 if no test running.
fn require_command_tx(
    inner: &Mutex<AgentInner>,
) -> Result<flume::Sender<WorkerCommand>, (StatusCode, Json<ApiResponse>)> {
    let guard = inner.lock().unwrap();
    guard
        .command_tx
        .clone()
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                Json(ApiResponse::error("no test running")),
            )
        })
}

// --- Handlers ---------------------------------------------------------------

async fn get_status(State(state): State<AgentState>) -> Json<TestStatus> {
    let inner = state.inner.lock().unwrap();
    Json(inner.shared_state.get_status())
}

async fn get_metrics(State(state): State<AgentState>) -> impl IntoResponse {
    let inner = state.inner.lock().unwrap();
    match inner.shared_state.get_metrics() {
        Some(metrics) => Json(serde_json::to_value(metrics).unwrap()).into_response(),
        None => Json(ApiResponse::error("no metrics yet")).into_response(),
    }
}

async fn get_metrics_prometheus(State(state): State<AgentState>) -> impl IntoResponse {
    let inner = state.inner.lock().unwrap();
    let body = handlers::build_prometheus_body(&inner.shared_state);
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn get_info(State(state): State<AgentState>) -> Json<NodeInfo> {
    let inner = state.inner.lock().unwrap();
    Json(NodeInfo {
        id: inner.node_id.clone(),
        addr: inner.listen_addr.clone(),
        cores: inner.cores,
        state: inner.state,
    })
}

async fn post_test_start(
    State(state): State<AgentState>,
    body: Result<Json<TestConfig>, JsonRejection>,
) -> impl IntoResponse {
    let config = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    // start_test_inner spawns the test thread and waits for readiness.
    // The readiness wait is async to avoid blocking the tokio runtime.
    match start_test_async(&state.inner, config).await {
        Ok(()) => Json(ApiResponse::success()).into_response(),
        Err(msg) => (StatusCode::CONFLICT, Json(ApiResponse::error(msg))).into_response(),
    }
}

async fn get_test_result(State(state): State<AgentState>) -> impl IntoResponse {
    let inner = state.inner.lock().unwrap();
    match &inner.last_result {
        Some(r) => Json(serde_json::to_value(r).unwrap()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error("no test result available")),
        )
            .into_response(),
    }
}

async fn put_rate(
    State(state): State<AgentState>,
    body: Result<Json<UpdateRateRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    let tx = require_command_tx(&state.inner)?;
    tracing::warn!(rps = body.rps, "PUT /rate is deprecated, use PUT /hold instead");
    let _ = tx.send(WorkerCommand::Hold(HoldCommand::Hold(body.rps)));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

async fn put_hold(
    State(state): State<AgentState>,
    body: Result<Json<UpdateRateRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    let tx = require_command_tx(&state.inner)?;
    let _ = tx.send(WorkerCommand::Hold(HoldCommand::Hold(body.rps)));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(serde_json::json!({
        "ok": true,
        "held_rps": body.rps,
        "controller_paused": true,
    })))
}

async fn delete_hold(State(state): State<AgentState>) -> impl IntoResponse {
    let tx = require_command_tx(&state.inner)?;
    let _ = tx.send(WorkerCommand::Hold(HoldCommand::Release));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(
        Json(serde_json::json!({"ok": true, "resumed": true})),
    )
}

async fn get_controller(State(state): State<AgentState>) -> impl IntoResponse {
    let tx = match require_command_tx(&state.inner) {
        Ok(tx) => tx,
        Err(e) => return e.into_response(),
    };
    let (response_tx, response_rx) = flume::bounded::<ControllerView>(1);
    let _ = tx.send(WorkerCommand::ControllerInfo { response_tx });
    match tokio::time::timeout(Duration::from_secs(10), response_rx.recv_async()).await {
        Ok(Ok(view)) => Json(serde_json::to_value(view).unwrap()).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error("timeout waiting for coordinator")),
        )
            .into_response(),
    }
}

async fn put_controller(
    State(state): State<AgentState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> impl IntoResponse {
    let body = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let action = match body.get("action").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "missing 'action' field"})),
            )
                .into_response()
        }
    };
    let tx = match require_command_tx(&state.inner) {
        Ok(tx) => tx,
        Err(e) => return e.into_response(),
    };
    let (response_tx, response_rx) = flume::bounded(1);
    let _ = tx.send(WorkerCommand::ControllerUpdate {
        action,
        params: body,
        response_tx,
    });
    match tokio::time::timeout(Duration::from_secs(10), response_rx.recv_async()).await {
        Ok(Ok(Ok(response))) => Json(response).into_response(),
        Ok(Ok(Err(e))) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": e})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error("timeout waiting for coordinator")),
        )
            .into_response(),
    }
}

async fn put_targets(
    State(state): State<AgentState>,
    body: Result<Json<UpdateTargetsRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    if body.targets.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::error("targets must not be empty")),
        )
            .into_response();
    }
    let tx = match require_command_tx(&state.inner) {
        Ok(tx) => tx,
        Err(e) => return e.into_response(),
    };
    let _ = tx.send(WorkerCommand::UpdateTargets(body.targets));
    Json(ApiResponse::success()).into_response()
}

async fn put_headers(
    State(state): State<AgentState>,
    body: Result<Json<UpdateMetadataRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    let tx = require_command_tx(&state.inner)?;
    let _ = tx.send(WorkerCommand::UpdateMetadata(body.headers));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

async fn put_signal(
    State(state): State<AgentState>,
    body: Result<Json<PushSignalRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    tracing::debug!(name = %body.name, value = body.value, "received pushed signal");
    let inner = state.inner.lock().unwrap();
    inner.shared_state.push_signal(body.name, body.value);
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

async fn post_stop(State(state): State<AgentState>) -> impl IntoResponse {
    let tx = require_command_tx(&state.inner)?;
    let _ = tx.send(WorkerCommand::Stop);
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

// --- Shared logic -----------------------------------------------------------

fn make_inner(bind_addr: &str, cores: usize, node_id: Option<String>) -> Arc<Mutex<AgentInner>> {
    let node_id = node_id.unwrap_or_else(|| {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".into());
        let port = bind_addr.rsplit(':').next().unwrap_or("9090");
        format!("{hostname}:{port}")
    });

    Arc::new(Mutex::new(AgentInner {
        node_id: NodeId(node_id),
        listen_addr: bind_addr.to_string(),
        cores,
        state: NodeState::Idle,
        shared_state: SharedState::new(),
        command_tx: None,
        last_result: None,
    }))
}

/// Start a test with async readiness waiting (for use in axum handlers).
///
/// Spawns the test on a background std::thread and waits asynchronously
/// for the coordinator to signal readiness.
async fn start_test_async(
    inner: &Arc<Mutex<AgentInner>>,
    test_config: TestConfig,
) -> Result<(), String> {
    let ready_rx = start_test_spawn(inner, test_config)?;

    // Wait for readiness asynchronously (does not block the tokio runtime).
    match tokio::time::timeout(Duration::from_secs(30), ready_rx.recv_async()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => {
            let mut agent = inner.lock().unwrap();
            agent.state = NodeState::Idle;
            agent.command_tx = None;
            Err("coordinator startup failed".into())
        }
        Err(_) => {
            let mut agent = inner.lock().unwrap();
            agent.state = NodeState::Idle;
            agent.command_tx = None;
            Err("coordinator startup timed out".into())
        }
    }
}

/// Spawn the test thread and return the readiness receiver.
/// Separated from the readiness wait so the wait can be async.
fn start_test_spawn(
    inner: &Arc<Mutex<AgentInner>>,
    test_config: TestConfig,
) -> Result<flume::Receiver<()>, String> {
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

    let (ready_tx, ready_rx) = flume::bounded::<()>(1);

    let plugin_config = test_config.plugin.clone();
    let inner_clone = inner.clone();
    let request_timeout = test_config.connections.request_timeout;
    let protocol = test_config.protocol.clone();

    std::thread::Builder::new()
        .name("netanvil-agent-test".into())
        .spawn(move || {
            let progress_state = shared_state.clone();

            let result = match protocol {
                Some(netanvil_types::ProtocolConfig::Tcp {
                    mode,
                    payload_hex,
                    framing,
                    request_size,
                    response_size,
                }) => run_tcp_test(
                    test_config,
                    request_timeout,
                    cmd_rx,
                    ready_tx,
                    progress_state,
                    &mode,
                    &payload_hex,
                    &framing,
                    request_size,
                    response_size,
                    plugin_config.as_ref(),
                ),
                Some(netanvil_types::ProtocolConfig::Udp {
                    payload_hex,
                    expect_response,
                }) => run_udp_test(
                    test_config,
                    request_timeout,
                    cmd_rx,
                    ready_tx,
                    progress_state,
                    &payload_hex,
                    expect_response,
                    plugin_config.as_ref(),
                ),
                Some(netanvil_types::ProtocolConfig::Dns {
                    domains,
                    query_type,
                    recursion,
                }) => run_dns_test(
                    test_config,
                    request_timeout,
                    cmd_rx,
                    ready_tx,
                    progress_state,
                    &domains,
                    &query_type,
                    recursion,
                    plugin_config.as_ref(),
                ),
                Some(netanvil_types::ProtocolConfig::Redis {
                    password,
                    db,
                    command,
                    args,
                }) => run_redis_test(
                    test_config,
                    request_timeout,
                    cmd_rx,
                    ready_tx,
                    progress_state,
                    password.as_deref(),
                    db,
                    &command,
                    &args,
                    plugin_config.as_ref(),
                ),
                None => {
                    // Default HTTP path
                    let plugin_factory = plugin_config.as_ref().and_then(|pc| {
                        match build_http_plugin_factory(pc, &test_config.targets) {
                            Ok(factory) => {
                                tracing::info!(
                                    plugin_type = ?pc.plugin_type,
                                    "loaded HTTP plugin generator"
                                );
                                Some(factory)
                            }
                            Err(e) => {
                                tracing::error!("failed to load plugin: {e}");
                                None
                            }
                        }
                    });
                    run_http_test(
                        test_config,
                        request_timeout,
                        cmd_rx,
                        ready_tx,
                        progress_state,
                        plugin_factory,
                    )
                }
            };

            let mut agent = inner_clone.lock().unwrap();
            agent.state = NodeState::Idle;
            agent.command_tx = None;
            agent.shared_state.reset_metrics();
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

    Ok(ready_rx)
}

/// Run an HTTP test (the original path).
fn run_http_test(
    config: TestConfig,
    request_timeout: std::time::Duration,
    cmd_rx: flume::Receiver<WorkerCommand>,
    ready_tx: flume::Sender<()>,
    progress_state: SharedState,
    plugin_factory: Option<netanvil_core::GeneratorFactory>,
) -> netanvil_types::Result<TestResult> {
    let tls_client = config.tls_client.clone();
    let bandwidth_bps = config.bandwidth_limit_bps;
    let http_version = config.http_version;
    let make_executor = move || -> HttpExecutor {
        match &tls_client {
            Some(tls_config) => HttpExecutor::with_tls_and_version(
                tls_config,
                bandwidth_bps,
                request_timeout,
                http_version,
            )
            .expect("TLS configuration error"),
            None => HttpExecutor::with_http_version(http_version, bandwidth_bps, request_timeout),
        }
    };
    let mut builder = TestBuilder::new(config, make_executor)
        .on_progress(move |update| {
            progress_state.update_from_progress(update);
        })
        .external_commands(cmd_rx)
        .ready_signal(ready_tx);

    if let Some(factory) = plugin_factory {
        builder = builder.generator_factory(factory);
    }

    builder.run()
}

/// Run a TCP test with connection pooling.
#[allow(clippy::too_many_arguments)]
fn run_tcp_test(
    config: TestConfig,
    timeout: std::time::Duration,
    cmd_rx: flume::Receiver<WorkerCommand>,
    ready_tx: flume::Sender<()>,
    progress_state: SharedState,
    mode_str: &str,
    payload_hex: &str,
    framing_str: &str,
    request_size: netanvil_types::ValueDistribution<u16>,
    response_size: netanvil_types::ValueDistribution<u32>,
    plugin: Option<&netanvil_types::PluginConfig>,
) -> netanvil_types::Result<TestResult> {
    use netanvil_tcp::*;

    let max_conns = config.connections.max_connections_per_core;
    let policy = config.connections.connection_policy.clone();

    let gen_factory: netanvil_core::GenericGeneratorFactory<TcpRequestSpec> = if let Some(factory) =
        plugin.and_then(|pc| build_tcp_plugin_factory(pc, &config.targets))
    {
        factory
    } else {
        let payload = decode_hex_bytes(payload_hex);
        let mode = match mode_str {
            "echo" => TcpTestMode::Echo,
            "rr" => TcpTestMode::RR,
            "sink" | "stream" => TcpTestMode::Sink,
            "source" | "maerts" => TcpTestMode::Source,
            "bidir" => TcpTestMode::Bidir,
            _ => TcpTestMode::Echo,
        };
        let framing = if framing_str == "raw" || framing_str.is_empty() {
            TcpFraming::Raw
        } else if let Some(rest) = framing_str.strip_prefix("delimiter:") {
            TcpFraming::Delimiter(rest.replace("\\r", "\r").replace("\\n", "\n").into_bytes())
        } else if framing_str == "delimiter" {
            TcpFraming::Delimiter(b"\r\n".to_vec())
        } else if let Some(rest) = framing_str.strip_prefix("length-prefix:") {
            TcpFraming::LengthPrefixed {
                width: rest.parse().unwrap_or(4),
            }
        } else if let Some(rest) = framing_str.strip_prefix("fixed:") {
            TcpFraming::FixedSize(rest.parse().unwrap_or(1024))
        } else {
            TcpFraming::Raw
        };

        let targets: Vec<std::net::SocketAddr> = config
            .targets
            .iter()
            .filter_map(|t| {
                t.strip_prefix("tcp://")
                    .and_then(|a| a.parse().ok())
                    .or_else(|| t.parse().ok())
            })
            .collect();

        let expect_response = mode == TcpTestMode::Echo || mode == TcpTestMode::RR;

        Box::new(move |_core_id| {
            Box::new(
                SimpleTcpGenerator::new(
                    targets.clone(),
                    payload.clone(),
                    framing.clone(),
                    expect_response,
                )
                .with_mode(mode)
                .with_request_size_dist(request_size.clone())
                .with_response_size_dist(response_size.clone()),
            )
        })
    };

    let trans_factory: netanvil_core::GenericTransformerFactory<TcpRequestSpec> =
        Box::new(|_| Box::new(TcpNoopTransformer));

    netanvil_core::GenericTestBuilder::new(
        config,
        move || TcpExecutor::with_pool(timeout, max_conns, policy.clone()),
        gen_factory,
        trans_factory,
    )
    .on_progress(move |update| {
        progress_state.update_from_progress(update);
    })
    .external_commands(cmd_rx)
    .ready_signal(ready_tx)
    .run()
}

/// Run a UDP test.
fn run_udp_test(
    config: TestConfig,
    timeout: std::time::Duration,
    cmd_rx: flume::Receiver<WorkerCommand>,
    ready_tx: flume::Sender<()>,
    progress_state: SharedState,
    payload_hex: &str,
    expect_response: bool,
    plugin: Option<&netanvil_types::PluginConfig>,
) -> netanvil_types::Result<TestResult> {
    use netanvil_udp::*;

    let gen_factory: netanvil_core::GenericGeneratorFactory<UdpRequestSpec> = if let Some(factory) =
        plugin.and_then(|pc| build_udp_plugin_factory(pc, &config.targets))
    {
        factory
    } else {
        let payload = decode_hex_bytes(payload_hex);
        let targets: Vec<std::net::SocketAddr> = config
            .targets
            .iter()
            .filter_map(|t| {
                t.strip_prefix("udp://")
                    .and_then(|a| a.parse().ok())
                    .or_else(|| t.parse().ok())
            })
            .collect();

        Box::new(move |_core_id| {
            Box::new(SimpleUdpGenerator::new(
                targets.clone(),
                payload.clone(),
                expect_response,
            ))
        })
    };

    let trans_factory: netanvil_core::GenericTransformerFactory<UdpRequestSpec> =
        Box::new(|_| Box::new(UdpNoopTransformer));

    netanvil_core::GenericTestBuilder::new(
        config,
        move || UdpExecutor::with_timeout(timeout),
        gen_factory,
        trans_factory,
    )
    .on_progress(move |update| {
        progress_state.update_from_progress(update);
    })
    .external_commands(cmd_rx)
    .ready_signal(ready_tx)
    .run()
}

fn run_dns_test(
    config: TestConfig,
    timeout: std::time::Duration,
    cmd_rx: flume::Receiver<WorkerCommand>,
    ready_tx: flume::Sender<()>,
    progress_state: SharedState,
    domains_csv: &str,
    query_type_str: &str,
    recursion: bool,
    plugin: Option<&netanvil_types::PluginConfig>,
) -> netanvil_types::Result<TestResult> {
    use netanvil_dns::*;

    let gen_factory: netanvil_core::GenericGeneratorFactory<DnsRequestSpec> = if let Some(factory) =
        plugin.and_then(|pc| build_dns_plugin_factory(pc, &config.targets))
    {
        factory
    } else {
        let domains: Vec<String> = domains_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        let query_type = DnsQueryType::from_str_name(query_type_str).unwrap_or(DnsQueryType::A);

        let targets: Vec<std::net::SocketAddr> = config
            .targets
            .iter()
            .filter_map(|t| {
                t.strip_prefix("dns://")
                    .and_then(|a| a.parse().ok())
                    .or_else(|| t.parse().ok())
            })
            .collect();

        Box::new(move |_core_id| {
            Box::new(SimpleDnsGenerator::new(
                targets.clone(),
                domains.clone(),
                query_type,
                recursion,
            ))
        })
    };

    let trans_factory: netanvil_core::GenericTransformerFactory<DnsRequestSpec> =
        Box::new(|_| Box::new(DnsNoopTransformer));

    netanvil_core::GenericTestBuilder::new(
        config,
        move || DnsExecutor::with_timeout(timeout),
        gen_factory,
        trans_factory,
    )
    .on_progress(move |update| {
        progress_state.update_from_progress(update);
    })
    .external_commands(cmd_rx)
    .ready_signal(ready_tx)
    .run()
}

/// Decode hex string to bytes (tolerant of empty strings).
fn decode_hex_bytes(hex: &str) -> Vec<u8> {
    if hex.is_empty() {
        return Vec::new();
    }
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| {
            if i + 2 <= hex.len() {
                u8::from_str_radix(&hex[i..i + 2], 16).ok()
            } else {
                None
            }
        })
        .collect()
}

/// Build an HTTP generator factory from an embedded PluginConfig.
fn build_http_plugin_factory(
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
                    as Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
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
                ) as Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
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
                ) as Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }))
        }
        PluginType::Js => {
            #[cfg(feature = "v8")]
            {
                let script = String::from_utf8(pc.source.clone())
                    .map_err(|e| format!("JS plugin not UTF-8: {e}"))?;
                let targets = targets.to_vec();
                Ok(Box::new(move |_core_id| {
                    Box::new(
                        netanvil_plugin_v8::V8Generator::new(&script, &targets)
                            .expect("V8 init failed"),
                    )
                        as Box<dyn RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
                }))
            }
            #[cfg(not(feature = "v8"))]
            {
                Err("V8/JS plugin support requires the 'v8' feature flag. \
                     Rebuild with: cargo build --features v8"
                    .into())
            }
        }
    }
}

/// Build a TCP generator factory from an embedded PluginConfig.
fn build_tcp_plugin_factory(
    pc: &netanvil_types::PluginConfig,
    targets: &[String],
) -> Option<netanvil_core::GenericGeneratorFactory<netanvil_types::TcpRequestSpec>> {
    if pc.plugin_type == PluginType::Hybrid {
        tracing::warn!("hybrid plugins are HTTP-only; ignoring for TCP");
        return None;
    }
    match pc.plugin_type {
        PluginType::Lua => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::TcpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT init failed"),
                )
            }))
        }
        PluginType::Wasm => {
            let (engine, module) = netanvil_plugin::compile_wasm_module(&pc.source).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::TcpRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM init failed"),
                )
            }))
        }
        #[cfg(feature = "v8")]
        PluginType::Js => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_v8::V8Generator::<netanvil_types::TcpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("V8 init failed"),
                )
            }))
        }
        _ => None,
    }
}

/// Build a UDP generator factory from an embedded PluginConfig.
fn build_udp_plugin_factory(
    pc: &netanvil_types::PluginConfig,
    targets: &[String],
) -> Option<netanvil_core::GenericGeneratorFactory<netanvil_types::UdpRequestSpec>> {
    if pc.plugin_type == PluginType::Hybrid {
        tracing::warn!("hybrid plugins are HTTP-only; ignoring for UDP");
        return None;
    }
    match pc.plugin_type {
        PluginType::Lua => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::UdpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT init failed"),
                )
            }))
        }
        PluginType::Wasm => {
            let (engine, module) = netanvil_plugin::compile_wasm_module(&pc.source).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::UdpRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM init failed"),
                )
            }))
        }
        #[cfg(feature = "v8")]
        PluginType::Js => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_v8::V8Generator::<netanvil_types::UdpRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("V8 init failed"),
                )
            }))
        }
        _ => None,
    }
}

/// Build a DNS generator factory from an embedded PluginConfig.
fn build_dns_plugin_factory(
    pc: &netanvil_types::PluginConfig,
    targets: &[String],
) -> Option<netanvil_core::GenericGeneratorFactory<netanvil_types::DnsRequestSpec>> {
    if pc.plugin_type == PluginType::Hybrid {
        tracing::warn!("hybrid plugins are HTTP-only; ignoring for DNS");
        return None;
    }
    match pc.plugin_type {
        PluginType::Lua => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::DnsRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT init failed"),
                )
            }))
        }
        PluginType::Wasm => {
            let (engine, module) = netanvil_plugin::compile_wasm_module(&pc.source).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::DnsRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM init failed"),
                )
            }))
        }
        #[cfg(feature = "v8")]
        PluginType::Js => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_v8::V8Generator::<netanvil_types::DnsRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("V8 init failed"),
                )
            }))
        }
        _ => None,
    }
}

/// Run a Redis test.
#[allow(clippy::too_many_arguments)]
fn run_redis_test(
    config: TestConfig,
    timeout: std::time::Duration,
    cmd_rx: flume::Receiver<WorkerCommand>,
    ready_tx: flume::Sender<()>,
    progress_state: SharedState,
    password: Option<&str>,
    db: Option<u16>,
    command: &str,
    args: &[String],
    plugin: Option<&netanvil_types::PluginConfig>,
) -> netanvil_types::Result<TestResult> {
    use netanvil_redis::*;

    let targets: Vec<std::net::SocketAddr> = config
        .targets
        .iter()
        .filter_map(|t| {
            t.strip_prefix("redis://")
                .and_then(|a| a.parse().ok())
                .or_else(|| t.parse().ok())
        })
        .collect();

    if targets.is_empty() {
        return Err(netanvil_types::NetAnvilError::Other(
            "no valid Redis targets".into(),
        ));
    }

    let gen_factory: netanvil_core::GenericGeneratorFactory<RedisRequestSpec> =
        if let Some(factory) =
            plugin.and_then(|pc| build_redis_plugin_factory(pc, &config.targets))
        {
            factory
        } else {
            let command = command.to_string();
            let args = args.to_vec();
            Box::new(move |_core_id| {
                Box::new(SimpleRedisGenerator::new(
                    targets.clone(),
                    command.clone(),
                    args.clone(),
                ))
            })
        };

    let trans_factory: netanvil_core::GenericTransformerFactory<RedisRequestSpec> =
        Box::new(|_| Box::new(RedisNoopTransformer));

    let password = password.map(|s| s.to_string());

    netanvil_core::GenericTestBuilder::new(
        config,
        move || RedisExecutor::with_auth(timeout, password.clone(), db),
        gen_factory,
        trans_factory,
    )
    .on_progress(move |update| {
        progress_state.update_from_progress(update);
    })
    .external_commands(cmd_rx)
    .ready_signal(ready_tx)
    .run()
}

/// Build a Redis generator factory from an embedded PluginConfig.
fn build_redis_plugin_factory(
    pc: &netanvil_types::PluginConfig,
    targets: &[String],
) -> Option<netanvil_core::GenericGeneratorFactory<netanvil_types::RedisRequestSpec>> {
    if pc.plugin_type == PluginType::Hybrid {
        tracing::warn!("hybrid plugins are HTTP-only; ignoring for Redis");
        return None;
    }
    match pc.plugin_type {
        PluginType::Lua => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::<netanvil_types::RedisRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("LuaJIT init failed"),
                )
            }))
        }
        PluginType::Wasm => {
            let (engine, module) = netanvil_plugin::compile_wasm_module(&pc.source).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::<netanvil_types::RedisRequestSpec>::new(
                        &engine, &module, &targets,
                    )
                    .expect("WASM init failed"),
                )
            }))
        }
        #[cfg(feature = "v8")]
        PluginType::Js => {
            let script = String::from_utf8(pc.source.clone()).ok()?;
            let targets = targets.to_vec();
            Some(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_v8::V8Generator::<netanvil_types::RedisRequestSpec>::new(
                        &script, &targets,
                    )
                    .expect("V8 init failed"),
                )
            }))
        }
        _ => None,
    }
}
