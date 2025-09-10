use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use netanvil_api::{AgentServer, ControlServer, SharedState};
use netanvil_core::{report::ProgressLine, GenericTestBuilder, TestBuilder};
use netanvil_distributed::{
    DistributedCoordinator, HttpMetricsFetcher, HttpNodeCommander, HttpSignalPoller,
    MtlsMetricsFetcher, MtlsNodeCommander, MtlsStaticDiscovery, StaticDiscovery,
};
use netanvil_http::HttpExecutor;
use netanvil_types::{
    ConnectionConfig, ConnectionPolicy, CountDistribution, PidTarget, RateConfig, SchedulerConfig,
    TargetMetric, TestConfig,
};

mod compat;
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
        #[arg(long, required_unless_present = "plugin", num_args = 1..)]
        url: Vec<String>,

        /// Path to a plugin script (.lua) or WASM module (.wasm).
        /// The plugin implements request generation logic.
        #[arg(long)]
        plugin: Option<String>,

        /// Plugin type: "hybrid" (Lua config → native hot path),
        /// "lua" (LuaJIT per-request), or "wasm" (WASM per-request).
        /// Auto-detected from file extension if omitted.
        #[arg(long, default_value = "auto")]
        plugin_type: String,

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

        /// PID proportional gain (omit for autotuning)
        #[arg(long)]
        pid_kp: Option<f64>,

        /// PID integral gain (omit for autotuning)
        #[arg(long)]
        pid_ki: Option<f64>,

        /// PID derivative gain (omit for autotuning)
        #[arg(long)]
        pid_kd: Option<f64>,

        /// PID minimum RPS
        #[arg(long, default_value = "10")]
        pid_min_rps: f64,

        /// PID maximum RPS
        #[arg(long, default_value = "10000")]
        pid_max_rps: f64,

        /// PID constraint for composite mode. Repeatable.
        /// Format: "metric < value" (e.g. "latency-p99 < 500", "error-rate < 2",
        /// "external:load < 80"). When specified, finds the max RPS where ALL
        /// constraints are satisfied.
        #[arg(long = "pid-constraint", num_args = 1)]
        pid_constraints: Vec<String>,

        /// Autotuning exploration duration (e.g. "3s", "5s").
        /// Only used when PID gains are not manually specified.
        #[arg(long, default_value = "3s")]
        pid_autotune_duration: String,

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

        /// Bandwidth limit for modem speed simulation. Throttles TCP reads to
        /// create real server-side backpressure. Accepts human-friendly values:
        /// "56k" (56 kbps), "384k", "1m" (1 Mbps), "10m", or raw bps "56000".
        #[arg(long)]
        bandwidth: Option<String>,

        // ── TCP/UDP-specific flags ──
        /// Payload as UTF-8 string (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload: Option<String>,

        /// Payload as hex-encoded bytes (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload_hex: Option<String>,

        /// Payload from file (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload_file: Option<String>,

        /// TCP framing mode: "raw", "length-prefix:1|2|4", "delimiter", "fixed:N"
        #[arg(long, default_value = "raw")]
        framing: String,

        /// Delimiter bytes for TCP delimiter framing (default: "\r\n")
        #[arg(long, default_value = r"\r\n")]
        delimiter: String,

        /// Don't wait for a response (fire-and-forget mode for TCP/UDP)
        #[arg(long)]
        no_response: bool,

        /// Test mode for TCP/UDP: "echo" (default), "rr", "stream", "maerts", "bidir"
        #[arg(long, default_value = "echo")]
        mode: String,

        /// Request payload size in bytes for RR protocol mode.
        /// Overrides --payload length; pads or truncates to this size.
        #[arg(long)]
        request_size: Option<u16>,

        /// Response payload size in bytes for RR/SOURCE/BIDIR protocol modes.
        #[arg(long)]
        response_size: Option<u32>,

        /// Chunk size in bytes for stream modes (default: 65536)
        #[arg(long, default_value = "65536")]
        chunk_size: usize,

        // ── DNS-specific flags ──
        /// Comma-separated domain names to query (for dns:// protocol)
        #[arg(long)]
        dns_domains: Option<String>,

        /// DNS query type: A, AAAA, MX, CNAME, TXT, NS, SOA, PTR, SRV, ANY
        #[arg(long, default_value = "A")]
        dns_query_type: String,

        // ── Ramp mode flags ──
        /// Duration of warmup phase to learn baseline latency (e.g. "10s")
        #[arg(long, default_value = "10s")]
        ramp_warmup: String,

        /// Acceptable latency multiplier over baseline.
        /// E.g., 3.0 means "p99 can be 3x the warmup baseline before backing off"
        #[arg(long, default_value = "3.0")]
        ramp_multiplier: f64,

        /// Maximum error rate (%) before forcing rate reduction in ramp mode
        #[arg(long, default_value = "5.0")]
        ramp_max_errors: f64,
    },

    /// Run as a remotely controllable agent node
    Agent {
        /// Port to listen on
        #[arg(long, default_value = "9090")]
        listen: u16,

        /// Number of worker cores (0 = auto-detect)
        #[arg(long, default_value = "0")]
        cores: usize,

        /// Path to CA certificate PEM (enables mTLS)
        #[arg(long)]
        tls_ca: Option<String>,

        /// Path to this agent's certificate PEM
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to this agent's private key PEM
        #[arg(long)]
        tls_key: Option<String>,
    },

    /// Run as distributed test leader, coordinating agent nodes
    Leader {
        /// Agent addresses (host:port), comma-separated
        #[arg(long, required = true, value_delimiter = ',')]
        workers: Vec<String>,

        /// Target URL(s)
        #[arg(long, required = true, num_args = 1..)]
        url: Vec<String>,

        /// Path to a plugin script (.lua) or WASM module (.wasm).
        /// The plugin is read locally and sent to all agents.
        #[arg(long)]
        plugin: Option<String>,

        /// Plugin type: "hybrid", "lua", or "wasm". Auto-detected from extension if omitted.
        #[arg(long, default_value = "auto")]
        plugin_type: String,

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

        /// PID proportional gain (omit for autotuning)
        #[arg(long)]
        pid_kp: Option<f64>,

        /// PID integral gain (omit for autotuning)
        #[arg(long)]
        pid_ki: Option<f64>,

        /// PID derivative gain (omit for autotuning)
        #[arg(long)]
        pid_kd: Option<f64>,

        /// PID minimum RPS
        #[arg(long, default_value = "10")]
        pid_min_rps: f64,

        /// PID maximum RPS
        #[arg(long, default_value = "10000")]
        pid_max_rps: f64,

        /// PID constraint for composite mode. Repeatable.
        #[arg(long = "pid-constraint", num_args = 1)]
        pid_constraints: Vec<String>,

        /// Autotuning exploration duration
        #[arg(long, default_value = "3s")]
        pid_autotune_duration: String,

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

        /// Path to CA certificate PEM (enables mTLS for agent communication)
        #[arg(long)]
        tls_ca: Option<String>,

        /// Path to leader's certificate PEM
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to leader's private key PEM
        #[arg(long)]
        tls_key: Option<String>,

        // ── TCP/UDP-specific flags (same as Test command) ──
        /// Payload as UTF-8 string (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload: Option<String>,

        /// Payload as hex-encoded bytes (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload_hex: Option<String>,

        /// Payload from file (for tcp:// and udp:// protocols)
        #[arg(long)]
        payload_file: Option<String>,

        /// TCP framing mode: "raw", "length-prefix:1|2|4", "delimiter", "fixed:N"
        #[arg(long, default_value = "raw")]
        framing: String,

        /// Delimiter bytes for TCP delimiter framing (default: "\r\n")
        #[arg(long, default_value = r"\r\n")]
        delimiter: String,

        /// Don't wait for a response (fire-and-forget mode for TCP/UDP)
        #[arg(long)]
        no_response: bool,

        /// Test mode for TCP/UDP: "echo" (default), "rr", "stream", "maerts", "bidir"
        #[arg(long, default_value = "echo")]
        mode: String,

        /// Request payload size in bytes for RR protocol mode
        #[arg(long)]
        request_size: Option<u16>,

        /// Response payload size in bytes for RR/SOURCE/BIDIR protocol modes
        #[arg(long)]
        response_size: Option<u32>,

        /// Chunk size in bytes for stream modes (default: 65536)
        #[arg(long, default_value = "65536")]
        chunk_size: usize,

        // ── DNS-specific flags ──
        /// Comma-separated domain names to query (for dns:// protocol)
        #[arg(long)]
        dns_domains: Option<String>,

        /// DNS query type: A, AAAA, MX, CNAME, TXT, NS, SOA, PTR, SRV, ANY
        #[arg(long, default_value = "A")]
        dns_query_type: String,

        // ── Ramp mode flags ──
        /// Duration of warmup phase to learn baseline latency (e.g. "10s")
        #[arg(long, default_value = "10s")]
        ramp_warmup: String,

        /// Acceptable latency multiplier over baseline.
        /// E.g., 3.0 means "p99 can be 3x the warmup baseline before backing off"
        #[arg(long, default_value = "3.0")]
        ramp_multiplier: f64,

        /// Maximum error rate (%) before forcing rate reduction in ramp mode
        #[arg(long, default_value = "5.0")]
        ramp_max_errors: f64,
    },
}

