//! Lightweight HTTP server for the distributed leader node.
//!
//! Exposes `/metrics/prometheus` with aggregated metrics from all agents,
//! giving Prometheus a single scrape target instead of N individual agents.

use std::sync::{Arc, Mutex};
use std::thread;

use crate::coordinator::DistributedProgressUpdate;

/// Shared state updated by the coordinator's progress callback.
#[derive(Clone)]
pub struct LeaderMetricsState {
    inner: Arc<Mutex<Option<DistributedProgressUpdate>>>,
}

impl Default for LeaderMetricsState {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaderMetricsState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Called from the coordinator's on_progress callback each tick.
    pub fn update(&self, progress: &DistributedProgressUpdate) {
        *self.inner.lock().unwrap() = Some(progress.clone());
    }

    fn snapshot(&self) -> Option<DistributedProgressUpdate> {
        self.inner.lock().unwrap().clone()
    }
}

/// A tiny HTTP server exposing aggregated Prometheus metrics on the leader.
pub struct LeaderServer {
    state: LeaderMetricsState,
}

impl LeaderServer {
    pub fn new(state: LeaderMetricsState) -> Self {
        Self { state }
    }

    /// Spawn the server on a background thread. Returns immediately.
    pub fn spawn(self, port: u16) -> std::io::Result<()> {
        let addr = format!("0.0.0.0:{port}");
        let server = tiny_http::Server::http(&addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e))?;

        tracing::info!(port, "leader metrics server listening");

        thread::Builder::new()
            .name("leader-metrics".into())
            .spawn(move || {
                for request in server.incoming_requests() {
                    let path = request.url().to_string();
                    match path.as_str() {
                        "/metrics/prometheus" | "/metrics" => {
                            self.handle_prometheus(request);
                        }
                        _ => {
                            let resp = tiny_http::Response::from_string("not found\n")
                                .with_status_code(404);
                            let _ = request.respond(resp);
                        }
                    }
                }
            })?;

        Ok(())
    }

    fn handle_prometheus(&self, request: tiny_http::Request) {
        let body = match self.state.snapshot() {
            Some(p) => format_prometheus(&p),
            None => "# No metrics available yet\n".to_string(),
        };

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
}

