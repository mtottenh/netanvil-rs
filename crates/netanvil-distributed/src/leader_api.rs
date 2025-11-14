//! HTTP API server for the leader daemon.
//!
//! Provides CRUD endpoints for test lifecycle management, live metrics,
//! and agent status. Backed by `TestQueue` for execution and
//! `ResultStore` for persistence.

use std::sync::{Arc, Mutex};

use netanvil_types::NodeInfo;

use crate::test_queue::{CancelResult, TestQueue};
use crate::test_spec::{format_timestamp, TestId, TestSpec};

/// Shared state for the leader API server.
pub struct LeaderApiState {
    pub queue: TestQueue,
    /// Agent discovery for the /agents endpoint.
    pub agents: Arc<Mutex<Vec<NodeInfo>>>,
    /// Prometheus metrics from the latest running test.
    pub leader_metrics: crate::LeaderMetricsState,
}

/// Start the leader API HTTP server. Blocks the calling thread.
pub fn serve(bind_addr: &str, state: Arc<LeaderApiState>) -> std::io::Result<()> {
    let server = tiny_http::Server::http(bind_addr)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e.to_string()))?;

    tracing::info!(addr = bind_addr, "leader API server listening");

    for request in server.incoming_requests() {
        let method = request.method().as_str().to_uppercase();
        let path = request.url().to_string();
        tracing::debug!("{method} {path}");

        handle_request(request, &method, &path, &state);
    }

    Ok(())
}

fn handle_request(
    request: tiny_http::Request,
    method: &str,
    full_path: &str,
    state: &LeaderApiState,
) {
    // Split path from query string for routing.
    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));

    match (method, path) {
        // --- Test CRUD ---
        ("POST", "/tests") => handle_create_test(request, state),
        ("GET", "/tests") => handle_list_tests(request, query, state),
        ("GET", "/health") => respond_json(request, 200, &serde_json::json!({"ok": true})),
        ("GET", "/agents") => handle_list_agents(request, state),
        ("GET", "/metrics/prometheus") => {
            crate::leader_server::handle_prometheus_from_state(
                request,
                &state.leader_metrics,
            );
        }
        _ => {
            // Routes with path parameters: /tests/{id}/*
            if let Some(rest) = path.strip_prefix("/tests/") {
                handle_test_routes(request, method, rest, state);
            } else {
                respond_json(request, 404, &serde_json::json!({"ok": false, "error": "not found"}));
            }
        }
    }
}

fn handle_test_routes(
    request: tiny_http::Request,
    method: &str,
    rest: &str,
    state: &LeaderApiState,
) {
    // Parse: "{id}" or "{id}/result" or "{id}/metrics"
    let (id_str, sub) = match rest.split_once('/') {
        Some((id, sub)) => (id, Some(sub)),
        None => (rest, None),
    };

    let id = TestId(id_str.to_string());

    match (method, sub) {
        ("GET", None) => handle_get_test(request, &id, state),
        ("GET", Some("result")) => handle_get_result(request, &id, state),
        ("GET", Some("metrics")) => handle_get_metrics(request, &id, state),
        ("PUT", Some("rate")) => handle_put_rate(request, &id, state),
        ("PUT", Some("signal")) => handle_put_signal(request, &id, state),
        ("DELETE", None) => handle_cancel_test(request, &id, state),
        _ => {
            respond_json(request, 404, &serde_json::json!({"ok": false, "error": "not found"}));
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn handle_create_test(mut request: tiny_http::Request, state: &LeaderApiState) {
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        respond_json(
            request,
            400,
            &serde_json::json!({"ok": false, "error": format!("read error: {e}")}),
        );
        return;
    }

    let spec: TestSpec = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            respond_json(
                request,
                400,
                &serde_json::json!({"ok": false, "error": format!("invalid JSON: {e}")}),
            );
            return;
        }
    };

    let id = match state.queue.enqueue(spec) {
        Ok(id) => id,
        Err(e) => {
            respond_json(
                request,
                400,
                &serde_json::json!({"ok": false, "error": format!("invalid spec: {e}")}),
            );
            return;
        }
    };
    let info = state.queue.get_info(&id);

    respond_json(
        request,
        200,
        &serde_json::json!({
            "ok": true,
            "test": info,
        }),
    );
}

