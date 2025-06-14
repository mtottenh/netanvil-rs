use netanvil_types::WorkerCommand;

use crate::types::*;

pub fn handle_get_status(request: tiny_http::Request, state: &SharedState) {
    let status = state.get_status();
    respond_json(request, 200, &status);
}

pub fn handle_get_metrics(request: tiny_http::Request, state: &SharedState) {
    match state.get_metrics() {
        Some(metrics) => respond_json(request, 200, &metrics),
        None => respond_json(request, 200, &ApiResponse::error("no metrics yet")),
    }
}

pub fn handle_put_rate(
    request: tiny_http::Request,
    command_tx: &flume::Sender<WorkerCommand>,
) {
    read_json_then_respond::<UpdateRateRequest>(request, |body, req| {
        let _ = command_tx.send(WorkerCommand::UpdateRate(body.rps));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_put_targets(
    request: tiny_http::Request,
    command_tx: &flume::Sender<WorkerCommand>,
) {
    read_json_then_respond::<UpdateTargetsRequest>(request, |body, req| {
        if body.targets.is_empty() {
            respond_json(req, 400, &ApiResponse::error("targets must not be empty"));
            return;
        }
        let _ = command_tx.send(WorkerCommand::UpdateTargets(body.targets));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_put_headers(
    request: tiny_http::Request,
    command_tx: &flume::Sender<WorkerCommand>,
) {
    read_json_then_respond::<UpdateHeadersRequest>(request, |body, req| {
        let _ = command_tx.send(WorkerCommand::UpdateHeaders(body.headers));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_post_stop(
    request: tiny_http::Request,
    command_tx: &flume::Sender<WorkerCommand>,
) {
    let _ = command_tx.send(WorkerCommand::Stop);
    respond_json(request, 200, &ApiResponse::success());
}

pub fn handle_not_found(request: tiny_http::Request) {
    respond_json(request, 404, &ApiResponse::error("not found"));
}

// --- helpers ---

/// Read JSON body, then call the handler with the parsed body and the request
/// (which still needs to be responded to). On parse failure, responds with 400.
fn read_json_then_respond<T: serde::de::DeserializeOwned>(
    mut request: tiny_http::Request,
    handler: impl FnOnce(T, tiny_http::Request),
) {
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        respond_json(request, 400, &ApiResponse::error(format!("read error: {e}")));
        return;
    }
    match serde_json::from_str::<T>(&body) {
        Ok(parsed) => handler(parsed, request),
        Err(e) => respond_json(request, 400, &ApiResponse::error(format!("invalid JSON: {e}"))),
    }
}

fn respond_json(request: tiny_http::Request, status_code: u16, body: &impl serde::Serialize) {
    let json = serde_json::to_string(body).unwrap_or_else(|_| r#"{"ok":false}"#.to_string());
    let response = tiny_http::Response::from_string(json)
        .with_status_code(status_code)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        );
    let _ = request.respond(response);
}
