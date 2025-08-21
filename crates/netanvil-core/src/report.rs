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

        // Show throughput if any bytes were transferred
        if r.total_bytes_sent > 0 || r.total_bytes_received > 0 {
            writeln!(f)?;
            writeln!(f, "  Throughput:")?;
            if r.total_bytes_sent > 0 {
                writeln!(
                    f,
                    "    Send:          {:>10.1} Mbps ({:.1} MB)",
                    r.throughput_send_mbps,
                    r.total_bytes_sent as f64 / 1_000_000.0
                )?;
            }
            if r.total_bytes_received > 0 {
                writeln!(
                    f,
                    "    Receive:       {:>10.1} Mbps ({:.1} MB)",
                    r.throughput_recv_mbps,
                    r.total_bytes_received as f64 / 1_000_000.0
                )?;
            }
        }

        // Show saturation info if any signals were detected
        let s = &r.saturation;
        if s.backpressure_drops > 0
            || s.scheduling_delay_mean_ms > 1.0
            || s.delayed_request_ratio > 0.01
        {
            writeln!(f)?;
            writeln!(f, "  Client saturation:")?;
            if s.backpressure_drops > 0 {
                writeln!(
                    f,
                    "    Backpressure:  {:>10} dropped ({:.1}%)",
                    s.backpressure_drops,
                    s.backpressure_ratio * 100.0
                )?;
            }
            writeln!(
                f,
                "    Sched delay:   {:>10.2}ms mean, {:.2}ms max",
                s.scheduling_delay_mean_ms, s.scheduling_delay_max_ms
            )?;
            writeln!(
                f,
                "    Delayed (>1ms):{:>10.1}% of requests",
                s.delayed_request_ratio * 100.0
            )?;
            use netanvil_types::SaturationAssessment;
            match s.assessment {
                SaturationAssessment::ClientSaturated => {
                    writeln!(
                        f,
                        "    Assessment:    CLIENT SATURATED — add more cores or nodes"
                    )?;
                }
                SaturationAssessment::BothSaturated => {
                    writeln!(f, "    Assessment:    CLIENT+SERVER SATURATED")?;
                }
                _ => {}
            }
        }

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
        let remaining = format_duration_short(u.remaining);

        // If significant bytes are flowing, show throughput alongside RPS
        let secs = u.elapsed.as_secs_f64().max(0.001);
        let send_mbps = u.total_bytes_sent as f64 * 8.0 / secs / 1_000_000.0;
        let recv_mbps = u.total_bytes_received as f64 * 8.0 / secs / 1_000_000.0;
        let has_throughput = send_mbps > 0.1 || recv_mbps > 0.1;

        if has_throughput {
            write!(
                f,
                "[{remaining:>6} remaining]  {rps:>8.1} ops/s  |  {send:>7.1} Mbps send  |  total {total:>8}  errors {errors}",
                rps = u.current_rps,
                send = send_mbps,
                total = u.total_requests,
                errors = u.total_errors,
            )?;
        } else {
            let p99_ms = u.window.latency_p99_ns as f64 / 1_000_000.0;
            write!(
                f,
                "[{remaining:>6} remaining]  {rps:>8.1} req/s  |  p99 {p99:>7.1}ms  |  total {total:>8}  errors {errors}",
                rps = u.current_rps,
                p99 = p99_ms,
                total = u.total_requests,
                errors = u.total_errors,
            )?;
        }

        // Show saturation warning inline when detected
        use netanvil_types::SaturationAssessment;
        match u.saturation.assessment {
            SaturationAssessment::ClientSaturated => write!(f, "  [CLIENT SATURATED]")?,
            SaturationAssessment::ServerSaturated => write!(f, "  [SERVER SATURATED]")?,
            SaturationAssessment::BothSaturated => write!(f, "  [CLIENT+SERVER SATURATED]")?,
            SaturationAssessment::Healthy => {}
        }
        Ok(())
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
