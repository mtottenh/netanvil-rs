//! HTTP API server for the leader daemon.
//!
//! Provides CRUD endpoints for test lifecycle management, live metrics,
//! and agent status. Backed by `TestQueue` for execution and
//! `ResultStore` for persistence.

use std::sync::{Arc, Mutex};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use netanvil_types::NodeInfo;
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

use crate::leader_server;
use crate::test_queue::{CancelResult, TestQueue};
use crate::test_spec::{format_timestamp, TestId, TestInfo, TestSpec, TestStatus};

/// Shared state for the leader API server.
pub struct LeaderApiState {
    pub queue: TestQueue,
    /// Agent discovery for the /agents endpoint.
    pub agents: Arc<Mutex<Vec<NodeInfo>>>,
    /// Prometheus metrics from the latest running test.
    pub leader_metrics: crate::LeaderMetricsState,
}

/// Start the leader API HTTP server. Async — call from a shared tokio runtime.
pub async fn serve(bind_addr: &str, state: Arc<LeaderApiState>) -> std::io::Result<()> {
    let app = leader_router(state);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e))?;
    tracing::info!(addr = bind_addr, "leader API server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| std::io::Error::other(format!("leader API serve: {e}")))
}

#[derive(OpenApi)]
#[openapi(
    components(schemas(TestId, TestStatus, TestInfo)),
    tags((name = "tests", description = "Test lifecycle management"),
         (name = "agents", description = "Agent discovery"),
         (name = "system", description = "Health and metrics"))
)]
pub struct LeaderApiDoc;

fn leader_router(state: Arc<LeaderApiState>) -> Router {
    Router::new()
        .route("/tests", post(create_test).get(list_tests))
        .route("/tests/{id}", get(get_test).delete(cancel_test))
        .route("/tests/{id}/result", get(get_result))
        .route("/tests/{id}/metrics", get(get_metrics))
        .route("/tests/{id}/rate", put(put_rate))
        .route("/tests/{id}/signal", put(put_signal))
        .route("/tests/{id}/hold", put(put_hold).delete(delete_hold))
        .route(
            "/tests/{id}/controller",
            get(get_controller).put(put_controller),
        )
        .route("/agents", get(list_agents))
        .route("/health", get(health))
        .route("/metrics/prometheus", get(prometheus_metrics))
        .merge(Scalar::with_url("/docs", LeaderApiDoc::openapi()))
        .fallback(handle_not_found)
        .with_state(state)
}

async fn handle_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"ok": false, "error": "not found"})),
    )
}

/// Extract JSON body, returning error JSON on parse failure.
fn json_or_error<T: serde::de::DeserializeOwned>(
    result: Result<Json<T>, JsonRejection>,
) -> Result<T, (StatusCode, Json<serde_json::Value>)> {
    match result {
        Ok(Json(v)) => Ok(v),
        Err(rejection) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": format!("invalid JSON: {rejection}")})),
        )),
    }
}

// --- Query params -----------------------------------------------------------

#[derive(Deserialize)]
struct ListTestsQuery {
    status: Option<String>,
    limit: Option<usize>,
}

// --- Handlers ---------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

async fn prometheus_metrics(
    State(state): State<Arc<LeaderApiState>>,
) -> impl IntoResponse {
    leader_server::handle_prometheus(State(state.leader_metrics.clone())).await
}

async fn create_test(
    State(state): State<Arc<LeaderApiState>>,
    body: Result<Json<TestSpec>, JsonRejection>,
) -> impl IntoResponse {
    let spec = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    let id = match state.queue.enqueue(spec) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": format!("invalid spec: {e}")})),
            )
                .into_response()
        }
    };
    let info = state.queue.get_info(&id);

    Json(serde_json::json!({
        "ok": true,
        "test": info,
    }))
    .into_response()
}

async fn list_tests(
    State(state): State<Arc<LeaderApiState>>,
    Query(params): Query<ListTestsQuery>,
) -> Json<serde_json::Value> {
    let mut tests = state.queue.list();

    if let Some(ref status_filter) = params.status {
        tests.retain(|t| format!("{}", t.status) == *status_filter);
    }
    if let Some(n) = params.limit {
        tests.truncate(n);
    }

    Json(serde_json::json!({"tests": tests}))
}

