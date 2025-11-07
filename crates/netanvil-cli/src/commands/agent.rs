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
    isolate_cpus: bool,
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

    if isolate_cpus {
        let available = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let num_io = cores.saturating_sub(2).max(1);
        let timer_core = 0usize;
        let io_core_start = 1usize;
        let misc_core = if available > num_io + 1 {
            num_io + 1
        } else {
            0
        };
        if misc_core != timer_core {
            let hot_cores: Vec<usize> = std::iter::once(timer_core)
                .chain(io_core_start..io_core_start + num_io)
                .collect();
            let shield = netanvil_core::isolation::shield_hot_cores(&hot_cores, misc_core);
            tracing::info!(
                migrated = shield.migrated,
                skipped = shield.skipped,
                failed = shield.failed,
                ?hot_cores,
                misc_core,
                "shielded hot cores from system threads"
            );
        } else {
            tracing::debug!(
                available,
                "skipping CPU isolation (not enough cores for dedicated housekeeping)"
            );
        }
    }

    server.run(); // blocks until Ctrl+C

    Ok(())
}
