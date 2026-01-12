use anyhow::Result;
use clap::Parser;

mod commands;
mod output;
mod parsing;

/// Build version string: "0.1.14 (abc1234)" or "0.1.14 (abc1234-dirty)".
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("NETANVIL_GIT_HASH"),
    ")",
);

#[derive(Parser)]
#[command(name = "netanvil-cli")]
#[command(about = "High-performance HTTP load testing")]
#[command(version)]
struct Cli {
    /// Enable debug/trace logging (equivalent to RUST_LOG=debug)
    #[arg(long, global = true)]
    debug: bool,

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

        /// Path to a plugin script (.lua, .js) or WASM module (.wasm).
        /// The plugin implements request generation logic.
        #[arg(long)]
        plugin: Option<String>,

        /// Plugin type: "hybrid" (Lua config → native hot path),
        /// "lua" (LuaJIT per-request), "wasm" (WASM per-request),
        /// or "js" (V8 JavaScript, requires --features v8).
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

        /// HTTP version: "1.1" (default), "2" (h2 over TLS), "2c" (h2 cleartext),
        /// "auto" (ALPN negotiation). Only applies to HTTP targets.
        #[arg(long, default_value = "1.1")]
        http_version: String,

        /// Bandwidth limit for modem speed simulation. Throttles TCP reads to
        /// create real server-side backpressure. Accepts human-friendly values:
        /// "56k" (56 kbps), "384k", "1m" (1 Mbps), "10m", or raw bps "56000".
        #[arg(long)]
        bandwidth: Option<String>,

        /// Extract numeric value from a response header for PID rate control.
        /// Format: "Header-Name" or "Header-Name:aggregation" where aggregation
        /// is "mean" (default), "max", or "last".
        /// Use with --rate-mode pid --pid-metric external:Header-Name.
        /// Repeatable for multiple signals.
        #[arg(long = "response-signal", num_args = 1)]
        response_signals: Vec<String>,

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

        /// Request payload size (distribution) for RR protocol mode.
        /// Accepts: bare integer "1200", or distribution syntax:
        /// "fixed:1200", "uniform:200,1500", "normal:800,200",
        /// "weighted:200@30,1200@50,1500@20"
        #[arg(long)]
        request_size: Option<String>,

        /// Response payload size (distribution) for RR/SOURCE/BIDIR modes.
        /// Same syntax as --request-size.
        #[arg(long)]
        response_size: Option<String>,

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

        // ── Event logging flags ──
        /// Directory for per-request Arrow IPC event logs. Each I/O worker core
        /// writes an events_core_N.arrow file for post-test statistical analysis.
        #[arg(long)]
        event_log_dir: Option<String>,

        /// Fraction of requests to include in the event log (0.0-1.0).
        /// Default: 1.0 (log all requests). Use lower values at high RPS.
        #[arg(long, default_value = "1.0")]
        event_sample_rate: f64,

