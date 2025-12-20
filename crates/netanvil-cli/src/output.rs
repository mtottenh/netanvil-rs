use netanvil_core::{Report, TestResult};
use netanvil_distributed::result_store::StoredTestResult;

/// Output format for test results.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    /// Human-readable table (default)
    Text,
    /// Machine-readable JSON to stdout
    Json,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "text" | "table" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unknown output format: {other} (use 'text' or 'json')"
            )),
        }
    }
}

pub fn print_results(result: &TestResult, format: OutputFormat) {
    match format {
        OutputFormat::Text => {
            eprint!("{}", Report::new(result));
        }
        OutputFormat::Json => {
            // JSON goes to stdout (pipeable), not stderr
            let json = serde_json::to_string_pretty(result).expect("serialize TestResult");
            println!("{json}");
        }
    }
}

/// Print distributed test results. Text goes to stderr, JSON to stdout.
///
/// Used by both one-shot leader mode (via TestQueue) and the daemon's
/// result API. Both paths produce `StoredTestResult` from the same
/// `DistributedTestResult::from_distributed()` conversion.
pub fn print_stored_results(result: &StoredTestResult, format: OutputFormat) {
    match format {
        OutputFormat::Text => {
            eprintln!();
            eprintln!("─── Distributed Test Results ───");
            eprintln!("  Duration:       {:.1}s", result.duration_secs);
            eprintln!("  Total requests: {}", result.total_requests);
            eprintln!("  Total errors:   {}", result.total_errors);
            eprintln!("  Avg RPS:        {:.1}", result.request_rate);
            eprintln!("  Error rate:     {:.4}%", result.error_rate * 100.0);
            eprintln!("  Latency p50:    {:.2}ms", result.latency_p50_ms);
            eprintln!("  Latency p90:    {:.2}ms", result.latency_p90_ms);
            eprintln!("  Latency p99:    {:.2}ms", result.latency_p99_ms);
            eprintln!("  Nodes:          {}", result.nodes.len());
            for node in &result.nodes {
                eprintln!(
                    "    {}: {} reqs, {:.1} RPS, p99={:.2}ms",
                    node.node_id, node.total_requests, node.current_rps, node.latency_p99_ms,
                );
            }
            eprintln!("────────────────────────────────");
        }
        OutputFormat::Json => {
            let json =
                serde_json::to_string_pretty(result).expect("serialize StoredTestResult");
            println!("{json}");
        }
    }
}
