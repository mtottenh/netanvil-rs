use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use netanvil_api::{AgentServer, ControlServer, SharedState};
use netanvil_core::{run_test_with_progress, report::ProgressLine};
use netanvil_distributed::{
    DistributedCoordinator, HttpMetricsFetcher, HttpNodeCommander, HttpSignalPoller,
    StaticDiscovery,
};
use netanvil_http::HttpExecutor;
use netanvil_types::{
    ConnectionConfig, ConnectionPolicy, CountDistribution, PidTarget, RateConfig, SchedulerConfig,
    TargetMetric, TestConfig,
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

        /// Connection policy: "keepalive", "always-new", or "mixed"
        #[arg(long, default_value = "keepalive")]
        connection_policy: String,

        /// For mixed policy: fraction of requests that reuse connections (0.0-1.0)
        #[arg(long, default_value = "0.7")]
        persistent_ratio: f64,

        /// Connection lifetime distribution: how many requests per connection.
        /// Format: "fixed:100", "uniform:50,200", "normal:100,20" (mean,stddev).
        /// Omit for unlimited.
        #[arg(long)]
        conn_lifetime: Option<String>,

        /// Output format: "text" (human-readable) or "json" (machine-readable to stdout)
        #[arg(long, default_value = "text")]
        output: String,

        /// Enable HTTP control API on this port (e.g. 8080). Allows mid-test
        /// rate changes, target updates, and live metrics via curl.
        #[arg(long)]
        api_port: Option<u16>,
    },

    /// Run as a remotely controllable agent node
    Agent {
        /// Port to listen on
        #[arg(long, default_value = "9090")]
        listen: u16,

        /// Number of worker cores (0 = auto-detect)
        #[arg(long, default_value = "0")]
        cores: usize,
    },

    /// Run as distributed test leader, coordinating agent nodes
    Leader {
        /// Agent addresses (host:port), comma-separated
        #[arg(long, required = true, value_delimiter = ',')]
        workers: Vec<String>,

        /// Target URL(s)
        #[arg(long, required = true, num_args = 1..)]
        url: Vec<String>,

        /// HTTP method
        #[arg(long, default_value = "GET")]
        method: String,

        /// Target requests per second (total across all agents)
        #[arg(long, default_value = "100")]
        rps: f64,

        /// Test duration (e.g. "10s", "1m")
        #[arg(long, default_value = "10s")]
        duration: String,

        /// Rate control mode: "static", "step", or "pid"
        #[arg(long, default_value = "static")]
        rate_mode: String,

        /// Step definitions for step mode
        #[arg(long)]
        steps: Option<String>,

        /// PID target metric
        #[arg(long, default_value = "latency-p99")]
        pid_metric: String,

        /// PID target value
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

        /// Number of worker cores per agent (0 = auto-detect)
        #[arg(long, default_value = "0")]
        cores: usize,

        /// Per-request timeout
        #[arg(long, default_value = "30s")]
        timeout: String,

        /// Headers to add to every request
        #[arg(long = "header", short = 'H', num_args = 1)]
        headers: Vec<String>,

        /// HTTP status error threshold
        #[arg(long, default_value = "400")]
        error_threshold: u16,

        /// External metrics URL for PID control (JSON endpoint on target)
        #[arg(long)]
        external_metrics_url: Option<String>,

        /// JSON field to extract from external metrics
        #[arg(long)]
        external_metrics_field: Option<String>,
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

fn parse_connection_policy(
    s: &str,
    persistent_ratio: f64,
    conn_lifetime: Option<&str>,
) -> Result<ConnectionPolicy> {
    match s.to_lowercase().as_str() {
        "keepalive" | "keep-alive" => Ok(ConnectionPolicy::KeepAlive),
        "always-new" | "new" => Ok(ConnectionPolicy::AlwaysNew),
        "mixed" => {
            if !(0.0..=1.0).contains(&persistent_ratio) {
                anyhow::bail!("--persistent-ratio must be between 0.0 and 1.0");
            }
            let lifetime = conn_lifetime
                .map(parse_count_distribution)
                .transpose()?;
            Ok(ConnectionPolicy::Mixed {
                persistent_ratio,
                connection_lifetime: lifetime,
            })
        }
        other => anyhow::bail!(
            "unknown connection policy: {other} (use 'keepalive', 'always-new', or 'mixed')"
        ),
    }
}

/// Parse a count distribution: "fixed:100", "uniform:50,200", "normal:100,20"
fn parse_count_distribution(s: &str) -> Result<CountDistribution> {
    let (kind, params) = s
        .split_once(':')
        .context("distribution format: 'fixed:N', 'uniform:min,max', or 'normal:mean,stddev'")?;
    match kind.to_lowercase().as_str() {
        "fixed" => {
            let n: u32 = params.trim().parse().context("fixed:N requires an integer")?;
            Ok(CountDistribution::Fixed(n))
        }
        "uniform" => {
            let (min_s, max_s) = params
                .split_once(',')
                .context("uniform requires min,max (e.g. 'uniform:50,200')")?;
            let min: u32 = min_s.trim().parse().context("invalid min")?;
            let max: u32 = max_s.trim().parse().context("invalid max")?;
            if min > max {
                anyhow::bail!("uniform min ({min}) must be <= max ({max})");
            }
            Ok(CountDistribution::Uniform { min, max })
        }
        "normal" => {
            let (mean_s, stddev_s) = params
                .split_once(',')
                .context("normal requires mean,stddev (e.g. 'normal:100,20')")?;
            let mean: f64 = mean_s.trim().parse().context("invalid mean")?;
            let stddev: f64 = stddev_s.trim().parse().context("invalid stddev")?;
            Ok(CountDistribution::Normal { mean, stddev })
        }
        other => anyhow::bail!(
            "unknown distribution: {other} (use 'fixed', 'uniform', or 'normal')"
        ),
    }
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
                .add_directive(tracing::Level::INFO.into()),
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
            connection_policy,
            persistent_ratio,
            conn_lifetime,
            output,
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

            let conn_policy = parse_connection_policy(
                &connection_policy,
                persistent_ratio,
                conn_lifetime.as_deref(),
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
                    connection_policy: conn_policy,
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
                tracing::info!(port, "control API listening");

                let progress_state = shared_state.clone();
                let signal_state = shared_state.clone();
                netanvil_core::TestBuilder::new(
                    config,
                    move || HttpExecutor::with_timeout(request_timeout),
                )
                .on_progress(move |update| {
                    progress_state.update_from_progress(update);
                    eprint!("\r{}", ProgressLine::new(update));
                })
                .external_commands(ext_cmd_rx)
                .pushed_signal_source(move || signal_state.get_pushed_signals())
                .run()
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

            let output_format = output::OutputFormat::parse(&output)
                .map_err(|e| anyhow::anyhow!(e))?;
            output::print_results(&result, output_format);
        }

        Commands::Agent { listen, cores } => {
            let cores = if cores == 0 {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            } else {
                cores
            };

            tracing::info!(port = listen, cores, "starting agent");
            let server = AgentServer::new(listen, cores)
                .context(format!("failed to start agent on port {listen}"))?;
            server.run(); // blocks until Ctrl+C
        }

        Commands::Leader {
            workers,
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
            cores,
            timeout,
            headers,
            error_threshold,
            external_metrics_url,
            external_metrics_field,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;
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
                scheduler: SchedulerConfig::ConstantRate,
                headers,
                num_cores: cores,
                connections: ConnectionConfig {
                    request_timeout: timeout,
                    ..Default::default()
                },
                metrics_interval: Duration::from_millis(500),
                control_interval: Duration::from_millis(200),
                error_status_threshold: error_threshold,
                external_metrics_url: external_metrics_url.clone(),
                external_metrics_field: external_metrics_field.clone(),
                ..Default::default()
            };

            tracing::info!(
                agents = workers.len(),
                targets = config.targets.len(),
                rps,
                ?duration,
                "starting distributed test"
            );

            // Build rate controller
            use netanvil_core::{PidRateController, StaticRateController, StepRateController};
            let rate_controller: Box<dyn netanvil_types::RateController> = match &config.rate {
                RateConfig::Static { rps } => Box::new(StaticRateController::new(*rps)),
                RateConfig::Step { steps } => {
                    Box::new(StepRateController::with_start_time(
                        steps.clone(),
                        std::time::Instant::now(),
                    ))
                }
                RateConfig::Pid { initial_rps, target } => Box::new(PidRateController::new(
                    target.metric.clone(),
                    target.value,
                    *initial_rps,
                    target.min_rps,
                    target.max_rps,
                    target.kp,
                    target.ki,
                    target.kd,
                )),
            };

            let discovery = StaticDiscovery::new(workers);
            let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
            let commander = HttpNodeCommander::new(Duration::from_secs(10));

            let mut coordinator =
                DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);

            // Wire external signal source
            if let Some(poller) = HttpSignalPoller::from_config(
                external_metrics_url.as_deref(),
                external_metrics_field.as_deref(),
            ) {
                coordinator.set_signal_source(poller.into_source());
            }

            coordinator.on_progress(|update| {
                eprint!(
                    "\r  [{:.1}s] {:.0} RPS target | {} requests | {} errors | {} nodes",
                    update.elapsed.as_secs_f64(),
                    update.target_rps,
                    update.total_requests,
                    update.total_errors,
                    update.active_nodes,
                );
            });

            let result = coordinator.run();
            eprintln!(); // newline after progress

            tracing::info!(
                total_requests = result.total_requests,
                total_errors = result.total_errors,
                duration_secs = result.duration.as_secs_f64(),
                avg_rps = result.total_requests as f64 / result.duration.as_secs_f64(),
                "distributed test complete"
            );
            for (id, metrics) in &result.nodes {
                tracing::info!(
                    node = %id,
                    requests = metrics.total_requests,
                    rps = metrics.current_rps,
                    p99_ms = metrics.latency_p99_ms,
                    "node result"
                );
            }
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

    #[test]
    fn parse_count_distribution_fixed() {
        let d = parse_count_distribution("fixed:100").unwrap();
        assert!(matches!(d, CountDistribution::Fixed(100)));
    }

    #[test]
    fn parse_count_distribution_uniform() {
        let d = parse_count_distribution("uniform:50,200").unwrap();
        assert!(matches!(d, CountDistribution::Uniform { min: 50, max: 200 }));
    }

    #[test]
    fn parse_count_distribution_normal() {
        let d = parse_count_distribution("normal:100,20").unwrap();
        match d {
            CountDistribution::Normal { mean, stddev } => {
                assert!((mean - 100.0).abs() < 0.01);
                assert!((stddev - 20.0).abs() < 0.01);
            }
            _ => panic!("expected Normal"),
        }
    }

    #[test]
    fn parse_count_distribution_invalid() {
        assert!(parse_count_distribution("bogus:1").is_err());
        assert!(parse_count_distribution("nocolon").is_err());
    }

    #[test]
    fn parse_connection_policy_variants() {
        assert!(matches!(
            parse_connection_policy("keepalive", 0.7, None).unwrap(),
            ConnectionPolicy::KeepAlive
        ));
        assert!(matches!(
            parse_connection_policy("always-new", 0.7, None).unwrap(),
            ConnectionPolicy::AlwaysNew
        ));
        let mixed = parse_connection_policy("mixed", 0.7, Some("fixed:100")).unwrap();
        match mixed {
            ConnectionPolicy::Mixed { persistent_ratio, connection_lifetime } => {
                assert!((persistent_ratio - 0.7).abs() < 0.01);
                assert!(matches!(connection_lifetime, Some(CountDistribution::Fixed(100))));
            }
            _ => panic!("expected Mixed"),
        }
    }
}
