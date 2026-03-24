use std::sync::{Arc, Mutex};

use netanvil_core::ProgressUpdate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Shared state between the coordinator and HTTP server.
/// Written by the coordinator's on_progress callback, read by GET endpoints.
#[derive(Debug)]
pub struct SharedState {
    inner: Arc<Mutex<SharedStateInner>>,
}

#[derive(Debug, Clone, Default)]
struct SharedStateInner {
    pub status: TestStatus,
    pub metrics: Option<MetricsView>,
    /// Signals pushed via PUT /signal. Read by the coordinator each tick.
    pub pushed_signals: Vec<(String, f64)>,
    /// Node identifier, included as a label in Prometheus metrics.
    pub node_id: Option<String>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SharedStateInner::default())),
        }
    }

    /// Set the node identifier for Prometheus metric labels.
    pub fn set_node_id(&self, id: String) {
        self.inner.lock().unwrap().node_id = Some(id);
    }

    /// Get the node identifier (if set).
    pub fn get_node_id(&self) -> Option<String> {
        self.inner.lock().unwrap().node_id.clone()
    }

    /// Update from a ProgressUpdate (called by the coordinator callback).
    pub fn update_from_progress(&self, update: &ProgressUpdate) {
        let mut inner = self.inner.lock().unwrap();
        inner.status = TestStatus {
            state: "running".into(),
            elapsed_secs: update.elapsed.as_secs_f64(),
            remaining_secs: update.remaining.as_secs_f64(),
            target_rps: update.target_rps,
            current_rps: update.current_rps,
            total_requests: update.total_requests,
            total_errors: update.total_errors,
        };
        let secs = update.elapsed.as_secs_f64().max(0.001);
        inner.metrics = Some(MetricsView {
            current_rps: update.current_rps,
            target_rps: update.target_rps,
            total_requests: update.total_requests,
            total_errors: update.total_errors,
            error_rate: if update.total_requests > 0 {
                update.total_errors as f64 / update.total_requests as f64
            } else {
                0.0
            },
            latency_p50_ms: update.window.latency_p50_ns as f64 / 1_000_000.0,
            latency_p90_ms: update.window.latency_p90_ns as f64 / 1_000_000.0,
            latency_p99_ms: update.window.latency_p99_ns as f64 / 1_000_000.0,
            elapsed_secs: update.elapsed.as_secs_f64(),
            remaining_secs: update.remaining.as_secs_f64(),
            bytes_sent: update.total_bytes_sent,
            bytes_received: update.total_bytes_received,
            throughput_send_mbps: update.total_bytes_sent as f64 * 8.0 / secs / 1_000_000.0,
            throughput_recv_mbps: update.total_bytes_received as f64 * 8.0 / secs / 1_000_000.0,
            latency_buckets: update.latency_buckets.clone(),
            saturation: update.saturation.clone(),
            packets_sent: update.packets_sent,
            packets_received: update.packets_received,
            packets_lost: update.packets_lost,
            timeout_count: update.total_timeouts,
        });
    }

    pub fn get_status(&self) -> TestStatus {
        self.inner.lock().unwrap().status.clone()
    }

    pub fn get_metrics(&self) -> Option<MetricsView> {
        self.inner.lock().unwrap().metrics.clone()
    }

    pub fn mark_completed(&self) {
        self.inner.lock().unwrap().status.state = "completed".into();
    }

    /// Reset metrics to zero/empty. Called when a test ends so that
    /// Prometheus gauges don't retain stale values from the last run.
    pub fn reset_metrics(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.metrics = None;
        inner.status = TestStatus::default();
    }

    /// Push an external signal (e.g., from PUT /signal).
    /// Overwrites any existing signal with the same name.
    pub fn push_signal(&self, name: String, value: f64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.pushed_signals.iter_mut().find(|(k, _)| k == &name) {
            existing.1 = value;
        } else {
            inner.pushed_signals.push((name, value));
        }
    }

    /// Drain all pushed signals. Signals are consumed — each push is a
    /// single update, not a persistent value. If the external system stops
    /// pushing, the signal disappears from the next coordinator tick.
    pub fn drain_pushed_signals(&self) -> Vec<(String, f64)> {
        std::mem::take(&mut self.inner.lock().unwrap().pushed_signals)
    }
}

