use std::fmt;
use std::time::Duration;

use crate::coordinator::ProgressUpdate;
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

/// Formats a single line of live progress for display during a test.
pub struct ProgressLine<'a> {
    pub update: &'a ProgressUpdate,
}

impl<'a> ProgressLine<'a> {
    pub fn new(update: &'a ProgressUpdate) -> Self {
        Self { update }
    }
}

impl fmt::Display for ProgressLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let u = self.update;
        let p99_ms = u.window.latency_p99_ns as f64 / 1_000_000.0;
        let remaining = format_duration_short(u.remaining);

        write!(
            f,
            "[{remaining:>6} remaining]  {rps:>8.1} req/s  |  p99 {p99:>7.1}ms  |  total {total:>8}  errors {errors}",
            rps = u.current_rps,
            p99 = p99_ms,
            total = u.total_requests,
            errors = u.total_errors,
        )
    }
}

fn format_duration_short(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}