        // ── Connection health flags ──
        /// Fraction of TCP reads to sample for CPU affinity observation (0.0-1.0).
        /// Uses io_uring linked SQEs to atomically read SO_INCOMING_CPU after recv.
        /// Requires kernel >= 6.7. Default: 0.0 (disabled).
        #[arg(long, default_value = "0.0")]
        health_sample_rate: f64,
    },

    /// Run as a remotely controllable agent node
    Agent {
        /// Address to listen on: a port (9090) or addr:port (10.0.0.2:9090).
        /// When only a port is given, binds to 0.0.0.0.
        #[arg(long, default_value = "9090")]
        listen: String,

        /// Human-readable node identifier for metrics labels and logging.
        /// Defaults to <hostname>:<port> if not specified.
        #[arg(long)]
        node_id: Option<String>,

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

        /// Plain HTTP port for Prometheus metrics scraping.
        /// When running with mTLS, Prometheus can scrape this port
        /// without client certificates. Only serves /metrics/prometheus.
        #[arg(long)]
        metrics_port: Option<u16>,

        /// Trusted Subject Alternative Names (SANs) from leader certificates.
        /// When specified, only clients presenting a certificate with a SAN
        /// matching one of these values are authorized. Comma-separated.
        /// Requires TLS to be enabled.
        #[arg(long, value_delimiter = ',')]
        trusted_san: Vec<String>,

        /// Migrate system threads off hot-path CPU cores before starting.
        /// Shields timer and I/O worker cores from scheduling noise by
        /// moving non-essential threads (including other processes) to the
        /// housekeeping core. Requires CAP_SYS_NICE for cross-process migration.
        #[arg(long)]
        isolate_cpus: bool,
    },

    /// Run as distributed test leader, coordinating agent nodes
    Leader {
        /// Agent addresses (host:port), comma-separated
        #[arg(long, required = true, value_delimiter = ',')]
        workers: Vec<String>,

        /// Target URL(s)
        #[arg(long, required = true, num_args = 1..)]
        url: Vec<String>,

        /// Path to a plugin script (.lua, .js) or WASM module (.wasm).
        /// The plugin is read locally and sent to all agents.
        #[arg(long)]
        plugin: Option<String>,

        /// Plugin type: "hybrid", "lua", "wasm", or "js". Auto-detected from extension if omitted.
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

        /// HTTP version: "1.1" (default), "2" (h2 over TLS), "2c" (h2 cleartext),
        /// "auto" (ALPN negotiation). Only applies to HTTP targets.
        #[arg(long, default_value = "1.1")]
        http_version: String,

        /// Extract numeric value from a response header for PID rate control.
        /// Format: "Header-Name" or "Header-Name:aggregation" (mean/max/last).
        /// Repeatable for multiple signals.
        #[arg(long = "response-signal", num_args = 1)]
        response_signals: Vec<String>,

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

        /// Request payload size (distribution) for RR protocol mode.
        /// Same syntax as test command --request-size.
        #[arg(long)]
        request_size: Option<String>,

        /// Response payload size (distribution) for RR/SOURCE/BIDIR modes.
        /// Same syntax as test command --response-size.
        #[arg(long)]
        response_size: Option<String>,

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

        /// Port for the leader's Prometheus metrics endpoint.
        /// Exposes aggregated metrics at /metrics/prometheus for single-target scraping.
        #[arg(long)]
        metrics_port: Option<u16>,

        /// Output format: "text" (human-readable to stderr) or "json" (machine-readable to stdout)
        #[arg(long, default_value = "text")]
        output: String,

        /// Fraction of TCP reads to sample for CPU affinity observation (0.0-1.0).
        /// Uses io_uring linked SQEs to atomically read SO_INCOMING_CPU after recv.
        /// Requires kernel >= 6.7 on agents. Default: 0.0 (disabled).
        #[arg(long, default_value = "0.0")]
        health_sample_rate: f64,
    },

    /// Run as a persistent leader daemon with HTTP API and test queue
    LeaderServer {
        /// HTTP API listen address (e.g., "0.0.0.0:8080")
        #[arg(long, default_value = "0.0.0.0:8080")]
        listen: String,

        /// Agent addresses (host:port), comma-separated
        #[arg(long, value_delimiter = ',', required = true)]
        workers: Vec<String>,

        /// Directory for storing test results
        #[arg(long, default_value = "/opt/netanvil-rs/results")]
        results_dir: String,

        /// Maximum number of completed test results to keep
        #[arg(long, default_value = "100")]
        max_results: usize,

        /// Port for the dedicated Prometheus metrics endpoint.
        /// Exposes the same aggregated metrics as one-shot mode,
        /// so Prometheus config is identical regardless of leader mode.
        #[arg(long)]
        metrics_port: Option<u16>,

        /// Path to CA certificate PEM for mTLS agent communication
        #[arg(long)]
        tls_ca: Option<String>,

        /// Path to leader's certificate PEM
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to leader's private key PEM
        #[arg(long)]
        tls_key: Option<String>,

        /// Optional URL to poll for external metrics
        #[arg(long)]
        external_metrics_url: Option<String>,

        /// JSON field name to extract from external metrics
        #[arg(long)]
        external_metrics_field: Option<String>,

    },
}

fn main() -> Result<()> {
    // Install the ring crypto provider for rustls before any TLS usage.
    // Without this, rustls 0.23 panics on auto-detection when no single
    // provider feature is unambiguously enabled across the dependency tree.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let cli = Cli::parse();

    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => {
            let level = if cli.debug { "debug" } else { "info" };
            tracing_subscriber::EnvFilter::new(level)
        }
    };

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

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
            http_version,
            bandwidth,
            response_signals,
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
            event_log_dir,
            event_sample_rate,
            health_sample_rate,
        } => commands::test::run(
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
            http_version,
            bandwidth,
            response_signals,
            payload,
            payload_hex,
            payload_file,
            framing,
            delimiter,
            no_response,
            mode,
            request_size,
            response_size,
            dns_domains,
            dns_query_type,
            ramp_warmup,
            ramp_multiplier,
            ramp_max_errors,
            event_log_dir,
            event_sample_rate,
            health_sample_rate,
        )?,

        Commands::Agent {
            listen,
            node_id,
            cores,
            tls_ca,
            tls_cert,
            tls_key,
            metrics_port,
            trusted_san,
            isolate_cpus,
        } => commands::agent::run(listen, node_id, cores, tls_ca, tls_cert, tls_key, metrics_port, trusted_san, isolate_cpus)?,

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
            http_version,
            response_signals,
            payload,
            payload_hex,
            payload_file,
            framing,
            delimiter: _,
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
            metrics_port,
            output,
            health_sample_rate,
        } => commands::leader::run(
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
            http_version,
            response_signals,
            payload,
            payload_hex,
            payload_file,
            framing,
            no_response,
            mode,
            request_size,
            response_size,
            dns_domains,
            dns_query_type,
            ramp_warmup,
            ramp_multiplier,
            ramp_max_errors,
            metrics_port,
            output,
            health_sample_rate,
        )?,

        Commands::LeaderServer {
            listen,
            workers,
            results_dir,
            max_results,
            metrics_port,
            tls_ca,
            tls_cert,
            tls_key,
            external_metrics_url,
            external_metrics_field,
        } => commands::leader_server::run(
            listen,
            workers,
            results_dir,
            max_results,
            metrics_port,
            tls_ca,
            tls_cert,
            tls_key,
            external_metrics_url,
            external_metrics_field,
        )?,
    }

    Ok(())
}