impl Clone for SharedState {
    fn clone(&self) -> Self {
        // SharedState's inner Arc handles the real sharing;
        // this clone just bumps the Arc refcount.
        Self {
            inner: self.inner.clone(),
        }
    }
}

// --- JSON response types ---

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct TestStatus {
    pub state: String,
    pub elapsed_secs: f64,
    pub remaining_secs: f64,
    pub target_rps: f64,
    pub current_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MetricsView {
    pub current_rps: f64,
    pub target_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub error_rate: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p99_ms: f64,
    pub elapsed_secs: f64,
    pub remaining_secs: f64,
    /// Total bytes sent across all cores (cumulative).
    #[serde(default)]
    pub bytes_sent: u64,
    /// Total bytes received across all cores (cumulative).
    #[serde(default)]
    pub bytes_received: u64,
    /// Send throughput in megabits per second.
    #[serde(default)]
    pub throughput_send_mbps: f64,
    /// Receive throughput in megabits per second.
    #[serde(default)]
    pub throughput_recv_mbps: f64,
    /// Cumulative histogram buckets: (upper_bound_seconds, cumulative_count).
    /// Standard Prometheus bucket boundaries.
    #[serde(default)]
    pub latency_buckets: Vec<(f64, u64)>,
    /// Client/server saturation assessment.
    #[serde(default)]
    pub saturation: netanvil_types::SaturationInfo,
    /// Protocol-level packets sent (cumulative).
    #[serde(default)]
    pub packets_sent: u64,
    /// Protocol-level packets received (cumulative).
    #[serde(default)]
    pub packets_received: u64,
    /// Protocol-level packets declared lost (cumulative).
    #[serde(default)]
    pub packets_lost: u64,
    /// Requests that completed due to timeout (cumulative).
    /// Tracked separately because timeout "latency" is the timeout duration,
    /// not the server's response time, and is excluded from the histogram.
    #[serde(default)]
    pub timeout_count: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ApiResponse {
    pub fn success() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

// --- JSON request body types ---

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRateRequest {
    pub rps: f64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTargetsRequest {
    pub targets: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMetadataRequest {
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PushSignalRequest {
    pub name: String,
    pub value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use netanvil_core::ProgressUpdate;
    use netanvil_types::{MetricsSummary, SaturationInfo};
    use std::time::Duration;

    #[test]
    fn reset_metrics_zeroes_state_after_test() {
        let state = SharedState::new();

        // Simulate a running test with non-zero metrics.
        state.update_from_progress(&ProgressUpdate {
            elapsed: Duration::from_secs(10),
            remaining: Duration::from_secs(50),
            target_rps: 1000.0,
            current_rps: 950.0,
            total_requests: 9500,
            total_errors: 12,
            total_bytes_sent: 1_000_000,
            total_bytes_received: 2_000_000,
            window: MetricsSummary {
                latency_p50_ns: 5_000_000,
                latency_p90_ns: 10_000_000,
                latency_p99_ns: 20_000_000,
                ..Default::default()
            },
            latency_buckets: vec![(0.005, 100), (0.01, 500), (f64::INFINITY, 9500)],
            saturation: SaturationInfo::default(),
            packets_sent: 0,
            packets_received: 0,
            packets_lost: 0,
            total_timeouts: 0,
        });

        // Verify metrics are populated.
        let m = state.get_metrics().expect("metrics should be present");
        assert!(m.total_requests > 0);
        assert!(m.current_rps > 0.0);

        // Reset (simulates test completion).
        state.reset_metrics();

        // Metrics should now be None (Prometheus handler returns empty).
        assert!(state.get_metrics().is_none(), "metrics should be cleared");

        // Status should be back to defaults.
        let status = state.get_status();
        assert_eq!(status.state, "");
        assert_eq!(status.total_requests, 0);
        assert_eq!(status.current_rps, 0.0);
    }
}
