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

    // Start dedicated Prometheus metrics server on --metrics-port.
    // This is the same LeaderServer used by one-shot mode, exposing
    // identical metrics on an identical port — Prometheus config is
    // mode-agnostic.
    if let Some(port) = metrics_port {
        let server = LeaderServer::new(leader_metrics.clone());
        server
            .spawn(port)
            .unwrap_or_else(|e| tracing::error!(port, "failed to start metrics server: {e}"));
    }

    // Spawn the scheduler thread (runs tests from the queue).
    let scheduler_queue = queue.clone();
    let queue_config = QueueConfig {
        workers: workers.clone(),
        tls: tls_config,
        external_metrics_url,
        external_metrics_field,
        agents: state.agents.clone(),
        leader_metrics,
    };
    std::thread::Builder::new()
        .name("test-scheduler".into())
        .spawn(move || {
            scheduler_queue.run_scheduler(queue_config);
        })
        .context("failed to spawn scheduler thread")?;

    // Run the HTTP API server (blocks).
    netanvil_distributed::leader_api::serve(&listen, state)
        .context("leader API server failed")?;

    Ok(())
}
