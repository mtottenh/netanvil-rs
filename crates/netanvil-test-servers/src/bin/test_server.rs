use clap::Parser;
use netanvil_test_servers::TestServer;

#[derive(Parser)]
#[command(
    name = "netanvil-test-server",
    about = "Multi-protocol test server (TCP + UDP + DNS, compio-based)"
)]
struct Args {
    /// TCP listen port (e.g. 9000). Omit to disable TCP.
    #[arg(long, default_value = "9000")]
    tcp_port: Option<u16>,

    /// UDP listen port (e.g. 9001). Omit to disable UDP.
    #[arg(long, default_value = "9001")]
    udp_port: Option<u16>,

    /// DNS listen port (e.g. 9053). Omit to disable DNS.
    #[arg(long)]
    dns_port: Option<u16>,

    /// Number of worker threads (default: 1).
    #[arg(long, default_value = "1")]
    workers: usize,

    /// Pin workers to CPU cores.
    #[arg(long, default_value = "true")]
    pin_cores: bool,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let mut builder = TestServer::builder()
        .workers(args.workers)
        .pin_cores(args.pin_cores);

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
        args.workers,
        if args.workers == 1 { "" } else { "s" }
    );
    loop {
        std::thread::park();
    }
}
