use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use netanvil_distributed::{
    LeaderApiState, LeaderMetricsState, LeaderServer, QueueConfig, ResultStore, TestQueue,
};
use netanvil_types::TlsConfig;

pub fn run(
    listen: String,
    workers: Vec<String>,
    results_dir: String,
    max_results: usize,
    metrics_port: Option<u16>,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    external_metrics_url: Option<String>,
    external_metrics_field: Option<String>,
) -> Result<()> {
    let tls_config = match (&tls_ca, &tls_cert, &tls_key) {
        (Some(ca), Some(cert), Some(key)) => Some(TlsConfig {
            ca_cert: ca.clone(),
            cert: cert.clone(),
            key: key.clone(),
        }),
        _ => None,
    };

    tracing::info!(
        listen = %listen,
        workers = workers.len(),
        results_dir = %results_dir,
        max_results,
        metrics_port = metrics_port.map(|p| p.to_string()).unwrap_or_else(|| "none".into()),
        mtls = tls_config.is_some(),
        "starting leader daemon"
    );

    // Open result store.
    let store = ResultStore::open(&results_dir, max_results)
        .context(format!("failed to open results directory: {results_dir}"))?;

    let loaded = store.load_index();
    tracing::info!(stored_tests = loaded.len(), "loaded test history from disk");

    // Create test queue.
    let queue = TestQueue::new(store);

    // Shared metrics state — used by both the dedicated metrics server
    // and the API server's /metrics/prometheus endpoint.
    let leader_metrics = LeaderMetricsState::new();

    // Shared API state.
    let state = Arc::new(LeaderApiState {
        queue: queue.clone(),
        agents: Arc::new(Mutex::new(Vec::new())),
        leader_metrics: leader_metrics.clone(),
    });

    let queue_config = QueueConfig {
        workers: workers.clone(),
        tls: tls_config,
        external_metrics_url,
        external_metrics_field,
        agents: state.agents.clone(),
        leader_metrics: leader_metrics.clone(),
    };

    // Single tokio runtime for the entire control plane.
    // All async components (API server, test scheduler, metrics server)
    // run cooperatively on one thread via select! — no Send required.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let scheduler = queue.run_scheduler(queue_config);
        let api = netanvil_distributed::leader_api::serve(&listen, state);

        if let Some(port) = metrics_port {
            let metrics = LeaderServer::new(leader_metrics).serve(port);
            tokio::select! {
                _ = scheduler => {},
                result = api => { result.context("leader API server failed")?; },
                result = metrics => { result.context("metrics server failed")?; },
            }
        } else {
            tokio::select! {
                _ = scheduler => {},
                result = api => { result.context("leader API server failed")?; },
            }
        }

        Ok(())
    })
}
