use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, SchedulerConfig, TestConfig};

mod output;

#[derive(Parser)]
#[command(name = "netanvil")]
#[command(about = "High-performance HTTP load testing")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Run a load test
    Test {
        /// Target URL(s). Specify multiple times for round-robin.
        #[arg(long, required = true, num_args = 1..)]
        url: Vec<String>,

        /// HTTP method
        #[arg(long, default_value = "GET")]
        method: String,

        /// Target requests per second
        #[arg(long, default_value = "100")]
        rps: f64,

        /// Test duration (e.g. "10s", "1m", "500ms")
        #[arg(long, default_value = "10s")]
        duration: String,

        /// Scheduling discipline: "constant" or "poisson"
        #[arg(long, default_value = "constant")]
        scheduler: String,

        /// Number of worker cores (0 = auto-detect)
        #[arg(long, default_value = "0")]
        cores: usize,

        /// Per-request timeout (e.g. "30s", "5s")
        #[arg(long, default_value = "30s")]
        timeout: String,

        /// Add a header to every request (format: "Name: Value"). Repeatable.
        #[arg(long = "header", short = 'H', num_args = 1)]
        headers: Vec<String>,

        /// HTTP status codes >= this value count as errors (0 = transport errors only)
        #[arg(long, default_value = "400")]
        error_threshold: u16,
    },
}

fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms.parse().context("invalid milliseconds")?;
        return Ok(Duration::from_millis(n));
    }
    if let Some(secs) = s.strip_suffix('s') {
        let n: f64 = secs.parse().context("invalid seconds")?;
        return Ok(Duration::from_secs_f64(n));
    }
    if let Some(mins) = s.strip_suffix('m') {
        let n: f64 = mins.parse().context("invalid minutes")?;
        return Ok(Duration::from_secs_f64(n * 60.0));
    }
    let n: f64 = s.parse().context("invalid duration")?;
    Ok(Duration::from_secs_f64(n))
}

fn parse_header(s: &str) -> Result<(String, String)> {
    let (name, value) = s
        .split_once(':')
        .context("header must be in 'Name: Value' format")?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

fn parse_scheduler(s: &str) -> Result<SchedulerConfig> {
    match s.to_lowercase().as_str() {
        "constant" | "const" => Ok(SchedulerConfig::ConstantRate),
        "poisson" => Ok(SchedulerConfig::Poisson { seed: None }),
        other => anyhow::bail!("unknown scheduler: {other} (use 'constant' or 'poisson')"),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Test {
            url,
            method,
            rps,
            duration,
            scheduler,
            cores,
            timeout,
            headers,
            error_threshold,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;
            let scheduler = parse_scheduler(&scheduler).context("invalid --scheduler")?;
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|h| parse_header(h))
                .collect::<Result<_>>()?;

            let config = TestConfig {
                targets: url,
                method,
                duration,
                rate: RateConfig::Static { rps },
                scheduler,
                headers,
                num_cores: cores,
                connections: ConnectionConfig {
                    request_timeout: timeout,
                    ..Default::default()
                },
                metrics_interval: Duration::from_millis(500),
                control_interval: Duration::from_millis(100),
                error_status_threshold: error_threshold,
            };

            eprintln!(
                "Running load test: {} target(s), {rps} RPS, {:?} duration, {} cores",
                config.targets.len(),
                duration,
                if cores == 0 {
                    "auto".to_string()
                } else {
                    cores.to_string()
                },
            );

            let request_timeout = timeout;
            let result = run_test(config, move || HttpExecutor::with_timeout(request_timeout))
                .context("load test failed")?;

            output::print_results(&result);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(
            parse_duration("1.5s").unwrap(),
            Duration::from_secs_f64(1.5)
        );
    }

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn parse_duration_bare_number() {
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_header_valid() {
        let (name, value) = parse_header("Content-Type: application/json").unwrap();
        assert_eq!(name, "Content-Type");
        assert_eq!(value, "application/json");
    }

    #[test]
    fn parse_header_invalid() {
        assert!(parse_header("no-colon-here").is_err());
    }

    #[test]
    fn parse_scheduler_variants() {
        assert!(matches!(
            parse_scheduler("constant").unwrap(),
            SchedulerConfig::ConstantRate
        ));
        assert!(matches!(
            parse_scheduler("poisson").unwrap(),
            SchedulerConfig::Poisson { .. }
        ));
        assert!(parse_scheduler("invalid").is_err());
    }
}
