use std::sync::{Arc, Mutex};

use netanvil_core::ProgressUpdate;
use serde::{Deserialize, Serialize};

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
            latency_buckets: update.latency_buckets.clone(),
            saturation: update.saturation.clone(),
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestStatus {
    pub state: String,
    pub elapsed_secs: f64,
    pub remaining_secs: f64,
    pub target_rps: f64,
    pub current_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Cumulative histogram buckets: (upper_bound_seconds, cumulative_count).
    /// Standard Prometheus bucket boundaries.
    #[serde(default)]
    pub latency_buckets: Vec<(f64, u64)>,
    /// Client/server saturation assessment.
    #[serde(default)]
    pub saturation: netanvil_types::SaturationInfo,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Deserialize)]
pub struct UpdateRateRequest {
    pub rps: f64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTargetsRequest {
    pub targets: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMetadataRequest {
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
pub struct PushSignalRequest {
    pub name: String,
    pub value: f64,
}
