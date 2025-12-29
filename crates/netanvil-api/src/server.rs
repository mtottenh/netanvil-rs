use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use netanvil_types::{ControllerView, HoldCommand, WorkerCommand};

use crate::handlers;
use crate::types::*;

/// Handle to a running control server. Drop to shut down.
pub struct ControlServerHandle {
    _thread: std::thread::JoinHandle<()>,
}

/// Shared state for control server handlers.
#[derive(Clone)]
pub struct ControlState {
    pub shared_state: SharedState,
    pub command_tx: flume::Sender<WorkerCommand>,
}

/// HTTP control server for a running load test.
///
/// Runs on a background `std::thread` using a single-threaded tokio runtime
/// with axum. Communicates with the coordinator via:
/// - `flume::Sender<WorkerCommand>` for injecting commands
/// - `SharedState` (Arc<Mutex>) for reading live metrics
pub struct ControlServer {
    port: u16,
    shared_state: SharedState,
    command_tx: flume::Sender<WorkerCommand>,
}

impl ControlServer {
    /// Create a new control server that will listen on the given port.
    pub fn new(
        port: u16,
        shared_state: SharedState,
        command_tx: flume::Sender<WorkerCommand>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            port,
            shared_state,
            command_tx,
        })
    }

    /// Spawn the server on a background thread.
    pub fn spawn(self) -> ControlServerHandle {
        let thread = std::thread::Builder::new()
            .name("netanvil-api".into())
            .spawn(move || self.run())
            .expect("spawn API server thread");
        ControlServerHandle { _thread: thread }
    }

    fn run(self) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio current_thread runtime for control API");

        rt.block_on(async move {
            let state = ControlState {
                shared_state: self.shared_state,
                command_tx: self.command_tx,
            };

            let app = control_router(state);

            let addr = format!("0.0.0.0:{}", self.port);
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .expect("bind control API");
            tracing::info!("Control API listening on {addr}");
            axum::serve(listener, app).await.expect("control API serve");
        });
    }
}

/// Build the control API router. Exported so AgentServer can nest it.
pub fn control_router(state: ControlState) -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/metrics", get(get_metrics))
        .route("/metrics/prometheus", get(get_metrics_prometheus))
        .route("/rate", put(put_rate))
        .route("/hold", put(put_hold).delete(delete_hold))
        .route("/controller", get(get_controller).put(put_controller))
        .route("/targets", put(put_targets))
        .route("/headers", put(put_headers))
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

// --- Handlers ---------------------------------------------------------------

#[utoipa::path(get, path = "/status",
    responses((status = 200, body = TestStatus)),
    tag = "control")]
async fn get_status(State(state): State<ControlState>) -> Json<TestStatus> {
    Json(state.shared_state.get_status())
}

#[utoipa::path(get, path = "/metrics",
    responses(
        (status = 200, body = MetricsView),
        (status = 200, body = ApiResponse, description = "No metrics yet"),
    ),
    tag = "control")]
async fn get_metrics(State(state): State<ControlState>) -> impl IntoResponse {
    match state.shared_state.get_metrics() {
        Some(metrics) => Json(serde_json::to_value(metrics).unwrap()).into_response(),
        None => Json(ApiResponse::error("no metrics yet")).into_response(),
    }
}

#[utoipa::path(get, path = "/metrics/prometheus",
    responses((status = 200, description = "Prometheus text exposition format")),
    tag = "control")]
async fn get_metrics_prometheus(State(state): State<ControlState>) -> impl IntoResponse {
    let body = handlers::build_prometheus_body(&state.shared_state);
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

#[utoipa::path(put, path = "/rate",
    request_body = UpdateRateRequest,
    responses((status = 200, body = ApiResponse)),
    tag = "control")]
