use std::time::Duration;

/// Final test results returned after a load test completes.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub total_requests: u64,
    pub total_errors: u64,
    pub duration: Duration,
    pub latency_p50: Duration,
    pub latency_p90: Duration,
    pub latency_p99: Duration,
    pub latency_max: Duration,
    pub request_rate: f64,
    pub error_rate: f64,
}
