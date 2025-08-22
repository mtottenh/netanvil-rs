use clap::Parser;

#[derive(Parser)]
#[command(
    name = "netanvil-test-server",
    about = "Multi-protocol test server (TCP + UDP, compio-based)"
)]
struct Args {
    /// TCP listen address (e.g. 0.0.0.0:9000). Omit to disable TCP.
    #[arg(long, default_value = "127.0.0.1:9000")]
    tcp_listen: Option<String>,

    /// UDP listen address (e.g. 0.0.0.0:9001). Omit to disable UDP.
    #[arg(long, default_value = "127.0.0.1:9001")]
    udp_listen: Option<String>,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let _tcp = if let Some(ref addr) = args.tcp_listen {
        let handle = netanvil_test_servers::tcp::start_tcp_echo_on(addr);
        eprintln!("TCP test server listening on {}", handle.addr);
        Some(handle)
    } else {
        None
    };

    let _udp = if let Some(ref addr) = args.udp_listen {
        let handle = netanvil_test_servers::udp::start_udp_echo_on(addr);
        eprintln!("UDP test server listening on {}", handle.addr);
        Some(handle)
    } else {
        None
    };

    if _tcp.is_none() && _udp.is_none() {
        eprintln!("No listeners configured.");
        std::process::exit(1);
    }

    eprintln!("Press Ctrl+C to stop");
    loop {
        std::thread::park();
    }
}
