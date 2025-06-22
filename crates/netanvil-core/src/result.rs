use std::time::Duration;

use serde::Serialize;

/// Final test results returned after a load test completes.
///
/// Serializable to JSON for machine-readable output (`--output json`).
/// Duration fields are serialized as fractional seconds for easy consumption.
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub total_requests: u64,
    pub total_errors: u64,
    #[serde(serialize_with = "ser_duration_secs")]
    pub duration: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p50: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p90: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_p99: Duration,
    #[serde(serialize_with = "ser_duration_secs")]
    pub latency_max: Duration,
    pub request_rate: f64,
    pub error_rate: f64,
}

fn ser_duration_secs<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}
