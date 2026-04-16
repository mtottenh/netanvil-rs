//! Metrics reporting modes and formatters.
//!
//! Defines the output mode for server-side metrics reporting. The actual
//! periodic reporting (spawning a reporter thread, formatting text/JSON
//! output) is deferred — the `metrics_snapshot()` API on `TestServer` is
//! the primary programmatic interface.

/// Output mode for server-side metrics reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReportMode {
    /// No periodic reporting.
    #[default]
    None,
    /// iperf3-style text output to stderr.
    Text,
    /// One JSON object per line per interval to stderr.
    Json,
}