fn handle_list_tests(request: tiny_http::Request, query: &str, state: &LeaderApiState) {
    let mut tests = state.queue.list();

    // Parse query parameters: ?status=X&limit=N
    if !query.is_empty() {
        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                match key {
                    "status" => {
                        tests.retain(|t| format!("{}", t.status) == value);
                    }
                    "limit" => {
                        if let Ok(n) = value.parse::<usize>() {
                            tests.truncate(n);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    respond_json(request, 200, &serde_json::json!({"tests": tests}));
}

fn handle_get_test(request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    let info = match state.queue.get_info(id) {
        Some(i) => i,
        None => {
            respond_json(request, 404, &serde_json::json!({"ok": false, "error": "test not found"}));
            return;
        }
    };

    let progress = state.queue.get_progress(id);

    // Build response with progress and config if available.
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

    // Include the submitted TestSpec config if the test was created via API.
    if let Some(entry) = state.queue.get_entry(id) {
        resp["config"] = serde_json::to_value(&entry.spec).unwrap_or_default();
    }

    respond_json(request, 200, &resp);
}

fn handle_get_result(request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    let entry = match state.queue.get_entry(id) {
        Some(e) => e,
        None => {
            respond_json(request, 404, &serde_json::json!({"ok": false, "error": "test not found"}));
            return;
        }
    };

    match entry.result {
        Some(result) => {
            respond_json(
                request,
                200,
                &serde_json::json!({
                    "id": entry.info.id,
                    "status": entry.info.status,
                    "started_at": entry.info.started_at,
                    "completed_at": entry.info.completed_at,
                    "result": result,
                }),
            );
        }
        None => {
            respond_json(
                request,
                404,
                &serde_json::json!({"ok": false, "error": "test not yet complete"}),
            );
        }
    }
}

fn handle_get_metrics(request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    match state.queue.get_progress(id) {
        Some(p) => {
            respond_json(
                request,
                200,
                &serde_json::json!({
                    "elapsed_secs": p.elapsed.as_secs_f64(),
                    "remaining_secs": p.remaining.as_secs_f64(),
                    "target_rps": p.target_rps,
                    "total_requests": p.total_requests,
                    "total_errors": p.total_errors,
                    "active_nodes": p.active_nodes,
                }),
            );
        }
        None => {
            respond_json(
                request,
                404,
                &serde_json::json!({"ok": false, "error": "no live metrics (test not running or wrong id)"}),
            );
        }
    }
}

fn handle_put_rate(mut request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        respond_json(request, 400, &serde_json::json!({"ok": false, "error": format!("read error: {e}")}));
        return;
    }
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            respond_json(request, 400, &serde_json::json!({"ok": false, "error": format!("bad JSON: {e}")}));
            return;
        }
    };
    let rps = match parsed.get("rps").and_then(|v| v.as_f64()) {
        Some(r) => r,
        None => {
            respond_json(request, 400, &serde_json::json!({"ok": false, "error": "missing 'rps' field"}));
            return;
        }
    };
    if state.queue.send_rate_override(id, rps) {
        respond_json(request, 200, &serde_json::json!({"ok": true}));
    } else {
        respond_json(request, 409, &serde_json::json!({"ok": false, "error": "test not running"}));
    }
}

fn handle_put_signal(mut request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        respond_json(request, 400, &serde_json::json!({"ok": false, "error": format!("read error: {e}")}));
        return;
    }
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            respond_json(request, 400, &serde_json::json!({"ok": false, "error": format!("bad JSON: {e}")}));
            return;
        }
    };
    let name = match parsed.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            respond_json(request, 400, &serde_json::json!({"ok": false, "error": "missing 'name' field"}));
            return;
        }
    };
    let value = match parsed.get("value").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => {
            respond_json(request, 400, &serde_json::json!({"ok": false, "error": "missing 'value' field"}));
            return;
        }
    };
    if state.queue.send_signal(id, name, value) {
        respond_json(request, 200, &serde_json::json!({"ok": true}));
    } else {
        respond_json(request, 409, &serde_json::json!({"ok": false, "error": "test not running"}));
    }
}

fn handle_cancel_test(request: tiny_http::Request, id: &TestId, state: &LeaderApiState) {
    match state.queue.cancel(id) {
        CancelResult::Cancelled => {
            respond_json(request, 200, &serde_json::json!({"ok": true, "action": "cancelled"}));
        }
        CancelResult::Stopping => {
            respond_json(request, 200, &serde_json::json!({"ok": true, "action": "stopping"}));
        }
        CancelResult::AlreadyDone => {
            respond_json(request, 409, &serde_json::json!({"ok": false, "error": "test already completed"}));
        }
        CancelResult::NotFound => {
            respond_json(request, 404, &serde_json::json!({"ok": false, "error": "test not found"}));
        }
    }
}

fn handle_list_agents(request: tiny_http::Request, state: &LeaderApiState) {
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
    respond_json(request, 200, &serde_json::json!({"agents": enriched}));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn respond_json(request: tiny_http::Request, status: u16, body: &impl serde::Serialize) {
    let json = serde_json::to_string(body).unwrap_or_else(|_| r#"{"ok":false}"#.to_string());
    let response = tiny_http::Response::from_string(json)
        .with_status_code(status)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        );
    let _ = request.respond(response);
}
