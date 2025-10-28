use anyhow::{Context, Result};

use netanvil_api::AgentServer;

pub fn run(
    listen: u16,
    cores: usize,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    metrics_port: Option<u16>,
) -> Result<()> {
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
        tracing::info!(port = listen, cores, "starting agent (mTLS)");
        AgentServer::with_tls(listen, cores, &tls)
            .context(format!("failed to start mTLS agent on port {listen}"))?
    } else {
        tracing::info!(port = listen, cores, "starting agent (plain HTTP)");
        AgentServer::new(listen, cores)
            .context(format!("failed to start agent on port {listen}"))?
    };

    if let Some(port) = metrics_port {
        server.set_metrics_port(port);
    }

    server.run(); // blocks until Ctrl+C

    Ok(())
}
