use clap::Parser;

#[derive(Parser)]
#[command(name = "tcp-echo", about = "TCP echo server (compio-based)")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9000")]
    listen: String,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let handle = netanvil_test_servers::tcp::start_tcp_echo_on(&args.listen);
    eprintln!("TCP echo server listening on {}", handle.addr);
    eprintln!("Press Ctrl+C to stop");

    // Wait for Ctrl+C — park the main thread indefinitely.
    // The handle's Drop impl will shut down the server when the process exits.
    std::thread::park();

    // Keep handle alive until here
    drop(handle);
}