/// Run in legacy compatibility mode.
fn REDACTED_FN(args: Vec<String>) -> Result<()> {
    let compat_result = compat::run_compat(args)?;
    let config = compat_result.test_config;
    let quiet = compat_result.quiet;

    // Apply debug level to tracing (reinitialize if needed)
    if let Some(level) = compat_result.debug_level {
        let filter = match level {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        };
        // Set RUST_LOG for any further tracing initialization
        std::env::set_var("RUST_LOG", filter);
    }

    if !quiet {
        eprintln!(
            "netanvil-rs compat mode: {} target(s), {:.1} RPS, {:?} duration, {} cores{}",
            config.targets.len(),
            match &config.rate {
                RateConfig::Static { rps } => *rps,
                RateConfig::Step { steps } => steps.first().map(|s| s.1).unwrap_or(0.0),
                _ => 0.0,
            },
            config.duration,
            if config.num_cores == 0 {
                "auto".to_string()
            } else {
                config.num_cores.to_string()
            },
            if let Some(warmup) = config.warmup_duration {
                format!(", warmup {warmup:?}")
            } else {
                String::new()
            },
        );
    }

    let request_timeout = config.connections.request_timeout;
    let bandwidth_bps = config.bandwidth_limit_bps;
    let tls_client = config.tls_client.clone();
    let drop_fraction = compat_result.drop_fraction;
    let load_feedback = compat_result.load_feedback;
    let targets = config.targets.clone();

    let make_executor = move || {
        let executor = match &tls_client {
            Some(tls_config) => {
                HttpExecutor::with_tls_config(tls_config, bandwidth_bps, request_timeout)
                    .expect("TLS configuration error")
            }
            None => match bandwidth_bps {
                Some(bps) => HttpExecutor::with_bandwidth_limit(bps, request_timeout),
                None => HttpExecutor::with_timeout(request_timeout),
            },
        };
        match drop_fraction {
            Some(frac) => {
                let stack_var: u32 = 0;
                let seed = (&stack_var as *const u32 as u32).wrapping_mul(2654435761);
                netanvil_core::dropping::DroppingExecutor::new(executor, frac, seed)
            }
            None => netanvil_core::dropping::DroppingExecutor::new(executor, 0.0, 1),
        }
    };

    // If load feedback is configured, we need a control API for the sidecar
    // to push signals into. Otherwise, run without the API.
    let result = if let Some(ref lf_config) = load_feedback {
        // Start control API on an ephemeral port
        let shared_state = SharedState::new();
        let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

        // Try ports starting from 19100 to find an available one
        let mut api_port = 19100u16;
        let server = loop {
            match ControlServer::new(api_port, shared_state.clone(), ext_cmd_tx.clone()) {
                Ok(s) => break s,
                Err(_) => {
                    api_port += 1;
                    if api_port > 19200 {
                        anyhow::bail!(
                            "could not find available port for control API (tried 19100-19200)"
                        );
                    }
                }
            }
        };
        let _server_handle = server.spawn();

        if !quiet {
            eprintln!("  load feedback: control API on port {api_port}, spawning REDACTED-CRATE");
        }

        // Spawn REDACTED-CRATE sidecar
        let mut sidecar_cmd = std::process::Command::new("REDACTED-CRATE");
        sidecar_cmd
            .arg("--target-api")
            .arg(format!("http://127.0.0.1:{api_port}"))
            .arg("--source")
            .arg("redacted_proto")
            .arg("--redacted_proto-ip")
            .arg(&lf_config.feedback_ip)
            .arg("--redacted_proto-service")
            .arg(&lf_config.service)
            .arg("--period")
            .arg(lf_config.period.to_string());

        let mut sidecar_child = sidecar_cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("failed to spawn REDACTED-CRATE sidecar (is it in PATH?)")?;

        tracing::info!(
            feedback_ip = %lf_config.feedback_ip,
            service = %lf_config.service,
            period = lf_config.period,
            "REDACTED-CRATE sidecar started"
        );

        // Wire up signal source and external commands
        let progress_state = shared_state.clone();
        let signal_state = shared_state.clone();
        let mut builder = TestBuilder::new(config, make_executor)
            .external_commands(ext_cmd_rx)
            .pushed_signal_source(move || signal_state.drain_pushed_signals());

        if !quiet {
            builder = builder.on_progress(move |update| {
                progress_state.update_from_progress(update);
                eprint!("\r{}", netanvil_core::report::ProgressLine::new(update));
            });
        }

        // Set up plugin generator
        if let Some(plugin_config) = compat_result.plugin_config {
            let script = String::from_utf8(plugin_config.source)
                .map_err(|_| anyhow::anyhow!("generated Lua script is not valid UTF-8"))?;
            let targets_for_factory = targets.clone();
            builder = builder.generator_factory(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::new(&script, &targets_for_factory)
                        .expect("LuaJIT generator init failed"),
                )
                    as Box<dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }));
        }

        let result = builder.run().context("load test failed")?;

        // Kill sidecar
        let _ = sidecar_child.kill();
        let _ = sidecar_child.wait();
        tracing::info!("REDACTED-CRATE sidecar stopped");

        result
    } else {
        // No load feedback — standard execution path
        let mut builder = TestBuilder::new(config, make_executor);

        // Progress callback with optional outlog
        if !quiet {
            let mut outlog_file = compat_result.outlog.map(|path| {
                std::fs::File::create(&path)
                    .unwrap_or_else(|e| panic!("cannot create outlog file '{path}': {e}"))
            });
            let lines_interval = compat_result.lines.unwrap_or(20);
            let mut line_counter: u32 = 0;

            builder = builder.on_progress(move |update| {
                let line = format!("{}", netanvil_core::report::ProgressLine::new(update));
                if line_counter % lines_interval == 0 {
                    // Header re-print interval
                }
                eprint!("\r{line}");
                if let Some(ref mut f) = outlog_file {
                    use std::io::Write;
                    let _ = writeln!(f, "{line}");
                }
                line_counter += 1;
            });
        }

        // Set up plugin generator
        if let Some(plugin_config) = compat_result.plugin_config {
            let script = String::from_utf8(plugin_config.source)
                .map_err(|_| anyhow::anyhow!("generated Lua script is not valid UTF-8"))?;
            let targets_for_factory = targets.clone();
            builder = builder.generator_factory(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::new(&script, &targets_for_factory)
                        .expect("LuaJIT generator init failed"),
                )
                    as Box<dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }));
        }

        builder.run().context("load test failed")?
    };

    if !quiet {
        eprintln!();
    }

    let output_format = output::OutputFormat::Text;
    output::print_results(&result, output_format);

    Ok(())
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
    // Check for "external:name" prefix first
    if let Some(name) = s.strip_prefix("external:") {
        return Ok(TargetMetric::External {
            name: name.to_string(),
        });
    }
    match s.to_lowercase().as_str() {
        "latency-p50" | "p50" => Ok(TargetMetric::LatencyP50),
        "latency-p90" | "p90" => Ok(TargetMetric::LatencyP90),
        "latency-p99" | "p99" => Ok(TargetMetric::LatencyP99),
        "error-rate" | "errors" => Ok(TargetMetric::ErrorRate),
        "throughput-send" | "throughput" | "send-mbps" => Ok(TargetMetric::ThroughputSend),
        "throughput-recv" | "recv-mbps" => Ok(TargetMetric::ThroughputRecv),
        other => anyhow::bail!(
            "unknown PID metric: {other} (use 'latency-p50', 'latency-p90', 'latency-p99', \
             'error-rate', 'throughput-send', 'throughput-recv', or 'external:<name>')"
        ),
    }
}

