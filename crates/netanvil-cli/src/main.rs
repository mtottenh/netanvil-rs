use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use netanvil_api::{ControlServer, SharedState};
use netanvil_core::{run_test_with_api, run_test_with_progress, report::ProgressLine};
use netanvil_http::HttpExecutor;
use netanvil_types::{
    ConnectionConfig, PidTarget, RateConfig, SchedulerConfig, TargetMetric, TestConfig,
};

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

        /// Target requests per second (initial RPS for PID mode)
        #[arg(long, default_value = "100")]
        rps: f64,

        /// Test duration (e.g. "10s", "1m", "500ms")
        #[arg(long, default_value = "10s")]
        duration: String,

        /// Rate control mode: "static", "step", or "pid"
        #[arg(long, default_value = "static")]
        rate_mode: String,

        /// Step definition for step mode: "0s:100,5s:500,10s:200"
        #[arg(long)]
        steps: Option<String>,

        /// PID target metric: "latency-p50", "latency-p90", "latency-p99", "error-rate"
        #[arg(long, default_value = "latency-p99")]
        pid_metric: String,

        /// PID target value (e.g. 200.0 for 200ms latency, or 5.0 for 5% error rate)
        #[arg(long, default_value = "200.0")]
        pid_target: f64,

        /// PID proportional gain
        #[arg(long, default_value = "0.1")]
        pid_kp: f64,

        /// PID integral gain
        #[arg(long, default_value = "0.01")]
        pid_ki: f64,

        /// PID derivative gain
        #[arg(long, default_value = "0.05")]
        pid_kd: f64,

        /// PID minimum RPS
        #[arg(long, default_value = "10")]
        pid_min_rps: f64,

        /// PID maximum RPS
        #[arg(long, default_value = "10000")]
        pid_max_rps: f64,

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

        /// Enable HTTP control API on this port (e.g. 8080). Allows mid-test
        /// rate changes, target updates, and live metrics via curl.
        #[arg(long)]
        api_port: Option<u16>,
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

fn parse_target_metric(s: &str) -> Result<TargetMetric> {
    match s.to_lowercase().as_str() {
        "latency-p50" | "p50" => Ok(TargetMetric::LatencyP50),
        "latency-p90" | "p90" => Ok(TargetMetric::LatencyP90),
        "latency-p99" | "p99" => Ok(TargetMetric::LatencyP99),
        "error-rate" | "errors" => Ok(TargetMetric::ErrorRate),
        other => anyhow::bail!(
            "unknown PID metric: {other} (use 'latency-p50', 'latency-p90', 'latency-p99', or 'error-rate')"
        ),
    }
}

/// Parse step definitions: "0s:100,5s:500,10s:200"
fn parse_steps(s: &str) -> Result<Vec<(Duration, f64)>> {
    let mut steps = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let (time_str, rps_str) = part
            .split_once(':')
            .context("step must be in 'duration:rps' format (e.g. '5s:500')")?;
        let time = parse_duration(time_str.trim())?;
        let rps: f64 = rps_str.trim().parse().context("invalid RPS in step")?;
        steps.push((time, rps));
    }
    if steps.is_empty() {
        anyhow::bail!("at least one step is required");
    }
    Ok(steps)
}

