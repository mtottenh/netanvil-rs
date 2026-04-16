//! Multicall test server binary.
//!
//! Supports three invocation modes:
//!
//! 1. **Subcommands**: `netanvil-test-server tcp --port 9000 --workers 4`
//! 2. **Symlinks** (busybox pattern): create symlinks named `netanvil-tcp-echo`,
//!    `netanvil-udp-echo`, `netanvil-dns-echo` → the binary detects argv[0] and
//!    behaves as the corresponding single-protocol server.
//! 3. **Multi-protocol**: `netanvil-test-server multi --tcp-port 9000 --udp-port 9001`

use clap::Parser;
use netanvil_test_servers::reporter::ReportMode;
use netanvil_test_servers::{ServerConfig, TestServer};

// ---------------------------------------------------------------------------
// Shared flags (flattened into every subcommand)
// ---------------------------------------------------------------------------

#[derive(clap::Args, Debug, Clone)]
struct CommonArgs {
    /// Number of worker threads (default: 1).
    #[arg(long, default_value = "1")]
    workers: usize,

    /// Pin workers to CPU cores (disable with --no-pin-cores).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pin_cores: bool,

    /// Path to a file whose contents are tiled across write buffers.
    /// Default: buffers filled with deterministic PRNG data.
    #[arg(long)]
    fill_data: Option<String>,

    /// Metrics reporting mode: none, text (iperf3-style), json,
    /// or prometheus:PORT (e.g. prometheus:9090).
    #[arg(long, default_value = "none")]
    report: String,

    /// Metrics reporting interval in seconds.
    #[arg(long, default_value = "1.0")]
    report_interval: f64,
}

// ---------------------------------------------------------------------------
// Per-protocol subcommand args
// ---------------------------------------------------------------------------

/// TCP test server — supports RR, CRR, echo, sink, source, bidir modes.
#[derive(Parser, Debug)]
#[command(name = "netanvil-tcp-echo")]
struct TcpArgs {
    /// TCP listen port (0 = random OS-assigned port).
    #[arg(long, default_value = "9000")]
    port: u16,

    /// Maximum simultaneous TCP connections (0 = unlimited).
    #[arg(long, default_value = "10000")]
    max_connections: usize,

    /// Fraction of TCP reads to piggyback with getsockopt(TCP_INFO) (0.0–1.0).
    /// 0.0 = disabled. Requires kernel >= 6.7.
    #[arg(long, default_value = "0.0")]
    tcp_health_sample_rate: f64,

    #[command(flatten)]
    common: CommonArgs,
}

/// UDP echo server with optional pacing, latency injection, and drop simulation.
#[derive(Parser, Debug)]
#[command(name = "netanvil-udp-echo")]
struct UdpArgs {
    /// UDP listen port (0 = random OS-assigned port).
    #[arg(long, default_value = "9001")]
    port: u16,

    /// Connected UDP idle timeout in seconds.
    /// 0 = disabled (use unconnected recvfrom/sendto). When > 0, creates
    /// per-client connected sockets for kernel-level demuxing.
    #[arg(long, default_value = "30")]
    udp_idle_timeout: u32,

    /// UDP send rate limit in bytes/sec per client. 0 = unlimited.
    /// Only effective with connected UDP (--udp-idle-timeout > 0).
    #[arg(long, default_value = "0")]
    udp_pacing_bps: u64,

    /// Delay in microseconds before each UDP echo response. 0 = none.
    #[arg(long, default_value = "0")]
    udp_latency_us: u64,

    /// UDP response drop rate per 10000. 0 = no drops.
    #[arg(long, default_value = "0")]
    udp_drop_rate: u32,

    #[command(flatten)]
    common: CommonArgs,
}

/// DNS authoritative echo server — responds to any query with 127.0.0.1.
#[derive(Parser, Debug)]
#[command(name = "netanvil-dns-echo")]
struct DnsArgs {
    /// DNS listen port (0 = random OS-assigned port).
    #[arg(long, default_value = "9053")]
    port: u16,

    #[command(flatten)]
    common: CommonArgs,
}

/// Multi-protocol server (TCP + UDP + DNS simultaneously).
#[derive(Parser, Debug)]
struct MultiArgs {
    /// TCP listen port. Omit to disable TCP.
    #[arg(long, default_value = "9000")]
    tcp_port: Option<u16>,

    /// UDP listen port. Omit to disable UDP.
    #[arg(long, default_value = "9001")]
    udp_port: Option<u16>,

    /// DNS listen port. Omit to disable DNS.
    #[arg(long)]
    dns_port: Option<u16>,

    /// Maximum simultaneous TCP connections (0 = unlimited).
    #[arg(long, default_value = "10000")]
    max_connections: usize,

    /// Fraction of TCP reads to piggyback with getsockopt(TCP_INFO) (0.0–1.0).
    #[arg(long, default_value = "0.0")]
    tcp_health_sample_rate: f64,

    /// Connected UDP idle timeout in seconds. 0 = disabled.
    #[arg(long, default_value = "30")]
    udp_idle_timeout: u32,

    /// UDP send rate limit in bytes/sec per client. 0 = unlimited.
    #[arg(long, default_value = "0")]
    udp_pacing_bps: u64,

    /// Delay in microseconds before each UDP echo response. 0 = none.
    #[arg(long, default_value = "0")]
    udp_latency_us: u64,

    /// UDP response drop rate per 10000. 0 = no drops.
    #[arg(long, default_value = "0")]
    udp_drop_rate: u32,

    #[command(flatten)]
    common: CommonArgs,
}