/// Format aggregated distributed metrics as Prometheus text exposition.
fn format_prometheus(p: &DistributedProgressUpdate) -> String {
    let mut out = String::with_capacity(2048);

    let total_requests = p.total_requests;
    let total_errors = p.total_errors;
    let error_rate = if total_requests > 0 {
        total_errors as f64 / total_requests as f64
    } else {
        0.0
    };

    // Sum current_rps across all nodes
    let current_rps: f64 = p.per_node.iter().map(|(_, m)| m.current_rps).sum();

    // Conservative aggregation: max of percentiles across nodes
    let p50 = p
        .per_node
        .iter()
        .map(|(_, m)| m.latency_p50_ms)
        .fold(0.0f64, f64::max);
    let p90 = p
        .per_node
        .iter()
        .map(|(_, m)| m.latency_p90_ms)
        .fold(0.0f64, f64::max);
    let p99 = p
        .per_node
        .iter()
        .map(|(_, m)| m.latency_p99_ms)
        .fold(0.0f64, f64::max);

    // -- Counters --
    out.push_str("# HELP netanvil_requests_total Total requests sent across all agents.\n");
    out.push_str("# TYPE netanvil_requests_total counter\n");
    out.push_str(&format!("netanvil_requests_total {total_requests}\n"));

    out.push_str("# HELP netanvil_errors_total Total errors across all agents.\n");
    out.push_str("# TYPE netanvil_errors_total counter\n");
    out.push_str(&format!("netanvil_errors_total {total_errors}\n"));

    // -- Gauges --
    out.push_str("# HELP netanvil_request_rate Current aggregate requests per second.\n");
    out.push_str("# TYPE netanvil_request_rate gauge\n");
    out.push_str(&format!("netanvil_request_rate {current_rps:.1}\n"));

    out.push_str("# HELP netanvil_target_rps Target requests per second (total).\n");
    out.push_str("# TYPE netanvil_target_rps gauge\n");
    out.push_str(&format!("netanvil_target_rps {:.1}\n", p.target_rps));

    out.push_str("# HELP netanvil_error_rate Aggregate error rate (0-1).\n");
    out.push_str("# TYPE netanvil_error_rate gauge\n");
    out.push_str(&format!("netanvil_error_rate {error_rate:.6}\n"));

    out.push_str("# HELP netanvil_elapsed_seconds Test elapsed time.\n");
    out.push_str("# TYPE netanvil_elapsed_seconds gauge\n");
    out.push_str(&format!(
        "netanvil_elapsed_seconds {:.1}\n",
        p.elapsed.as_secs_f64()
    ));

    out.push_str("# HELP netanvil_active_nodes Number of active agent nodes.\n");
    out.push_str("# TYPE netanvil_active_nodes gauge\n");
    out.push_str(&format!("netanvil_active_nodes {}\n", p.active_nodes));

    // -- Latency gauges (max across nodes, conservative) --
    out.push_str(
        "# HELP netanvil_latency_p50_seconds 50th percentile latency (max across agents).\n",
    );
    out.push_str("# TYPE netanvil_latency_p50_seconds gauge\n");
    out.push_str(&format!(
        "netanvil_latency_p50_seconds {:.6}\n",
        p50 / 1000.0
    ));

    out.push_str(
        "# HELP netanvil_latency_p90_seconds 90th percentile latency (max across agents).\n",
    );
    out.push_str("# TYPE netanvil_latency_p90_seconds gauge\n");
    out.push_str(&format!(
        "netanvil_latency_p90_seconds {:.6}\n",
        p90 / 1000.0
    ));

    out.push_str(
        "# HELP netanvil_latency_p99_seconds 99th percentile latency (max across agents).\n",
    );
    out.push_str("# TYPE netanvil_latency_p99_seconds gauge\n");
    out.push_str(&format!(
        "netanvil_latency_p99_seconds {:.6}\n",
        p99 / 1000.0
    ));

    // -- Per-node breakdown --
    out.push_str(
        "# HELP netanvil_node_requests_total Total requests per agent node.\n",
    );
    out.push_str("# TYPE netanvil_node_requests_total counter\n");
    for (id, m) in &p.per_node {
        out.push_str(&format!(
            "netanvil_node_requests_total{{node=\"{id}\"}} {}\n",
            m.total_requests
        ));
    }

    out.push_str("# HELP netanvil_node_request_rate Current RPS per agent node.\n");
    out.push_str("# TYPE netanvil_node_request_rate gauge\n");
    for (id, m) in &p.per_node {
        out.push_str(&format!(
            "netanvil_node_request_rate{{node=\"{id}\"}} {:.1}\n",
            m.current_rps
        ));
    }

    out.push_str(
        "# HELP netanvil_node_latency_p99_seconds p99 latency per agent node.\n",
    );
    out.push_str("# TYPE netanvil_node_latency_p99_seconds gauge\n");
    for (id, m) in &p.per_node {
        out.push_str(&format!(
            "netanvil_node_latency_p99_seconds{{node=\"{id}\"}} {:.6}\n",
            m.latency_p99_ms / 1000.0
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_types::{NodeId, RemoteMetrics};
    use std::time::Duration;

    fn make_progress() -> DistributedProgressUpdate {
        DistributedProgressUpdate {
            elapsed: Duration::from_secs(5),
            remaining: Duration::from_secs(25),
            target_rps: 1000.0,
            total_requests: 5000,
            total_errors: 10,
            active_nodes: 2,
            per_node: vec![
                (
                    NodeId("agent-1".into()),
                    RemoteMetrics {
                        node_id: NodeId("agent-1".into()),
                        current_rps: 450.0,
                        target_rps: 500.0,
                        total_requests: 2400,
                        total_errors: 4,
                        error_rate: 0.0017,
                        latency_p50_ms: 5.0,
                        latency_p90_ms: 15.0,
                        latency_p99_ms: 45.0,
                    },
                ),
                (
                    NodeId("agent-2".into()),
                    RemoteMetrics {
                        node_id: NodeId("agent-2".into()),
                        current_rps: 480.0,
                        target_rps: 500.0,
                        total_requests: 2600,
                        total_errors: 6,
                        error_rate: 0.0023,
                        latency_p50_ms: 4.0,
                        latency_p90_ms: 12.0,
                        latency_p99_ms: 50.0,
                    },
                ),
            ],
        }
    }

    #[test]
    fn test_format_prometheus_contains_aggregate_metrics() {
        let p = make_progress();
        let body = format_prometheus(&p);

        assert!(body.contains("netanvil_requests_total 5000"));
        assert!(body.contains("netanvil_errors_total 10"));
        assert!(body.contains("netanvil_target_rps 1000.0"));
        assert!(body.contains("netanvil_active_nodes 2"));
        // current_rps = 450 + 480 = 930
        assert!(body.contains("netanvil_request_rate 930.0"));
        // p99 = max(45, 50) = 50ms = 0.050000s
        assert!(body.contains("netanvil_latency_p99_seconds 0.050000"));
        // p50 = max(5, 4) = 5ms = 0.005000s
        assert!(body.contains("netanvil_latency_p50_seconds 0.005000"));
    }

    #[test]
    fn test_format_prometheus_contains_per_node_metrics() {
        let p = make_progress();
        let body = format_prometheus(&p);

        assert!(body.contains("netanvil_node_requests_total{node=\"agent-1\"} 2400"));
        assert!(body.contains("netanvil_node_requests_total{node=\"agent-2\"} 2600"));
        assert!(body.contains("netanvil_node_request_rate{node=\"agent-1\"} 450.0"));
        assert!(body.contains("netanvil_node_request_rate{node=\"agent-2\"} 480.0"));
    }

    #[test]
    fn test_leader_metrics_state_update_and_snapshot() {
        let state = LeaderMetricsState::new();
        assert!(state.snapshot().is_none());

        let p = make_progress();
        state.update(&p);

        let snap = state.snapshot().unwrap();
        assert_eq!(snap.total_requests, 5000);
        assert_eq!(snap.active_nodes, 2);
    }
}