/// Parse a PID constraint string: "metric < value" or "metric > value".
/// Only '<' (upper limit) is supported for now.
fn parse_pid_constraint(s: &str) -> Result<netanvil_types::PidConstraint> {
    let parts: Vec<&str> = s.split('<').collect();
    if parts.len() != 2 {
        anyhow::bail!(
            "constraint must be in 'metric < value' format (e.g. 'latency-p99 < 500'), got: {s}"
        );
    }
    let metric = parse_target_metric(parts[0].trim())?;
    let limit: f64 = parts[1]
        .trim()
        .parse()
        .context("invalid numeric limit in constraint")?;
    Ok(netanvil_types::PidConstraint {
        metric,
        limit,
        gains: netanvil_types::PidGains::default(),
    })
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
            let lifetime = conn_lifetime.map(parse_count_distribution).transpose()?;
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
            let n: u32 = params
                .trim()
                .parse()
                .context("fixed:N requires an integer")?;
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
        other => {
            anyhow::bail!("unknown distribution: {other} (use 'fixed', 'uniform', or 'normal')")
        }
    }
}

/// Parse a human-friendly bandwidth string into bits per second.
///
/// Accepts:
/// - "56k" or "56K" → 56,000 bps
/// - "1m" or "1M" → 1,000,000 bps
/// - "10m" → 10,000,000 bps
/// - "1g" or "1G" → 1,000,000,000 bps
/// - "56000" → 56,000 bps (raw number)
/// - "1.5m" → 1,500,000 bps (fractional)
fn parse_bandwidth(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty bandwidth string");
    }

    let (num_str, multiplier) = if let Some(n) = s.strip_suffix(['k', 'K']) {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix("kbps") {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix(['m', 'M']) {
        (n, 1_000_000u64)
    } else if let Some(n) = s.strip_suffix("mbps") {
        (n, 1_000_000u64)
    } else if let Some(n) = s.strip_suffix(['g', 'G']) {
        (n, 1_000_000_000u64)
    } else if let Some(n) = s.strip_suffix("gbps") {
        (n, 1_000_000_000u64)
    } else {
        (s, 1u64)
    };

    let num: f64 = num_str
        .trim()
        .parse()
        .context(format!("invalid bandwidth number: '{num_str}'"))?;
    let bps = (num * multiplier as f64) as u64;
    if bps == 0 {
        anyhow::bail!("bandwidth must be > 0");
    }
    Ok(bps)
}

/// Format bandwidth in human-readable form.
fn format_bandwidth(bps: u64) -> String {
    if bps >= 1_000_000_000 {
        format!("{:.1} Gbps", bps as f64 / 1_000_000_000.0)
    } else if bps >= 1_000_000 {
        format!("{:.1} Mbps", bps as f64 / 1_000_000.0)
    } else if bps >= 1_000 {
        format!("{:.0} kbps", bps as f64 / 1_000.0)
    } else {
        format!("{bps} bps")
    }
}

/// Detect plugin type from file extension or explicit --plugin-type flag.
fn detect_plugin_type(path: &str, explicit: &str) -> Result<PluginType> {
    if explicit != "auto" {
        return match explicit.to_lowercase().as_str() {
            "hybrid" => Ok(PluginType::Hybrid),
            "lua" | "luajit" => Ok(PluginType::Lua),
            "wasm" => Ok(PluginType::Wasm),
            other => {
                anyhow::bail!("unknown --plugin-type: {other} (use 'hybrid', 'lua', or 'wasm')")
            }
        };
    }
    // Auto-detect from extension
    if path.ends_with(".wasm") {
        Ok(PluginType::Wasm)
    } else if path.ends_with(".lua") {
        Ok(PluginType::Lua)
    } else {
        anyhow::bail!("cannot auto-detect plugin type for '{path}'. Use --plugin-type to specify.")
    }
}

use netanvil_types::PluginType;

/// Build a generator factory from a plugin file.
///
/// Returns a closure suitable for `TestBuilder::generator_factory()`.
/// Each call creates a new generator instance for one core.
fn build_plugin_factory(
    plugin_path: &str,
    plugin_type: PluginType,
    targets: &[String],
) -> Result<netanvil_core::GeneratorFactory> {
    match plugin_type {
        PluginType::Hybrid => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let config = netanvil_plugin_luajit::config_from_lua(&script)
                .map_err(|e| anyhow::anyhow!("hybrid config error: {e}"))?;
            Ok(Box::new(move |_core_id| {
                Box::new(netanvil_plugin::HybridGenerator::new(config.clone()))
                    as Box<dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }))
        }
        PluginType::Lua => {
            let script = std::fs::read_to_string(plugin_path)
                .context(format!("failed to read plugin: {plugin_path}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin_luajit::LuaJitGenerator::new(&script, &targets)
                        .expect("LuaJIT generator init failed"),
                )
                    as Box<dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }))
        }
        PluginType::Wasm => {
            let wasm_bytes = std::fs::read(plugin_path)
                .context(format!("failed to read WASM module: {plugin_path}"))?;
            let (engine, module) = netanvil_plugin::compile_wasm_module(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("WASM compile error: {e}"))?;
            let targets = targets.to_vec();
            Ok(Box::new(move |_core_id| {
                Box::new(
                    netanvil_plugin::WasmGenerator::new(&engine, &module, &targets)
                        .expect("WASM generator init failed"),
                )
                    as Box<dyn netanvil_types::RequestGenerator<Spec = netanvil_types::HttpRequestSpec>>
            }))
        }
    }
}

