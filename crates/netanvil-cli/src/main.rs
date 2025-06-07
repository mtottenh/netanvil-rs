use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use netanvil_core::run_test;
use netanvil_http::HttpExecutor;
use netanvil_types::{ConnectionConfig, RateConfig, TestConfig};

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

        /// Target requests per second
        #[arg(long, default_value = "100")]
        rps: f64,

        /// Test duration (e.g. "10s", "1m", "500ms")
        #[arg(long, default_value = "10s")]
        duration: String,

        /// Number of worker cores (0 = auto-detect)
        #[arg(long, default_value = "0")]
        cores: usize,

        /// Per-request timeout (e.g. "30s", "5s")
        #[arg(long, default_value = "30s")]
        timeout: String,
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
    // Bare number — treat as seconds
    let n: f64 = s.parse().context("invalid duration")?;
    Ok(Duration::from_secs_f64(n))
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
            rps,
            duration,
            cores,
            timeout,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;

            let config = TestConfig {
                targets: url,
                duration,
                rate: RateConfig::Static { rps },
                num_cores: cores,
                connections: ConnectionConfig {
                    request_timeout: timeout,
                    ..Default::default()
                },
                metrics_interval: Duration::from_millis(500),
                control_interval: Duration::from_millis(100),
            };

            eprintln!(
                "Running load test: {} target(s), {rps} RPS, {:?} duration, {} cores",
                config.targets.len(),
                duration,
                if cores == 0 { "auto".to_string() } else { cores.to_string() },
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
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_secs_f64(1.5));
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
}