fn build_rate_config(
    rate_mode: &str,
    rps: f64,
    steps: Option<&str>,
    pid_metric: &str,
    pid_target: f64,
    pid_kp: f64,
    pid_ki: f64,
    pid_kd: f64,
    pid_min_rps: f64,
    pid_max_rps: f64,
) -> Result<RateConfig> {
    match rate_mode.to_lowercase().as_str() {
        "static" | "const" => Ok(RateConfig::Static { rps }),
        "step" => {
            let steps_str = steps.context("--steps required for step rate mode")?;
            let steps = parse_steps(steps_str)?;
            Ok(RateConfig::Step { steps })
        }
        "pid" => {
            let metric = parse_target_metric(pid_metric)?;
            Ok(RateConfig::Pid {
                initial_rps: rps,
                target: PidTarget {
                    metric,
                    value: pid_target,
                    kp: pid_kp,
                    ki: pid_ki,
                    kd: pid_kd,
                    min_rps: pid_min_rps,
                    max_rps: pid_max_rps,
                },
            })
        }
        other => anyhow::bail!("unknown rate mode: {other} (use 'static', 'step', or 'pid')"),
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
            rate_mode,
            steps,
            pid_metric,
            pid_target,
            pid_kp,
            pid_ki,
            pid_kd,
            pid_min_rps,
            pid_max_rps,
            scheduler,
            cores,
            timeout,
            headers,
            error_threshold,
            api_port,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;
            let scheduler = parse_scheduler(&scheduler).context("invalid --scheduler")?;
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|h| parse_header(h))
                .collect::<Result<_>>()?;
            let rate = build_rate_config(
                &rate_mode,
                rps,
                steps.as_deref(),
                &pid_metric,
                pid_target,
                pid_kp,
                pid_ki,
                pid_kd,
                pid_min_rps,
                pid_max_rps,
            )?;

            let config = TestConfig {
                targets: url,
                method,
                duration,
                rate,
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
                ..Default::default()
            };

            let rate_desc = match &config.rate {
                RateConfig::Static { rps } => format!("{rps} RPS (static)"),
                RateConfig::Step { steps } => format!("{} steps", steps.len()),
                RateConfig::Pid { initial_rps, .. } => {
                    format!("{initial_rps} RPS initial (PID)")
                }
            };

            eprintln!(
                "Running load test: {} target(s), {rate_desc}, {:?} duration, {} cores",
                config.targets.len(),
                duration,
                if cores == 0 {
                    "auto".to_string()
                } else {
                    cores.to_string()
                },
            );

            let request_timeout = timeout;
            let result = if let Some(port) = api_port {
                // API mode: spawn control server, wire up shared state
                let shared_state = SharedState::new();
                let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

                let server = ControlServer::new(port, shared_state.clone(), ext_cmd_tx)
                    .context(format!("failed to start API server on port {port}"))?;
                let _server_handle = server.spawn();
                eprintln!("Control API listening on http://0.0.0.0:{port}");

                let progress_state = shared_state.clone();
                run_test_with_api(
                    config,
                    move || HttpExecutor::with_timeout(request_timeout),
                    move |update| {
                        progress_state.update_from_progress(update);
                        eprint!("\r{}", ProgressLine::new(update));
                    },
                    ext_cmd_rx,
                )
                .context("load test failed")?
            } else {
                // No API: simple progress output
                run_test_with_progress(
                    config,
                    move || HttpExecutor::with_timeout(request_timeout),
                    |update| {
                        eprint!("\r{}", ProgressLine::new(update));
                    },
                )
                .context("load test failed")?
            };
            eprintln!(); // newline after progress

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

    #[test]
    fn parse_steps_valid() {
        let steps = parse_steps("0s:100,5s:500,10s:200").unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0], (Duration::from_secs(0), 100.0));
        assert_eq!(steps[1], (Duration::from_secs(5), 500.0));
        assert_eq!(steps[2], (Duration::from_secs(10), 200.0));
    }

    #[test]
    fn parse_steps_with_ms() {
        let steps = parse_steps("0s:50, 500ms:200").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].0, Duration::from_millis(500));
    }

    #[test]
    fn parse_steps_invalid() {
        assert!(parse_steps("not-a-step").is_err());
        assert!(parse_steps("").is_err());
    }

    #[test]
    fn parse_target_metric_variants() {
        assert!(matches!(
            parse_target_metric("latency-p99").unwrap(),
            TargetMetric::LatencyP99
        ));
        assert!(matches!(
            parse_target_metric("p50").unwrap(),
            TargetMetric::LatencyP50
        ));
        assert!(matches!(
            parse_target_metric("error-rate").unwrap(),
            TargetMetric::ErrorRate
        ));
        assert!(parse_target_metric("bogus").is_err());
    }

    #[test]
    fn build_static_rate() {
        let rate = build_rate_config("static", 500.0, None, "p99", 200.0, 0.1, 0.01, 0.05, 10.0, 10000.0).unwrap();
        assert!(matches!(rate, RateConfig::Static { rps } if rps == 500.0));
    }

    #[test]
    fn build_step_rate() {
        let rate = build_rate_config("step", 0.0, Some("0s:100,5s:500"), "p99", 200.0, 0.1, 0.01, 0.05, 10.0, 10000.0).unwrap();
        assert!(matches!(rate, RateConfig::Step { steps } if steps.len() == 2));
    }

    #[test]
    fn build_step_rate_requires_steps() {
        let result = build_rate_config("step", 0.0, None, "p99", 200.0, 0.1, 0.01, 0.05, 10.0, 10000.0);
        assert!(result.is_err());
    }

    #[test]
    fn build_pid_rate() {
        let rate = build_rate_config("pid", 500.0, None, "latency-p99", 200.0, 0.1, 0.01, 0.05, 10.0, 10000.0).unwrap();
        match rate {
            RateConfig::Pid { initial_rps, target } => {
                assert_eq!(initial_rps, 500.0);
                assert_eq!(target.value, 200.0);
                assert!(matches!(target.metric, TargetMetric::LatencyP99));
            }
            _ => panic!("expected Pid"),
        }
    }
}
