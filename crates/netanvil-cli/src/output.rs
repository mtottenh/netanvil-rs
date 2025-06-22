use netanvil_core::{Report, TestResult};

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
