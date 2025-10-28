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

/// Build a status view for the mTLS handler (no tiny_http dependency).
pub fn build_status_view(state: &SharedState) -> serde_json::Value {
    serde_json::to_value(state.get_status()).unwrap_or(serde_json::json!({}))
}

/// Build a metrics view for the mTLS handler (no tiny_http dependency).
pub fn build_metrics_view(state: &SharedState) -> serde_json::Value {
    match state.get_metrics() {
        Some(metrics) => serde_json::to_value(metrics).unwrap_or(serde_json::json!({})),
        None => serde_json::to_value(ApiResponse::error("no metrics yet"))
            .unwrap_or(serde_json::json!({})),
    }
}

pub fn handle_put_rate(request: tiny_http::Request, command_tx: &flume::Sender<WorkerCommand>) {
    read_json_then_respond::<UpdateRateRequest>(request, |body, req| {
        let _ = command_tx.send(WorkerCommand::UpdateRate(body.rps));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_put_targets(request: tiny_http::Request, command_tx: &flume::Sender<WorkerCommand>) {
    read_json_then_respond::<UpdateTargetsRequest>(request, |body, req| {
        if body.targets.is_empty() {
            respond_json(req, 400, &ApiResponse::error("targets must not be empty"));
            return;
        }
        let _ = command_tx.send(WorkerCommand::UpdateTargets(body.targets));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_put_headers(request: tiny_http::Request, command_tx: &flume::Sender<WorkerCommand>) {
    read_json_then_respond::<UpdateMetadataRequest>(request, |body, req| {
        let _ = command_tx.send(WorkerCommand::UpdateMetadata(body.headers));
        respond_json(req, 200, &ApiResponse::success());
    });
}

pub fn handle_post_stop(request: tiny_http::Request, command_tx: &flume::Sender<WorkerCommand>) {
    let _ = command_tx.send(WorkerCommand::Stop);
    respond_json(request, 200, &ApiResponse::success());
}

pub fn handle_put_signal(request: tiny_http::Request, state: &SharedState) {
    read_json_then_respond::<PushSignalRequest>(request, |body, req| {
        tracing::debug!(name = %body.name, value = body.value, "received pushed signal");
        state.push_signal(body.name, body.value);
        respond_json(req, 200, &ApiResponse::success());
    });
}

/// Build Prometheus text exposition body from shared state.
/// Public so the metrics-only server (plain HTTP alongside mTLS) can reuse it.
pub fn build_prometheus_body(state: &SharedState) -> String {
    match state.get_metrics() {
        Some(m) => {
            let node_id = state.get_node_id();
            // If node_id is set, emit it as a label on every metric.
            let label = match &node_id {
                Some(id) => format!("{{node_id=\"{id}\"}}"),
                None => String::new(),
            };
            // For histogram buckets, we need to merge the label with the le label.
            let bucket_prefix = match &node_id {
                Some(id) => format!("{{node_id=\"{id}\",le="),
                None => "{le=".to_string(),
            };

            let mut out = String::with_capacity(2048);

            // Counters
            out.push_str("# HELP netanvil_requests_total Total requests sent.\n");
            out.push_str("# TYPE netanvil_requests_total counter\n");
            out.push_str(&format!(
                "netanvil_requests_total{label} {}\n",
                m.total_requests
            ));

            out.push_str("# HELP netanvil_errors_total Total errors.\n");
            out.push_str("# TYPE netanvil_errors_total counter\n");
            out.push_str(&format!(
                "netanvil_errors_total{label} {}\n",
                m.total_errors
            ));

            // Throughput counters
            out.push_str("# HELP netanvil_bytes_sent_total Total bytes sent to targets.\n");
            out.push_str("# TYPE netanvil_bytes_sent_total counter\n");
            out.push_str(&format!(
                "netanvil_bytes_sent_total{label} {}\n",
                m.bytes_sent
            ));

            out.push_str(
                "# HELP netanvil_bytes_received_total Total bytes received from targets.\n",
            );
            out.push_str("# TYPE netanvil_bytes_received_total counter\n");
            out.push_str(&format!(
                "netanvil_bytes_received_total{label} {}\n",
                m.bytes_received
            ));

            // Gauges
            out.push_str("# HELP netanvil_request_rate Current requests per second.\n");
            out.push_str("# TYPE netanvil_request_rate gauge\n");
            out.push_str(&format!(
                "netanvil_request_rate{label} {:.1}\n",
                m.current_rps
            ));

            out.push_str("# HELP netanvil_target_rps Target requests per second.\n");
            out.push_str("# TYPE netanvil_target_rps gauge\n");
            out.push_str(&format!("netanvil_target_rps{label} {:.1}\n", m.target_rps));

            out.push_str("# HELP netanvil_error_rate Current error rate (0-1).\n");
            out.push_str("# TYPE netanvil_error_rate gauge\n");
            out.push_str(&format!(
                "netanvil_error_rate{label} {:.6}\n",
                m.error_rate
            ));

            out.push_str("# HELP netanvil_elapsed_seconds Test elapsed time.\n");
            out.push_str("# TYPE netanvil_elapsed_seconds gauge\n");
            out.push_str(&format!(
                "netanvil_elapsed_seconds{label} {:.1}\n",
                m.elapsed_secs
            ));

            // Latency histogram (cumulative buckets, Prometheus native format)
            out.push_str("# HELP netanvil_latency_seconds Request latency distribution.\n");
            out.push_str("# TYPE netanvil_latency_seconds histogram\n");
            for &(le, count) in &m.latency_buckets {
                if le.is_infinite() {
                    out.push_str(&format!(
                        "netanvil_latency_seconds_bucket{bucket_prefix}\"+Inf\"}} {count}\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "netanvil_latency_seconds_bucket{bucket_prefix}\"{le}\"}} {count}\n"
                    ));
                }
            }
            // _count and _sum (sum approximated from percentiles — exact sum
            // would require tracking in the coordinator, but count is exact)
            out.push_str(&format!(
                "netanvil_latency_seconds_count{label} {}\n",
                m.total_requests
            ));
            // Approximate sum from p50 * count (rough, but Prometheus needs it)
            let approx_sum = m.latency_p50_ms / 1000.0 * m.total_requests as f64;
            out.push_str(&format!(
                "netanvil_latency_seconds_sum{label} {:.3}\n",
                approx_sum
            ));

            // Saturation / client health metrics
            let s = &m.saturation;

            out.push_str(
                "# HELP netanvil_backpressure_drops_total Requests dropped (fire channel full).\n",
            );
            out.push_str("# TYPE netanvil_backpressure_drops_total counter\n");
            out.push_str(&format!(
                "netanvil_backpressure_drops_total{label} {}\n",
                s.backpressure_drops
            ));

            out.push_str(
                "# HELP netanvil_scheduling_delay_mean_seconds Mean scheduling delay (intended vs actual).\n",
            );
            out.push_str("# TYPE netanvil_scheduling_delay_mean_seconds gauge\n");
            out.push_str(&format!(
                "netanvil_scheduling_delay_mean_seconds{label} {:.6}\n",
                s.scheduling_delay_mean_ms / 1000.0
            ));

            out.push_str(
                "# HELP netanvil_scheduling_delay_max_seconds Max scheduling delay this window.\n",
            );
            out.push_str("# TYPE netanvil_scheduling_delay_max_seconds gauge\n");
            out.push_str(&format!(
                "netanvil_scheduling_delay_max_seconds{label} {:.6}\n",
                s.scheduling_delay_max_ms / 1000.0
            ));

            out.push_str(
                "# HELP netanvil_rate_achievement Ratio of achieved to target RPS (1.0 = on target).\n",
            );
            out.push_str("# TYPE netanvil_rate_achievement gauge\n");
            out.push_str(&format!(
                "netanvil_rate_achievement{label} {:.4}\n",
                s.rate_achievement
            ));

            out.push_str(
                "# HELP netanvil_client_saturated Whether the client is the bottleneck (1=yes).\n",
            );
            out.push_str("# TYPE netanvil_client_saturated gauge\n");
            let saturated = matches!(
                s.assessment,
                netanvil_types::SaturationAssessment::ClientSaturated
                    | netanvil_types::SaturationAssessment::BothSaturated
            );
            out.push_str(&format!(
                "netanvil_client_saturated{label} {}\n",
                if saturated { 1 } else { 0 }
            ));

            out
        }
        None => "# No metrics available yet\n".to_string(),
    }
}

pub fn handle_get_metrics_prometheus(request: tiny_http::Request, state: &SharedState) {
    let body = build_prometheus_body(state);

    let response = tiny_http::Response::from_string(body)
        .with_status_code(200)
        .with_header(
            tiny_http::Header::from_bytes(
                &b"Content-Type"[..],
                &b"text/plain; version=0.0.4; charset=utf-8"[..],
            )
            .unwrap(),
        );
    let _ = request.respond(response);
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
        respond_json(
            request,
            400,
            &ApiResponse::error(format!("read error: {e}")),
        );
        return;
    }
    match serde_json::from_str::<T>(&body) {
        Ok(parsed) => handler(parsed, request),
        Err(e) => respond_json(
            request,
            400,
            &ApiResponse::error(format!("invalid JSON: {e}")),
        ),
    }
}

pub fn respond_json(request: tiny_http::Request, status_code: u16, body: &impl serde::Serialize) {
    let json = serde_json::to_string(body).unwrap_or_else(|_| r#"{"ok":false}"#.to_string());
    let response = tiny_http::Response::from_string(json)
        .with_status_code(status_code)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        );
    let _ = request.respond(response);
}