async fn put_rate(
    State(state): State<ControlState>,
    body: Result<Json<UpdateRateRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    tracing::warn!(rps = body.rps, "PUT /rate is deprecated, use PUT /hold instead");
    let _ = state
        .command_tx
        .send(WorkerCommand::Hold(HoldCommand::Hold(body.rps)));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

#[utoipa::path(put, path = "/hold",
    request_body = UpdateRateRequest,
    responses((status = 200, description = "Hold rate response")),
    tag = "control")]
async fn put_hold(
    State(state): State<ControlState>,
    body: Result<Json<UpdateRateRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    let _ = state
        .command_tx
        .send(WorkerCommand::Hold(HoldCommand::Hold(body.rps)));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(serde_json::json!({
        "ok": true,
        "held_rps": body.rps,
        "controller_paused": true,
    })))
}

#[utoipa::path(delete, path = "/hold",
    responses((status = 200, description = "Release hold response")),
    tag = "control")]
async fn delete_hold(State(state): State<ControlState>) -> Json<serde_json::Value> {
    let _ = state
        .command_tx
        .send(WorkerCommand::Hold(HoldCommand::Release));
    Json(serde_json::json!({"ok": true, "resumed": true}))
}

#[utoipa::path(get, path = "/controller",
    responses(
        (status = 200, description = "Controller state"),
        (status = 500, body = ApiResponse),
    ),
    tag = "control")]
async fn get_controller(State(state): State<ControlState>) -> impl IntoResponse {
    let (response_tx, response_rx) = flume::bounded::<ControllerView>(1);
    let _ = state
        .command_tx
        .send(WorkerCommand::ControllerInfo { response_tx });
    match tokio::time::timeout(Duration::from_secs(10), response_rx.recv_async()).await {
        Ok(Ok(view)) => Json(serde_json::to_value(view).unwrap()).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error("timeout waiting for coordinator")),
        )
            .into_response(),
    }
}

#[utoipa::path(put, path = "/controller",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Controller update response"),
        (status = 400, body = ApiResponse),
        (status = 500, body = ApiResponse),
    ),
    tag = "control")]
async fn put_controller(
    State(state): State<ControlState>,
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
                .into_response();
        }
    };
    let (response_tx, response_rx) = flume::bounded(1);
    let _ = state.command_tx.send(WorkerCommand::ControllerUpdate {
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

#[utoipa::path(put, path = "/targets",
    request_body = UpdateTargetsRequest,
    responses((status = 200, body = ApiResponse), (status = 400, body = ApiResponse)),
    tag = "control")]
async fn put_targets(
    State(state): State<ControlState>,
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
    let _ = state
        .command_tx
        .send(WorkerCommand::UpdateTargets(body.targets));
    Json(ApiResponse::success()).into_response()
}

#[utoipa::path(put, path = "/headers",
    request_body = UpdateMetadataRequest,
    responses((status = 200, body = ApiResponse)),
    tag = "control")]
async fn put_headers(
    State(state): State<ControlState>,
    body: Result<Json<UpdateMetadataRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    let _ = state
        .command_tx
        .send(WorkerCommand::UpdateMetadata(body.headers));
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

#[utoipa::path(put, path = "/signal",
    request_body = PushSignalRequest,
    responses((status = 200, body = ApiResponse)),
    tag = "control")]
async fn put_signal(
    State(state): State<ControlState>,
    body: Result<Json<PushSignalRequest>, JsonRejection>,
) -> impl IntoResponse {
    let body = json_or_error(body)?;
    tracing::debug!(name = %body.name, value = body.value, "received pushed signal");
    state.shared_state.push_signal(body.name, body.value);
    Ok::<_, (StatusCode, Json<ApiResponse>)>(Json(ApiResponse::success()))
}

#[utoipa::path(post, path = "/stop",
    responses((status = 200, body = ApiResponse)),
    tag = "control")]
async fn post_stop(State(state): State<ControlState>) -> Json<ApiResponse> {
    let _ = state.command_tx.send(WorkerCommand::Stop);
    Json(ApiResponse::success())
}