fn build_pid_gains(
    pid_kp: Option<f64>,
    pid_ki: Option<f64>,
    pid_kd: Option<f64>,
    autotune_duration: Duration,
) -> netanvil_types::PidGains {
    match (pid_kp, pid_ki, pid_kd) {
        (Some(kp), Some(ki), Some(kd)) => netanvil_types::PidGains::Manual { kp, ki, kd },
        (None, None, None) => netanvil_types::PidGains::Auto {
            autotune_duration,
            smoothing: 0.3,
        },
        // Partial specification: fill missing with defaults
        (kp, ki, kd) => netanvil_types::PidGains::Manual {
            kp: kp.unwrap_or(0.1),
            ki: ki.unwrap_or(0.01),
            kd: kd.unwrap_or(0.05),
        },
    }
}

/// PID-specific arguments for building a rate config.
struct PidArgs<'a> {
    metric: &'a str,
    target: f64,
    kp: Option<f64>,
    ki: Option<f64>,
    kd: Option<f64>,
    min_rps: f64,
    max_rps: f64,
    constraints: &'a [String],
    autotune_duration: Duration,
}

/// Ramp-specific CLI arguments.
struct RampArgs {
    warmup_duration: Duration,
    latency_multiplier: f64,
    max_error_rate: f64,
}

