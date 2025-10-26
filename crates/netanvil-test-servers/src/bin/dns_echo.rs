use clap::Parser;

#[derive(Parser)]
#[command(name = "dns-echo", about = "DNS echo server (compio-based)")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9053")]
    listen: String,
}

fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let handle = netanvil_test_servers::dns::start_dns_echo_on(&args.listen);
    eprintln!("DNS echo server listening on {}", handle.addr);
    eprintln!("Press Ctrl+C to stop");

    std::thread::park();

    drop(handle);
}
