use anyhow::{Context, Result};

use netanvil_api::AgentServer;

/// Parse a listen address string into a bind address.
/// Accepts either a bare port ("9090") or addr:port ("10.0.0.2:9090").
fn parse_listen_addr(listen: &str) -> String {
    if listen.contains(':') {
        listen.to_string()
    } else {
        format!("0.0.0.0:{listen}")
    }
}

pub fn run(
    listen: String,
    node_id: Option<String>,
    cores: usize,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    metrics_port: Option<u16>,
) -> Result<()> {
    let bind_addr = parse_listen_addr(&listen);

    let cores = if cores == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        cores
    };

    let mut server = if let (Some(ca), Some(cert), Some(key)) = (tls_ca, tls_cert, tls_key) {
        let tls = netanvil_types::TlsConfig {
            ca_cert: ca,
            cert,
            key,
        };
        tracing::info!(addr = %bind_addr, cores, node_id = node_id.as_deref().unwrap_or("-"), "starting agent (mTLS)");
        AgentServer::with_tls(&bind_addr, cores, node_id, &tls)
            .context(format!("failed to start mTLS agent on {bind_addr}"))?
    } else {
        tracing::info!(addr = %bind_addr, cores, node_id = node_id.as_deref().unwrap_or("-"), "starting agent (plain HTTP)");
        AgentServer::new(&bind_addr, cores, node_id)
            .context(format!("failed to start agent on {bind_addr}"))?
    };

    if let Some(port) = metrics_port {
        server.set_metrics_port(port);
    }

    server.run(); // blocks until Ctrl+C

    Ok(())
}
