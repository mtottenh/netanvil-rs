use std::fmt;

use crate::result::TestResult;

/// A formatted report of test results.
///
/// Wraps `TestResult` with a `Display` implementation that produces
/// a human-readable summary. Keeps data (TestResult) separate from
/// presentation.
pub struct Report<'a> {
    pub result: &'a TestResult,
}

impl<'a> Report<'a> {
    pub fn new(result: &'a TestResult) -> Self {
        Self { result }
    }
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.result;

        writeln!(f)?;
        writeln!(f, "\u{2501}\u{2501}\u{2501} Results \u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}")?;
        writeln!(f, "  Duration:        {:>10.2?}", r.duration)?;
        writeln!(f, "  Total requests:  {:>10}", r.total_requests)?;
        writeln!(f, "  Total errors:    {:>10}", r.total_errors)?;
        writeln!(f, "  Request rate:    {:>10.1} req/s", r.request_rate)?;
        writeln!(f, "  Error rate:      {:>10.2}%", r.error_rate * 100.0)?;
        writeln!(f)?;
        writeln!(f, "  Latency:")?;
        writeln!(f, "    p50:           {:>10.2?}", r.latency_p50)?;
        writeln!(f, "    p90:           {:>10.2?}", r.latency_p90)?;
        writeln!(f, "    p99:           {:>10.2?}", r.latency_p99)?;
        writeln!(f, "    max:           {:>10.2?}", r.latency_max)?;
        write!(f, "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}")
    }
}