// ---------------------------------------------------------------------------
// Top-level CLI with subcommands
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "netanvil-test-server",
    about = "Multi-protocol test server (TCP + UDP + DNS, compio-based)\n\n\
             Use subcommands for single-protocol mode, or create symlinks\n\
             named netanvil-tcp-echo / netanvil-udp-echo / netanvil-dns-echo."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// TCP test server (RR, CRR, echo, sink, source, bidir modes)
    Tcp(TcpArgs),
    /// UDP echo server with optional pacing, latency, and drop injection
    Udp(UdpArgs),
    /// DNS authoritative echo server
    Dns(DnsArgs),
    /// Multi-protocol server (TCP + UDP + DNS simultaneously)
    Multi(MultiArgs),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_report_mode(s: &str) -> ReportMode {
    if let Some(port_str) = s.strip_prefix("prometheus:") {
        let port: u16 = port_str.parse().unwrap_or_else(|_| {
            eprintln!("Invalid prometheus port: '{}'", port_str);
            std::process::exit(1);
        });
        ReportMode::Prometheus { port }
    } else {
        match s {
            "text" => ReportMode::Text,
            "json" => ReportMode::Json,
            _ => ReportMode::None,
        }
    }
}

fn load_fill_pattern(common: &CommonArgs) -> Option<Vec<u8>> {
    common.fill_data.as_ref().map(|path| {
        std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("Failed to read fill-data file '{}': {}", path, e);
            std::process::exit(1);
        })
    })
}

fn base_config(common: &CommonArgs) -> ServerConfig {
    ServerConfig {
        fill_pattern: load_fill_pattern(common),
        report_mode: parse_report_mode(&common.report),
        report_interval_secs: common.report_interval,
        num_workers: common.workers,
        pin_cores: common.pin_cores,
        ..Default::default()
    }
}

fn print_addrs_and_park(server: &TestServer, workers: usize) {
    if let Some(addr) = server.tcp_addr() {
        eprintln!("TCP test server listening on {}", addr);
    }
    if let Some(addr) = server.udp_addr() {
        eprintln!("UDP test server listening on {}", addr);
    }
    if let Some(addr) = server.dns_addr() {
        eprintln!("DNS test server listening on {}", addr);
    }

    if server.tcp_addr().is_none() && server.udp_addr().is_none() && server.dns_addr().is_none() {
        eprintln!("No listeners configured.");
        std::process::exit(1);
    }

    eprintln!(
        "Press Ctrl+C to stop ({} worker{})",
        workers,
        if workers == 1 { "" } else { "s" }
    );
    loop {
        std::thread::park();
    }
}

// ---------------------------------------------------------------------------
// run_* functions
// ---------------------------------------------------------------------------

fn run_tcp(args: TcpArgs) {
    let config = ServerConfig {
        max_connections: args.max_connections,
        tcp_health_sample_rate: args.tcp_health_sample_rate,
        ..base_config(&args.common)
    };
    let workers = config.num_workers;
    let server = TestServer::builder().tcp(args.port).config(config).build();
    print_addrs_and_park(&server, workers);
}

fn run_udp(args: UdpArgs) {
    let config = ServerConfig {
        udp_idle_timeout_secs: args.udp_idle_timeout,
        udp_pacing_bps: args.udp_pacing_bps,
        udp_latency_us: args.udp_latency_us,
        udp_drop_rate: args.udp_drop_rate,
        ..base_config(&args.common)
    };
    let workers = config.num_workers;
    let server = TestServer::builder().udp(args.port).config(config).build();
    print_addrs_and_park(&server, workers);
}

fn run_dns(args: DnsArgs) {
    let config = base_config(&args.common);
    let workers = config.num_workers;
    let server = TestServer::builder().dns(args.port).config(config).build();
    print_addrs_and_park(&server, workers);
}

fn run_multi(args: MultiArgs) {
    let config = ServerConfig {
        max_connections: args.max_connections,
        tcp_health_sample_rate: args.tcp_health_sample_rate,
        udp_idle_timeout_secs: args.udp_idle_timeout,
        udp_pacing_bps: args.udp_pacing_bps,
        udp_latency_us: args.udp_latency_us,
        udp_drop_rate: args.udp_drop_rate,
        ..base_config(&args.common)
    };
    let workers = config.num_workers;
    let mut builder = TestServer::builder().config(config);

    if let Some(port) = args.tcp_port {
        builder = builder.tcp(port);
    }
    if let Some(port) = args.udp_port {
        builder = builder.udp(port);
    }
    if let Some(port) = args.dns_port {
        builder = builder.dns(port);
    }

    let server = builder.build();
    print_addrs_and_park(&server, workers);
}

// ---------------------------------------------------------------------------
// main: argv[0] detection + subcommand dispatch
// ---------------------------------------------------------------------------

fn main() {
    tracing_subscriber::fmt::init();

    // Busybox pattern: detect symlink name to select protocol mode.
    let argv0 = std::env::args()
        .next()
        .and_then(|p| {
            std::path::Path::new(&p)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_default();

    match argv0.as_str() {
        s if s.contains("tcp-echo") || s.contains("tcp_echo") => {
            run_tcp(TcpArgs::parse());
        }
        s if s.contains("udp-echo") || s.contains("udp_echo") => {
            run_udp(UdpArgs::parse());
        }
        s if s.contains("dns-echo") || s.contains("dns_echo") => {
            run_dns(DnsArgs::parse());
        }
        _ => {
            let cli = Cli::parse();
            match cli.command {
                Commands::Tcp(args) => run_tcp(args),
                Commands::Udp(args) => run_udp(args),
                Commands::Dns(args) => run_dns(args),
                Commands::Multi(args) => run_multi(args),
            }
        }
    }
}