async fn get_test(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    let info = match state.queue.get_info(&id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"ok": false, "error": "test not found"})),
            )
                .into_response()
        }
    };

    let progress = state.queue.get_progress(&id);

    let mut resp = serde_json::to_value(&info).unwrap_or_default();
    if let Some(p) = progress {
        resp["progress"] = serde_json::json!({
            "elapsed_secs": p.elapsed.as_secs_f64(),
            "remaining_secs": p.remaining.as_secs_f64(),
            "target_rps": p.target_rps,
            "total_requests": p.total_requests,
            "total_errors": p.total_errors,
            "active_nodes": p.active_nodes,
        });
    }

    if let Some(entry) = state.queue.get_entry(&id) {
        resp["config"] = serde_json::to_value(&entry.spec).unwrap_or_default();
    }

    Json(resp).into_response()
}

async fn get_result(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    let entry = match state.queue.get_entry(&id) {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"ok": false, "error": "test not found"})),
            )
                .into_response()
        }
    };

    match entry.result {
        Some(result) => Json(serde_json::json!({
            "id": entry.info.id,
            "status": entry.info.status,
            "started_at": entry.info.started_at,
            "completed_at": entry.info.completed_at,
            "result": result,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "test not yet complete"})),
        )
            .into_response(),
    }
}

async fn get_metrics(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    match state.queue.get_progress(&id) {
        Some(p) => Json(serde_json::json!({
            "elapsed_secs": p.elapsed.as_secs_f64(),
            "remaining_secs": p.remaining.as_secs_f64(),
            "target_rps": p.target_rps,
            "total_requests": p.total_requests,
            "total_errors": p.total_errors,
            "active_nodes": p.active_nodes,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "no live metrics (test not running or wrong id)"})),
        )
            .into_response(),
    }
}

async fn put_rate(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> impl IntoResponse {
    let body = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let rps = match body.get("rps").and_then(|v| v.as_f64()) {
        Some(r) => r,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "missing 'rps' field"})),
            )
                .into_response()
        }
    };

    let id = TestId(id);
    tracing::warn!(test_id = %id, rps, "PUT /rate is deprecated, use PUT /hold instead");
    if state.queue.send_hold(&id, rps) {
        Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response()
    }
}

async fn put_hold(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> impl IntoResponse {
    let body = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let rps = match body.get("rps").and_then(|v| v.as_f64()) {
        Some(r) => r,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "missing 'rps' field"})),
            )
                .into_response()
        }
    };

    let id = TestId(id);
    if state.queue.send_hold(&id, rps) {
        Json(serde_json::json!({
            "ok": true,
            "held_rps": rps,
            "controller_paused": true,
        }))
        .into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response()
    }
}

async fn delete_hold(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    if state.queue.send_release(&id) {
        Json(serde_json::json!({"ok": true, "resumed": true})).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response()
    }
}

async fn get_controller(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    match state.queue.get_controller_info(&id) {
        Some(view) => Json(serde_json::to_value(view).unwrap()).into_response(),
        None => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response(),
    }
}

async fn put_controller(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
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

    let id = TestId(id);
    match state.queue.send_controller_update(&id, action, body) {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": e})),
        )
            .into_response(),
        None => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response(),
    }
}

async fn put_signal(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> impl IntoResponse {
    let body = match json_or_error(body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "missing 'name' field"})),
            )
                .into_response()
        }
    };
    let value = match body.get("value").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "missing 'value' field"})),
            )
                .into_response()
        }
    };

    let id = TestId(id);
    if state.queue.send_signal(&id, name, value) {
        Json(serde_json::json!({"ok": true})).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test not running"})),
        )
            .into_response()
    }
}

async fn cancel_test(
    State(state): State<Arc<LeaderApiState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let id = TestId(id);
    match state.queue.cancel(&id) {
        CancelResult::Cancelled => {
            Json(serde_json::json!({"ok": true, "action": "cancelled"})).into_response()
        }
        CancelResult::Stopping => {
            Json(serde_json::json!({"ok": true, "action": "stopping"})).into_response()
        }
        CancelResult::AlreadyDone => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "test already completed"})),
        )
            .into_response(),
        CancelResult::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "test not found"})),
        )
            .into_response(),
    }
}

async fn list_agents(
    State(state): State<Arc<LeaderApiState>>,
) -> Json<serde_json::Value> {
    let agents = state.agents.lock().unwrap().clone();
    let now = format_timestamp(std::time::SystemTime::now());
    let enriched: Vec<serde_json::Value> = agents
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": a.id.0,
                "addr": a.addr,
                "cores": a.cores,
                "state": format!("{:?}", a.state),
                "last_seen": now,
            })
        })
        .collect();
    Json(serde_json::json!({"agents": enriched}))
}