fn build_rate_config(
    rate_mode: &str,
    rps: f64,
    steps: Option<&str>,
    pid: &PidArgs<'_>,
    ramp: &RampArgs,
) -> Result<RateConfig> {
    match rate_mode.to_lowercase().as_str() {
        "static" | "const" => Ok(RateConfig::Static { rps }),
        "step" => {
            let steps_str = steps.context("--steps required for step rate mode")?;
            let steps = parse_steps(steps_str)?;
            Ok(RateConfig::Step { steps })
        }
        "pid" => {
            if !pid.constraints.is_empty() {
                let mut constraints: Vec<netanvil_types::PidConstraint> = pid
                    .constraints
                    .iter()
                    .map(|s| parse_pid_constraint(s))
                    .collect::<Result<_>>()?;

                let gains = build_pid_gains(pid.kp, pid.ki, pid.kd, pid.autotune_duration);
                if matches!(gains, netanvil_types::PidGains::Manual { .. }) {
                    for c in &mut constraints {
                        c.gains = gains.clone();
                    }
                }

                Ok(RateConfig::CompositePid {
                    initial_rps: rps,
                    constraints,
                    min_rps: pid.min_rps,
                    max_rps: pid.max_rps,
                })
            } else {
                let metric = parse_target_metric(pid.metric)?;
                let gains = build_pid_gains(pid.kp, pid.ki, pid.kd, pid.autotune_duration);
                Ok(RateConfig::Pid {
                    initial_rps: rps,
                    target: PidTarget {
                        metric,
                        value: pid.target,
                        gains,
                        min_rps: pid.min_rps,
                        max_rps: pid.max_rps,
                    },
                })
            }
        }
        "ramp" => Ok(RateConfig::Ramp {
            warmup_rps: rps.clamp(1.0, 100.0), // warmup at low rate (capped at 100)
            warmup_duration: ramp.warmup_duration,
            latency_multiplier: ramp.latency_multiplier,
            max_error_rate: ramp.max_error_rate,
            min_rps: pid.min_rps,
            max_rps: pid.max_rps,
        }),
        other => {
            anyhow::bail!("unknown rate mode: {other} (use 'static', 'step', 'pid', or 'ramp')")
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    // Binary name detection: if invoked as "netanvil" or "redacted_client", enter compat mode.
    let args: Vec<String> = std::env::args().collect();
    let bin_name = std::path::Path::new(&args[0])
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("netanvil-rs");

    if bin_name == "netanvil" || bin_name == "redacted_client" {
        return REDACTED_FN(args);
    }

    // Check for explicit "compat" subcommand before clap parsing
    if args.len() > 1 && args[1] == "compat" {
        // Pass remaining args as if they were the full argv for legacy parsing
        let mut compat_args = vec![args[0].clone()];
        compat_args.extend(args[2..].iter().cloned());
        return REDACTED_FN(compat_args);
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Test {
            url,
            plugin,
            plugin_type,
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
            pid_constraints,
            pid_autotune_duration,
            connection_policy,
            persistent_ratio,
            conn_lifetime,
            output,
            api_port,
            bandwidth,
            payload,
            payload_hex,
            payload_file,
            framing,
            delimiter,
            no_response,
            mode,
            request_size,
            response_size,
            chunk_size: _,
            dns_domains,
            dns_query_type,
            ramp_warmup,
            ramp_multiplier,
            ramp_max_errors,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;
            let bandwidth_bps = bandwidth
                .as_deref()
                .map(parse_bandwidth)
                .transpose()
                .context("invalid --bandwidth")?;
            let autotune_dur = parse_duration(&pid_autotune_duration)
                .context("invalid --pid-autotune-duration")?;
            let scheduler = parse_scheduler(&scheduler).context("invalid --scheduler")?;
            let ramp_warmup_dur = parse_duration(&ramp_warmup).context("invalid --ramp-warmup")?;
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|h| parse_header(h))
                .collect::<Result<_>>()?;
            let rate = build_rate_config(
                &rate_mode,
                rps,
                steps.as_deref(),
                &PidArgs {
                    metric: &pid_metric,
                    target: pid_target,
                    kp: pid_kp,
                    ki: pid_ki,
                    kd: pid_kd,
                    min_rps: pid_min_rps,
                    max_rps: pid_max_rps,
                    constraints: &pid_constraints,
                    autotune_duration: autotune_dur,
                },
                &RampArgs {
                    warmup_duration: ramp_warmup_dur,
                    latency_multiplier: ramp_multiplier,
                    max_error_rate: ramp_max_errors,
                },
            )?;

            let conn_policy = parse_connection_policy(
                &connection_policy,
                persistent_ratio,
                conn_lifetime.as_deref(),
            )?;

            let mut config = TestConfig {
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
                bandwidth_limit_bps: bandwidth_bps,
                ..Default::default()
            };

            let rate_desc = match &config.rate {
                RateConfig::Static { rps } => format!("{rps} RPS (static)"),
                RateConfig::Step { steps } => format!("{} steps", steps.len()),
                RateConfig::Pid {
                    initial_rps,
                    target,
                } => {
                    let mode = match &target.gains {
                        netanvil_types::PidGains::Auto { .. } => "PID/autotune",
                        netanvil_types::PidGains::Manual { .. } => "PID",
                    };
                    format!("{initial_rps} RPS initial ({mode})")
                }
                RateConfig::CompositePid {
                    initial_rps,
                    constraints,
                    ..
                } => format!(
                    "{initial_rps} RPS initial (composite PID, {} constraints)",
                    constraints.len()
                ),
                RateConfig::Ramp {
                    warmup_rps,
                    latency_multiplier,
                    ..
                } => {
                    format!("{warmup_rps} RPS warmup (ramp, {latency_multiplier}x baseline)")
                }
            };

            let bw_desc = match bandwidth_bps {
                Some(bps) => format!(", bandwidth: {}", format_bandwidth(bps)),
                None => String::new(),
            };
            eprintln!(
                "Running load test: {} target(s), {rate_desc}, {:?} duration, {} cores{bw_desc}",
                config.targets.len(),
                duration,
                if cores == 0 {
                    "auto".to_string()
                } else {
                    cores.to_string()
                },
            );

            let request_timeout = timeout;
            let protocol = detect_protocol(&config.targets);

            let result = match protocol {
                DetectedProtocol::Tcp => {
                    let tcp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                        .context("invalid TCP payload")?;
                    let tcp_framing =
                        parse_tcp_framing(&framing, &delimiter).context("invalid --framing")?;
                    let targets = parse_socket_targets(&config.targets, "tcp://")
                        .context("invalid TCP target address")?;
                    let tcp_mode = parse_tcp_mode(&mode).context("invalid --mode")?;
                    let expect_response = !no_response;

                    // Determine payload and sizes based on mode
                    let req_size = request_size.unwrap_or(tcp_payload.len() as u16);
                    let resp_size = response_size.unwrap_or(req_size as u32);

                    eprintln!(
                        "  protocol: TCP, mode: {mode}, payload: {} bytes, request: {req_size}, response: {resp_size}",
                        tcp_payload.len()
                    );

                    config.error_status_threshold = 0; // no HTTP status for TCP
                    config.protocol = Some(netanvil_types::ProtocolConfig::Tcp {
                        mode: mode.clone(),
                        payload_hex: encode_hex(&tcp_payload),
                        framing: framing.clone(),
                        request_size: req_size,
                        response_size: resp_size,
                    });

                    let conn_policy = config.connections.connection_policy.clone();
                    let max_conns = config.connections.max_connections_per_core;

                    let gen_factory: netanvil_core::GenericGeneratorFactory<
                        netanvil_tcp::TcpRequestSpec,
                    > = Box::new(move |_core_id| {
                        Box::new(
                            netanvil_tcp::SimpleTcpGenerator::new(
                                targets.clone(),
                                tcp_payload.clone(),
                                tcp_framing.clone(),
                                expect_response,
                            )
                            .with_mode(tcp_mode)
                            .with_request_size(req_size)
                            .with_response_size(resp_size),
                        )
                    });
                    let trans_factory: netanvil_core::GenericTransformerFactory<
                        netanvil_tcp::TcpRequestSpec,
                    > = Box::new(|_| Box::new(netanvil_tcp::TcpNoopTransformer));

                    GenericTestBuilder::new(
                        config,
                        move || {
                            netanvil_tcp::TcpExecutor::with_pool(
                                request_timeout,
                                max_conns,
                                conn_policy.clone(),
                            )
                        },
                        gen_factory,
                        trans_factory,
                    )
                    .on_progress(|update| {
                        eprint!("\r{}", ProgressLine::new(update));
                    })
                    .run()
                    .context("TCP load test failed")?
                }

                DetectedProtocol::Udp => {
                    let udp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                        .context("invalid UDP payload")?;
                    let targets = parse_socket_targets(&config.targets, "udp://")
                        .context("invalid UDP target address")?;
                    let expect_response = !no_response;

                    eprintln!(
                        "  protocol: UDP, {} byte payload, response: {expect_response}",
                        udp_payload.len()
                    );

                    config.error_status_threshold = 0; // no HTTP status for UDP
                    config.protocol = Some(netanvil_types::ProtocolConfig::Udp {
                        payload_hex: encode_hex(&udp_payload),
                        expect_response,
                    });

                    let gen_factory: netanvil_core::GenericGeneratorFactory<
                        netanvil_udp::UdpRequestSpec,
                    > = Box::new(move |_core_id| {
                        Box::new(netanvil_udp::SimpleUdpGenerator::new(
                            targets.clone(),
                            udp_payload.clone(),
                            expect_response,
                        ))
                    });
                    let trans_factory: netanvil_core::GenericTransformerFactory<
                        netanvil_udp::UdpRequestSpec,
                    > = Box::new(|_| Box::new(netanvil_udp::UdpNoopTransformer));

                    GenericTestBuilder::new(
                        config,
                        move || netanvil_udp::UdpExecutor::with_timeout(request_timeout),
                        gen_factory,
                        trans_factory,
                    )
                    .on_progress(|update| {
                        eprint!("\r{}", ProgressLine::new(update));
                    })
                    .run()
                    .context("UDP load test failed")?
                }

                DetectedProtocol::Dns => {
                    let servers = parse_socket_targets(&config.targets, "dns://")
                        .context("invalid DNS target address")?;
                    let domains: Vec<String> = dns_domains
                        .as_deref()
                        .unwrap_or("example.com")
                        .split(',')
                        .map(|s: &str| s.trim().to_string())
                        .collect();
                    let query_type = netanvil_dns::DnsQueryType::from_str_name(&dns_query_type)
                        .ok_or_else(|| {
                            anyhow::anyhow!("unknown DNS query type: {dns_query_type}")
                        })?;

                    eprintln!(
                        "  protocol: DNS, query_type: {:?}, domains: {}",
                        query_type,
                        domains.len()
                    );

                    config.error_status_threshold = 0;

                    let gen_factory: netanvil_core::GenericGeneratorFactory<
                        netanvil_dns::DnsRequestSpec,
                    > = Box::new(move |_core_id| {
                        Box::new(netanvil_dns::SimpleDnsGenerator::new(
                            servers.clone(),
                            domains.clone(),
                            query_type,
                            true,
                        ))
                    });
                    let trans_factory: netanvil_core::GenericTransformerFactory<
                        netanvil_dns::DnsRequestSpec,
                    > = Box::new(|_| Box::new(netanvil_dns::DnsNoopTransformer));

                    GenericTestBuilder::new(
                        config,
                        move || netanvil_dns::DnsExecutor::with_timeout(request_timeout),
                        gen_factory,
                        trans_factory,
                    )
                    .on_progress(|update| {
                        eprint!("\r{}", ProgressLine::new(update));
                    })
                    .run()
                    .context("DNS load test failed")?
                }

                DetectedProtocol::Http => {
                    // Build executor factory — with TLS config, bandwidth throttling, or default
                    let tls_client = config.tls_client.clone();
                    let make_executor = move || -> HttpExecutor {
                        match &tls_client {
                            Some(tls_config) => HttpExecutor::with_tls_config(
                                tls_config,
                                bandwidth_bps,
                                request_timeout,
                            )
                            .expect("TLS configuration error"),
                            None => match bandwidth_bps {
                                Some(bps) => {
                                    HttpExecutor::with_bandwidth_limit(bps, request_timeout)
                                }
                                None => HttpExecutor::with_timeout(request_timeout),
                            },
                        }
                    };

                    // Build plugin generator factory if --plugin is specified
                    let plugin_factory = if let Some(ref plugin_path) = plugin {
                        let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                        let factory = build_plugin_factory(plugin_path, ptype, &config.targets)?;
                        tracing::info!(
                            plugin = %plugin_path,
                            plugin_type = ?ptype,
                            "loaded plugin generator"
                        );
                        Some(factory)
                    } else {
                        None
                    };

                    if let Some(port) = api_port {
                        let shared_state = SharedState::new();
                        let (ext_cmd_tx, ext_cmd_rx) = flume::unbounded();

                        let server = ControlServer::new(port, shared_state.clone(), ext_cmd_tx)
                            .context(format!("failed to start API server on port {port}"))?;
                        let _server_handle = server.spawn();
                        tracing::info!(port, "control API listening");

                        let progress_state = shared_state.clone();
                        let signal_state = shared_state.clone();
                        let mut builder = TestBuilder::new(config, make_executor)
                            .on_progress(move |update| {
                                progress_state.update_from_progress(update);
                                eprint!("\r{}", ProgressLine::new(update));
                            })
                            .external_commands(ext_cmd_rx)
                            .pushed_signal_source(move || signal_state.drain_pushed_signals());

                        if let Some(factory) = plugin_factory {
                            builder = builder.generator_factory(factory);
                        }

                        builder.run().context("load test failed")?
                    } else {
                        let mut builder =
                            TestBuilder::new(config, make_executor).on_progress(|update| {
                                eprint!("\r{}", ProgressLine::new(update));
                            });

                        if let Some(factory) = plugin_factory {
                            builder = builder.generator_factory(factory);
                        }

                        builder.run().context("load test failed")?
                    }
                }
            };

            eprintln!(); // newline after progress

            let output_format =
                output::OutputFormat::parse(&output).map_err(|e| anyhow::anyhow!(e))?;
            output::print_results(&result, output_format);
        }

        Commands::Agent {
            listen,
            cores,
            tls_ca,
            tls_cert,
            tls_key,
        } => {
            let cores = if cores == 0 {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1)
            } else {
                cores
            };

            let server = if let (Some(ca), Some(cert), Some(key)) = (tls_ca, tls_cert, tls_key) {
                let tls = netanvil_types::TlsConfig {
                    ca_cert: ca,
                    cert,
                    key,
                };
                tracing::info!(port = listen, cores, "starting agent (mTLS)");
                AgentServer::with_tls(listen, cores, &tls)
                    .context(format!("failed to start mTLS agent on port {listen}"))?
            } else {
                tracing::info!(port = listen, cores, "starting agent (plain HTTP)");
                AgentServer::new(listen, cores)
                    .context(format!("failed to start agent on port {listen}"))?
            };
            server.run(); // blocks until Ctrl+C
        }

        Commands::Leader {
            workers,
            url,
            plugin,
            plugin_type,
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
            pid_constraints,
            pid_autotune_duration,
            cores,
            timeout,
            headers,
            error_threshold,
            external_metrics_url,
            external_metrics_field,
            tls_ca,
            tls_cert,
            tls_key,
            payload,
            payload_hex,
            payload_file,
            framing,
            delimiter: _delimiter,
            no_response,
            mode,
            request_size,
            response_size,
            chunk_size: _chunk_size,
            dns_domains: _dns_domains,
            dns_query_type: _dns_query_type,
            ramp_warmup,
            ramp_multiplier,
            ramp_max_errors,
        } => {
            let duration = parse_duration(&duration).context("invalid --duration")?;
            let timeout = parse_duration(&timeout).context("invalid --timeout")?;
            let autotune_dur = parse_duration(&pid_autotune_duration)
                .context("invalid --pid-autotune-duration")?;
            let ramp_warmup_dur = parse_duration(&ramp_warmup).context("invalid --ramp-warmup")?;
            let headers: Vec<(String, String)> = headers
                .iter()
                .map(|h| parse_header(h))
                .collect::<Result<_>>()?;
            let rate = build_rate_config(
                &rate_mode,
                rps,
                steps.as_deref(),
                &PidArgs {
                    metric: &pid_metric,
                    target: pid_target,
                    kp: pid_kp,
                    ki: pid_ki,
                    kd: pid_kd,
                    min_rps: pid_min_rps,
                    max_rps: pid_max_rps,
                    constraints: &pid_constraints,
                    autotune_duration: autotune_dur,
                },
                &RampArgs {
                    warmup_duration: ramp_warmup_dur,
                    latency_multiplier: ramp_multiplier,
                    max_error_rate: ramp_max_errors,
                },
            )?;

            // Build plugin config: read the file locally and embed it for agents
            let plugin_config = if let Some(ref plugin_path) = plugin {
                let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
                let source = std::fs::read(plugin_path)
                    .context(format!("failed to read plugin: {plugin_path}"))?;
                tracing::info!(
                    plugin = %plugin_path,
                    plugin_type = ?ptype,
                    size = source.len(),
                    "embedding plugin for distribution to agents"
                );
                Some(netanvil_types::PluginConfig {
                    plugin_type: ptype,
                    source,
                })
            } else {
                None
            };

            let mut config = TestConfig {
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
                bandwidth_limit_bps: None,
                plugin: plugin_config,
                ..Default::default()
            };

            // Detect protocol from URLs and populate config.protocol for TCP/UDP
            let protocol = detect_protocol(&config.targets);
            match protocol {
                DetectedProtocol::Tcp => {
                    let tcp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                        .context("invalid TCP payload")?;
                    let _tcp_mode = parse_tcp_mode(&mode).context("invalid --mode")?;
                    let req_size = request_size.unwrap_or(tcp_payload.len() as u16);
                    let resp_size = response_size.unwrap_or(req_size as u32);
                    config.protocol = Some(netanvil_types::ProtocolConfig::Tcp {
                        mode: mode.clone(),
                        payload_hex: encode_hex(&tcp_payload),
                        framing: framing.clone(),
                        request_size: req_size,
                        response_size: resp_size,
                    });
                    config.error_status_threshold = 0;
                }
                DetectedProtocol::Udp => {
                    let udp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                        .context("invalid UDP payload")?;
                    config.protocol = Some(netanvil_types::ProtocolConfig::Udp {
                        payload_hex: encode_hex(&udp_payload),
                        expect_response: !no_response,
                    });
                    config.error_status_threshold = 0;
                }
                DetectedProtocol::Dns => {
                    // DNS is handled by agents via SimpleDnsGenerator; no ProtocolConfig yet.
                    // Future: add ProtocolConfig::Dns for distributed DNS tests.
                    config.error_status_threshold = 0;
                }
                DetectedProtocol::Http => {
                    // No change needed — config.protocol stays None (default HTTP)
                }
            }

            tracing::info!(
                agents = workers.len(),
                targets = config.targets.len(),
                rps,
                ?duration,
                plugin = plugin.as_deref().unwrap_or("none"),
                "starting distributed test"
            );

            // Build rate controller
            use netanvil_core::{
                AutotuningPidController, CompositePidController, PidRateController,
                StaticRateController, StepRateController,
            };
            let rate_controller: Box<dyn netanvil_types::RateController> = match &config.rate {
                RateConfig::Static { rps } => Box::new(StaticRateController::new(*rps)),
                RateConfig::Step { steps } => Box::new(StepRateController::with_start_time(
                    steps.clone(),
                    std::time::Instant::now(),
                )),
                RateConfig::Pid {
                    initial_rps,
                    target,
                } => match &target.gains {
                    netanvil_types::PidGains::Manual { kp, ki, kd } => {
                        Box::new(PidRateController::new(
                            target.metric.clone(),
                            target.value,
                            *initial_rps,
                            target.min_rps,
                            target.max_rps,
                            netanvil_core::PidGainValues {
                                kp: *kp,
                                ki: *ki,
                                kd: *kd,
                            },
                        ))
                    }
                    netanvil_types::PidGains::Auto {
                        autotune_duration,
                        smoothing,
                    } => Box::new(AutotuningPidController::new(
                        target.metric.clone(),
                        target.value,
                        *initial_rps,
                        target.min_rps,
                        target.max_rps,
                        netanvil_core::AutotuneParams {
                            autotune_duration: *autotune_duration,
                            smoothing: *smoothing,
                            control_interval: config.control_interval,
                        },
                    )),
                },
                RateConfig::CompositePid {
                    initial_rps,
                    constraints,
                    min_rps,
                    max_rps,
                } => Box::new(CompositePidController::new(
                    constraints,
                    *initial_rps,
                    *min_rps,
                    *max_rps,
                    config.control_interval,
                )),
                RateConfig::Ramp {
                    warmup_rps,
                    warmup_duration,
                    latency_multiplier,
                    max_error_rate,
                    min_rps,
                    max_rps,
                } => Box::new(netanvil_core::RampRateController::new(
                    netanvil_core::RampConfig {
                        warmup_rps: *warmup_rps,
                        warmup_duration: *warmup_duration,
                        latency_multiplier: *latency_multiplier,
                        max_error_rate: *max_error_rate,
                        min_rps: *min_rps,
                        max_rps: *max_rps,
                        control_interval: config.control_interval,
                    },
                )),
            };

            let tls_config = match (&tls_ca, &tls_cert, &tls_key) {
                (Some(ca), Some(cert), Some(key)) => Some(netanvil_types::TlsConfig {
                    ca_cert: ca.clone(),
                    cert: cert.clone(),
                    key: key.clone(),
                }),
                _ => None,
            };

            // Helper to configure and run a coordinator (generic over trait impls).
            fn run_coordinator<D, M, C>(
                mut coordinator: DistributedCoordinator<D, M, C>,
                external_metrics_url: Option<&str>,
                external_metrics_field: Option<&str>,
            ) -> netanvil_distributed::DistributedTestResult
            where
                D: netanvil_types::NodeDiscovery,
                M: netanvil_types::MetricsFetcher,
                C: netanvil_types::NodeCommander,
            {
                if let Some(poller) =
                    HttpSignalPoller::from_config(external_metrics_url, external_metrics_field)
                {
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
                coordinator.run()
            }

            let result = if let Some(ref tls) = tls_config {
                tracing::info!("using mTLS for agent communication");
                let discovery = MtlsStaticDiscovery::new(workers, tls)
                    .map_err(|e| anyhow::anyhow!("mTLS discovery: {e}"))?;
                let fetcher = MtlsMetricsFetcher::new(tls)
                    .map_err(|e| anyhow::anyhow!("mTLS fetcher: {e}"))?;
                let commander = MtlsNodeCommander::new(tls)
                    .map_err(|e| anyhow::anyhow!("mTLS commander: {e}"))?;
                let coordinator = DistributedCoordinator::new(
                    discovery,
                    fetcher,
                    commander,
                    config,
                    rate_controller,
                );
                run_coordinator(
                    coordinator,
                    external_metrics_url.as_deref(),
                    external_metrics_field.as_deref(),
                )
            } else {
                let discovery = StaticDiscovery::new(workers);
                let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
                let commander = HttpNodeCommander::new(Duration::from_secs(10));
                let coordinator = DistributedCoordinator::new(
                    discovery,
                    fetcher,
                    commander,
                    config,
                    rate_controller,
                );
                run_coordinator(
                    coordinator,
                    external_metrics_url.as_deref(),
                    external_metrics_field.as_deref(),
                )
            };
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

// ---------------------------------------------------------------------------
// Hex encoding helpers
// ---------------------------------------------------------------------------

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[allow(dead_code)]
fn decode_hex(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..clean.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&clean[i..i + 2], 16)
                .map_err(|e| anyhow::anyhow!("hex decode at {i}: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Multi-protocol support
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum DetectedProtocol {
    Http,
    Tcp,
    Udp,
    Dns,
}

fn detect_protocol(urls: &[String]) -> DetectedProtocol {
    let first = urls.first().map(|s| s.as_str()).unwrap_or("http://");
    if first.starts_with("tcp://") {
        DetectedProtocol::Tcp
    } else if first.starts_with("udp://") {
        DetectedProtocol::Udp
    } else if first.starts_with("dns://") {
        DetectedProtocol::Dns
    } else {
        DetectedProtocol::Http
    }
}

/// Parse tcp:// or udp:// URLs to SocketAddr, resolving DNS if needed.
fn parse_socket_targets(urls: &[String], scheme: &str) -> Result<Vec<std::net::SocketAddr>> {
    urls.iter()
        .map(|u| {
            let addr_str = u
                .strip_prefix(scheme)
                .ok_or_else(|| anyhow::anyhow!("URL {u} does not start with {scheme}"))?;
            use std::net::ToSocketAddrs;
            addr_str
                .to_socket_addrs()
                .context(format!("failed to resolve {addr_str}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("no addresses found for {addr_str}"))
        })
        .collect()
}

/// Resolve payload from --payload, --payload-hex, or --payload-file flags.
fn resolve_payload(
    text: &Option<String>,
    hex: &Option<String>,
    file: &Option<String>,
) -> Result<Vec<u8>> {
    if let Some(t) = text {
        // Unescape common sequences
        let unescaped = t
            .replace("\\r", "\r")
            .replace("\\n", "\n")
            .replace("\\t", "\t");
        Ok(unescaped.into_bytes())
    } else if let Some(h) = hex {
        let clean: String = h.chars().filter(|c| !c.is_whitespace()).collect();
        (0..clean.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&clean[i..i + 2], 16)
                    .context(format!("invalid hex at position {i}"))
            })
            .collect()
    } else if let Some(path) = file {
        std::fs::read(path).context(format!("failed to read payload file: {path}"))
    } else {
        Ok(Vec::new())
    }
}

/// Parse --framing flag into TcpFraming.
fn parse_tcp_mode(s: &str) -> Result<netanvil_tcp::TcpTestMode> {
    match s.to_lowercase().as_str() {
        "echo" => Ok(netanvil_tcp::TcpTestMode::Echo),
        "rr" => Ok(netanvil_tcp::TcpTestMode::RR),
        "stream" | "sink" => Ok(netanvil_tcp::TcpTestMode::Sink),
        "maerts" | "source" => Ok(netanvil_tcp::TcpTestMode::Source),
        "bidir" => Ok(netanvil_tcp::TcpTestMode::Bidir),
        other => anyhow::bail!("unknown mode: {other}. Expected: echo, rr, stream, maerts, bidir"),
    }
}

fn parse_tcp_framing(s: &str, delimiter: &str) -> Result<netanvil_tcp::TcpFraming> {
    if s == "raw" {
        Ok(netanvil_tcp::TcpFraming::Raw)
    } else if let Some(rest) = s.strip_prefix("length-prefix:") {
        let width: u8 = rest
            .parse()
            .context("length-prefix width must be 1, 2, or 4")?;
        if !matches!(width, 1 | 2 | 4) {
            anyhow::bail!("length-prefix width must be 1, 2, or 4, got {width}");
        }
        Ok(netanvil_tcp::TcpFraming::LengthPrefixed { width })
    } else if s == "delimiter" {
        let bytes = delimiter
            .replace("\\r", "\r")
            .replace("\\n", "\n")
            .into_bytes();
        Ok(netanvil_tcp::TcpFraming::Delimiter(bytes))
    } else if let Some(rest) = s.strip_prefix("fixed:") {
        let size: usize = rest.parse().context("fixed size must be a number")?;
        Ok(netanvil_tcp::TcpFraming::FixedSize(size))
    } else {
        anyhow::bail!("unknown framing: {s}. Expected: raw, length-prefix:N, delimiter, fixed:N")
    }
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
        let rate = build_rate_config(
            "static",
            500.0,
            None,
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        )
        .unwrap();
        assert!(matches!(rate, RateConfig::Static { rps } if rps == 500.0));
    }

    #[test]
    fn build_step_rate() {
        let rate = build_rate_config(
            "step",
            0.0,
            Some("0s:100,5s:500"),
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        )
        .unwrap();
        assert!(matches!(rate, RateConfig::Step { steps } if steps.len() == 2));
    }

    #[test]
    fn build_step_rate_requires_steps() {
        let result = build_rate_config(
            "step",
            0.0,
            None,
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn build_pid_rate_with_manual_gains() {
        let rate = build_rate_config(
            "pid",
            500.0,
            None,
            &PidArgs {
                metric: "latency-p99",
                target: 200.0,
                kp: Some(0.1),
                ki: Some(0.01),
                kd: Some(0.05),
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        )
        .unwrap();
        match rate {
            RateConfig::Pid {
                initial_rps,
                target,
            } => {
                assert_eq!(initial_rps, 500.0);
                assert_eq!(target.value, 200.0);
                assert!(matches!(target.metric, TargetMetric::LatencyP99));
                assert!(
                    matches!(target.gains, netanvil_types::PidGains::Manual { kp, .. } if (kp - 0.1).abs() < 0.001)
                );
            }
            _ => panic!("expected Pid"),
        }
    }

    #[test]
    fn build_pid_rate_autotune_default() {
        let rate = build_rate_config(
            "pid",
            500.0,
            None,
            &PidArgs {
                metric: "latency-p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 10000.0,
                constraints: &[],
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        )
        .unwrap();
        match rate {
            RateConfig::Pid { target, .. } => {
                assert!(matches!(target.gains, netanvil_types::PidGains::Auto { .. }));
            }
            _ => panic!("expected Pid"),
        }
    }

    #[test]
    fn build_composite_pid() {
        let constraints = vec![
            "latency-p99 < 500".to_string(),
            "error-rate < 2".to_string(),
        ];
        let rate = build_rate_config(
            "pid",
            1000.0,
            None,
            &PidArgs {
                metric: "p99",
                target: 200.0,
                kp: None,
                ki: None,
                kd: None,
                min_rps: 10.0,
                max_rps: 50000.0,
                constraints: &constraints,
                autotune_duration: Duration::from_secs(3),
            },
            &RampArgs {
                warmup_duration: Duration::from_secs(10),
                latency_multiplier: 3.0,
                max_error_rate: 5.0,
            },
        )
        .unwrap();
        match rate {
            RateConfig::CompositePid {
                initial_rps,
                constraints,
                ..
            } => {
                assert_eq!(initial_rps, 1000.0);
                assert_eq!(constraints.len(), 2);
                assert_eq!(constraints[0].limit, 500.0);
                assert_eq!(constraints[1].limit, 2.0);
            }
            _ => panic!("expected CompositePid"),
        }
    }

    #[test]
    fn parse_pid_constraint_valid() {
        let c = parse_pid_constraint("latency-p99 < 500").unwrap();
        assert!(matches!(c.metric, TargetMetric::LatencyP99));
        assert_eq!(c.limit, 500.0);
    }

    #[test]
    fn parse_pid_constraint_external() {
        let c = parse_pid_constraint("external:load < 80").unwrap();
        assert!(matches!(c.metric, TargetMetric::External { name } if name == "load"));
        assert_eq!(c.limit, 80.0);
    }

    #[test]
    fn parse_pid_constraint_invalid() {
        assert!(parse_pid_constraint("no-operator").is_err());
    }

    #[test]
    fn parse_target_metric_external() {
        let m = parse_target_metric("external:load").unwrap();
        assert!(matches!(m, TargetMetric::External { name } if name == "load"));
    }

    #[test]
    fn parse_count_distribution_fixed() {
        let d = parse_count_distribution("fixed:100").unwrap();
        assert!(matches!(d, CountDistribution::Fixed(100)));
    }

    #[test]
    fn parse_count_distribution_uniform() {
        let d = parse_count_distribution("uniform:50,200").unwrap();
        assert!(matches!(
            d,
            CountDistribution::Uniform { min: 50, max: 200 }
        ));
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
            ConnectionPolicy::Mixed {
                persistent_ratio,
                connection_lifetime,
            } => {
                assert!((persistent_ratio - 0.7).abs() < 0.01);
                assert!(matches!(
                    connection_lifetime,
                    Some(CountDistribution::Fixed(100))
                ));
            }
            _ => panic!("expected Mixed"),
        }
    }
}
