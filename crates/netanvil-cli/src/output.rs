use netanvil_core::{Report, TestResult};
use netanvil_distributed::DistributedTestResult;
use serde::Serialize;

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
pub fn print_distributed_results(result: &DistributedTestResult, format: OutputFormat) {
    let summary = DistributedTestSummary::from(result);
    match format {
        OutputFormat::Text => {
            // Text output mirrors the existing tracing::info logs but in a
            // structured report format on stderr.
            let secs = result.duration.as_secs_f64().max(0.001);
            eprintln!();
            eprintln!("─── Distributed Test Results ───");
            eprintln!("  Duration:       {:.1}s", secs);
            eprintln!("  Total requests: {}", summary.total_requests);
            eprintln!("  Total errors:   {}", summary.total_errors);
            eprintln!("  Avg RPS:        {:.1}", summary.request_rate);
            eprintln!("  Error rate:     {:.4}%", summary.error_rate * 100.0);
            eprintln!("  Latency p50:    {:.2}ms", summary.latency_p50_ms);
            eprintln!("  Latency p90:    {:.2}ms", summary.latency_p90_ms);
            eprintln!("  Latency p99:    {:.2}ms", summary.latency_p99_ms);
            eprintln!("  Nodes:          {}", summary.nodes.len());
            for node in &summary.nodes {
                eprintln!(
                    "    {}: {} reqs, {:.1} RPS, p99={:.2}ms",
                    node.node_id, node.total_requests, node.current_rps, node.latency_p99_ms,
                );
            }
            eprintln!("────────────────────────────────");
        }
        OutputFormat::Json => {
            let json =
                serde_json::to_string_pretty(&summary).expect("serialize DistributedTestSummary");
            println!("{json}");
        }
    }
}

/// JSON-serializable summary of a distributed test, mirroring the structure
/// of `TestResult` with additional per-node breakdown.
#[derive(Debug, Clone, Serialize)]
struct DistributedTestSummary {
    pub total_requests: u64,
    pub total_errors: u64,
    pub duration_secs: f64,
    pub request_rate: f64,
    pub error_rate: f64,
    /// Conservative aggregate: max of per-node p50.
    pub latency_p50_ms: f64,
    /// Conservative aggregate: max of per-node p90.
    pub latency_p90_ms: f64,
    /// Conservative aggregate: max of per-node p99.
    pub latency_p99_ms: f64,
    pub nodes: Vec<NodeSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct NodeSummary {
    pub node_id: String,
    pub total_requests: u64,
    pub total_errors: u64,
    pub current_rps: f64,
    pub target_rps: f64,
    pub error_rate: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p99_ms: f64,
}

impl From<&DistributedTestResult> for DistributedTestSummary {
    fn from(r: &DistributedTestResult) -> Self {
        let secs = r.duration.as_secs_f64().max(0.001);
        let error_rate = if r.total_requests > 0 {
            r.total_errors as f64 / r.total_requests as f64
        } else {
            0.0
        };

        let nodes: Vec<NodeSummary> = r
            .nodes
            .iter()
            .map(|(id, m)| NodeSummary {
                node_id: id.0.clone(),
                total_requests: m.total_requests,
                total_errors: m.total_errors,
                current_rps: m.current_rps,
                target_rps: m.target_rps,
                error_rate: m.error_rate,
                latency_p50_ms: m.latency_p50_ms,
                latency_p90_ms: m.latency_p90_ms,
                latency_p99_ms: m.latency_p99_ms,
            })
            .collect();

        // Conservative: max across nodes
        let p50 = nodes.iter().map(|n| n.latency_p50_ms).fold(0.0f64, f64::max);
        let p90 = nodes.iter().map(|n| n.latency_p90_ms).fold(0.0f64, f64::max);
        let p99 = nodes.iter().map(|n| n.latency_p99_ms).fold(0.0f64, f64::max);

        Self {
            total_requests: r.total_requests,
            total_errors: r.total_errors,
            duration_secs: secs,
            request_rate: r.total_requests as f64 / secs,
            error_rate,
            latency_p50_ms: p50,
            latency_p90_ms: p90,
            latency_p99_ms: p99,
            nodes,
        }
    }
}
