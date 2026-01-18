//! Distributed coordinator: orchestrates load testing across multiple agent nodes.
//!
//! Mirrors the local `Coordinator`'s tick-based pattern but operates over the
//! network. Each tick: discover nodes → fetch metrics → aggregate → rate control
//! → distribute rate.
//!
//! The `run()` method is async, enabling concurrent agent communication
//! (e.g., fetching metrics from N agents in parallel). It runs on a
//! current_thread tokio runtime on the coordinator's dedicated thread.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use netanvil_types::{
    ControllerView, HoldCommand, HoldState, MetricsFetcher, MetricsSummary, NodeCommander,
    NodeDiscovery, NodeId, NodeInfo, RateController, RateDecision, RemoteMetrics, TestConfig,
};

/// Async signal source: called each tick to get external signals.
pub type AsyncSignalSource =
    Box<dyn FnMut() -> Pin<Box<dyn Future<Output = Vec<(String, f64)>> + Send>> + Send>;

/// Command for controller parameter updates in the distributed coordinator.
pub struct ControllerUpdateCommand {
    pub action: String,
    pub params: serde_json::Value,
    pub response_tx: flume::Sender<Result<serde_json::Value, String>>,
}

/// Progress update emitted each tick of the distributed coordinator.
#[derive(Debug, Clone)]
pub struct DistributedProgressUpdate {
    pub elapsed: Duration,
    pub remaining: Duration,
    pub target_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub active_nodes: usize,
    pub per_node: Vec<(NodeId, RemoteMetrics)>,
    /// Controller-specific state for Prometheus gauges (e.g., ramp ceiling).
    pub controller_state: Vec<(&'static str, f64)>,
}

/// Orchestrates a distributed load test across multiple agent nodes.
///
/// Generic over the three distributed traits so implementations can be
/// swapped (HTTP for MVP, gossip/CRDT in the future).
#[allow(clippy::type_complexity)]
pub struct DistributedCoordinator<D, M, C>
where
    D: NodeDiscovery,
    M: MetricsFetcher,
    C: NodeCommander,
{
    discovery: D,
    fetcher: M,
    commander: C,
    config: TestConfig,
    rate_controller: Box<dyn RateController>,
    node_metrics: HashMap<NodeId, RemoteMetrics>,
    node_weights: HashMap<NodeId, f64>,
    signal_source: Option<AsyncSignalSource>,
    on_progress: Option<Box<dyn FnMut(&DistributedProgressUpdate)>>,
    cancel_rx: Option<flume::Receiver<()>>,
    /// External rate override channel (from leader API PUT /tests/{id}/rate).
    rate_override_rx: Option<flume::Receiver<f64>>,
    /// External signal push channel (from leader API PUT /tests/{id}/signal).
    signal_push_rx: Option<flume::Receiver<(String, f64)>>,
    /// Hold state: when Held, controller.update() is skipped.
    hold_state: HoldState,
    /// Channel for hold/release commands.
    hold_command_rx: Option<flume::Receiver<HoldCommand>>,
    /// Channel for controller parameter update commands.
    controller_update_rx: Option<flume::Receiver<ControllerUpdateCommand>>,
    /// Channel for controller introspection requests.
    controller_info_rx: Option<flume::Receiver<flume::Sender<ControllerView>>>,
}

impl<D, M, C> DistributedCoordinator<D, M, C>
where
    D: NodeDiscovery,
    M: MetricsFetcher,
    C: NodeCommander,
{
    pub fn new(
        discovery: D,
        fetcher: M,
        commander: C,
        config: TestConfig,
        rate_controller: Box<dyn RateController>,
    ) -> Self {
        Self {
            discovery,
            fetcher,
            commander,
            config,
            rate_controller,
            node_metrics: HashMap::new(),
            node_weights: HashMap::new(),
            signal_source: None,
            on_progress: None,
            cancel_rx: None,
            rate_override_rx: None,
            signal_push_rx: None,
            hold_state: HoldState::Released,
            hold_command_rx: None,
            controller_update_rx: None,
            controller_info_rx: None,
        }
    }

    /// Set an async signal source called each coordinator tick.
    pub fn set_signal_source<F, Fut>(&mut self, mut f: F)
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Vec<(String, f64)>> + Send + 'static,
    {
        self.signal_source = Some(Box::new(move || Box::pin(f())));
    }

    pub fn on_progress(&mut self, f: impl FnMut(&DistributedProgressUpdate) + 'static) {
        self.on_progress = Some(Box::new(f));
    }

    pub fn set_cancel_rx(&mut self, rx: flume::Receiver<()>) {
        self.cancel_rx = Some(rx);
    }

    pub fn set_rate_override_rx(&mut self, rx: flume::Receiver<f64>) {
        self.rate_override_rx = Some(rx);
    }

    pub fn set_signal_push_rx(&mut self, rx: flume::Receiver<(String, f64)>) {
        self.signal_push_rx = Some(rx);
    }

    pub fn set_hold_command_rx(&mut self, rx: flume::Receiver<HoldCommand>) {
        self.hold_command_rx = Some(rx);
    }

    pub fn set_controller_update_rx(&mut self, rx: flume::Receiver<ControllerUpdateCommand>) {
        self.controller_update_rx = Some(rx);
    }

    pub fn set_controller_info_rx(&mut self, rx: flume::Receiver<flume::Sender<ControllerView>>) {
        self.controller_info_rx = Some(rx);
    }

    /// Run the distributed test. Async — call from a tokio runtime.
    pub async fn run(&mut self) -> DistributedTestResult {
        let start = Instant::now();

        // Discover nodes and compute initial weights
        let nodes = self.discovery.discover().await;
        if nodes.is_empty() {
            tracing::error!("no agent nodes available");
            return DistributedTestResult {
                total_requests: 0,
                total_errors: 0,
                duration: Duration::ZERO,
                nodes: vec![],
            };
        }

        self.compute_weights(&nodes);

        // Start test on all nodes, each with its weighted share of the
        // initial rate baked into the config.
        let initial_rps = self.rate_controller.current_rate();
        for node in &nodes {
            let weight = self.node_weights.get(&node.id).copied().unwrap_or(0.0);
            let node_rps = initial_rps * weight;

            let mut agent_config = self.config.clone();
            agent_config.rate = netanvil_types::RateConfig::Static { rps: node_rps };

            if let Err(e) = self.commander.start_test(node, &agent_config).await {
                tracing::error!(node = %node.id, "failed to start test: {e}");
                self.discovery.mark_failed(&node.id);
            }
        }

        // Re-discover after startup to exclude any agents that failed,
        // then recompute weights for the tick loop.
        let nodes = self.discovery.discover().await;
        if nodes.is_empty() {
            tracing::error!("all nodes failed during startup");
            return DistributedTestResult {
                total_requests: 0,
                total_errors: 0,
                duration: Duration::ZERO,
                nodes: vec![],
            };
        }
        self.compute_weights(&nodes);
        self.distribute_rate(&nodes, initial_rps).await;

        // Tick loop
        loop {
            tokio::time::sleep(self.config.control_interval).await;

            let nodes = self.discovery.discover().await;
            if nodes.is_empty() {
                tracing::error!("all nodes failed, stopping");
                break;
            }

            let decision = self.tick(&nodes).await;

            let elapsed = start.elapsed();
            if elapsed >= self.config.duration {
                break;
            }

            // Check for external cancellation.
            if let Some(ref rx) = self.cancel_rx {
                if rx.try_recv().is_ok() {
                    tracing::info!("test cancelled via cancel channel");
                    break;
                }
            }

            // Progress callback
            if let Some(ref mut callback) = self.on_progress {
                let per_node: Vec<_> = self
                    .node_metrics
                    .iter()
                    .map(|(id, m)| (id.clone(), m.clone()))
                    .collect();
                let update = DistributedProgressUpdate {
                    elapsed,
                    remaining: self.config.duration.saturating_sub(elapsed),
                    target_rps: decision.target_rps,
                    total_requests: self.node_metrics.values().map(|m| m.total_requests).sum(),
                    total_errors: self.node_metrics.values().map(|m| m.total_errors).sum(),
                    active_nodes: nodes.len(),
                    per_node,
                    controller_state: self.rate_controller.controller_state(),
                };
                callback(&update);
            }
        }

        // Collect final metrics BEFORE stopping nodes — agents reset their
        // metrics on test completion, so fetching after stop risks a race.
        let nodes = self.discovery.discover().await;
        // Fetch final metrics from all agents concurrently.
        let metrics_futures: Vec<_> = nodes
            .iter()
            .map(|node| self.fetcher.fetch_metrics(node))
            .collect();
        let metrics_results = futures::future::join_all(metrics_futures).await;
        for (node, metrics) in nodes.iter().zip(metrics_results) {
            if let Some(m) = metrics {
                self.node_metrics.insert(node.id.clone(), m);
            }
        }

        // Stop all nodes
        for node in &nodes {
            let _ = self.commander.stop_test(node).await;
        }

        let total_requests: u64 = self.node_metrics.values().map(|m| m.total_requests).sum();
        let total_errors: u64 = self.node_metrics.values().map(|m| m.total_errors).sum();

        DistributedTestResult {
            total_requests,
            total_errors,
            duration: start.elapsed(),
            nodes: self
                .node_metrics
                .iter()
                .map(|(id, m)| (id.clone(), m.clone()))
                .collect(),
        }
    }

    async fn tick(&mut self, nodes: &[NodeInfo]) -> RateDecision {
        // --- Phase 0: Process control commands ---
        self.process_commands();

        // --- Phase 1: Fetch metrics from all agents concurrently ---
        let metrics_futures: Vec<_> = nodes
            .iter()
            .map(|node| self.fetcher.fetch_metrics(node))
            .collect();
        let metrics_results = futures::future::join_all(metrics_futures).await;

        for (node, metrics) in nodes.iter().zip(metrics_results) {
            match metrics {
                Some(m) => {
                    self.node_metrics.insert(node.id.clone(), m);
                }
                None => {
                    tracing::warn!(node = %node.id, "metrics fetch failed");
                    self.discovery.mark_failed(&node.id);
                }
            }
        }

        let mut summary = self.aggregate_metrics();

        // Inject external signals from polling source.
        if let Some(ref mut source) = self.signal_source {
            summary.external_signals = source().await;
            if !summary.external_signals.is_empty() {
                tracing::info!(signals = ?summary.external_signals, "external signals for rate controller");
            }
        }

        // Inject externally pushed signals (from leader API PUT /signal).
        if let Some(ref rx) = self.signal_push_rx {
            while let Ok((name, value)) = rx.try_recv() {
                if let Some(existing) = summary
                    .external_signals
                    .iter_mut()
                    .find(|(k, _)| *k == name)
                {
                    existing.1 = value;
                } else {
                    summary.external_signals.push((name, value));
                }
            }
        }

        // --- Phase 2: Rate decision ---
        let previous_rps = self.rate_controller.current_rate();
        let decision = match self.hold_state {
            HoldState::Held { rps } => RateDecision { target_rps: rps },
            HoldState::Released => self.rate_controller.update(&summary),
        };

        if (decision.target_rps - previous_rps).abs() > 0.01 {
            tracing::info!(
                previous_rps,
                new_rps = decision.target_rps,
                active_nodes = nodes.len(),
                error_rate = format!("{:.4}", summary.error_rate),
                latency_p99_ms = format!("{:.1}", summary.latency_p99_ns as f64 / 1_000_000.0),
                "leader rate adjustment"
            );
        }

        // Debug: per-agent rate breakdown for diagnosing accounting mismatches.
        // A persistent ratio of achieved/target != 1.0 indicates a systematic bug.
        {
            let per_agent: Vec<_> = self
                .node_metrics
                .iter()
                .map(|(id, m)| format!("{}={:.0}", id.0, m.current_rps))
                .collect();
            tracing::debug!(
                total_requests = summary.total_requests,
                summary_request_rate = format!("{:.0}", summary.request_rate),
                controller_target = format!("{:.0}", decision.target_rps),
                controller_current = format!("{:.0}", previous_rps),
                active_nodes = nodes.len(),
                per_agent = ?per_agent,
                "distributed coordinator tick"
            );
        }

        // Recompute weights (nodes may have changed)
        self.compute_weights(nodes);

        // Distribute rate to all agents concurrently
        self.distribute_rate(nodes, decision.target_rps).await;

        decision
    }

    /// Process all pending control commands (hold, controller updates, etc.).
    /// Sync — just drains channels and updates local state.
    fn process_commands(&mut self) {
        // Hold/release commands.
        if let Some(ref rx) = self.hold_command_rx {
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    HoldCommand::Hold(rps) => {
                        tracing::info!(rps, "rate hold engaged");
                        self.hold_state = HoldState::Held { rps };
                    }
                    HoldCommand::Release => {
                        tracing::info!(
                            controller_rps = self.rate_controller.current_rate(),
                            "rate hold released, resuming controller"
                        );
                        self.hold_state = HoldState::Released;
                    }
                }
            }
        }

        // Controller parameter updates.
        if let Some(ref rx) = self.controller_update_rx {
            while let Ok(cmd) = rx.try_recv() {
                let result = self.rate_controller.apply_update(&cmd.action, &cmd.params);
                let _ = cmd.response_tx.send(result);
            }
        }

        // Controller introspection requests.
        if let Some(ref rx) = self.controller_info_rx {
            while let Ok(response_tx) = rx.try_recv() {
                let info = self.rate_controller.controller_info();
                let view = ControllerView {
                    info,
                    held: matches!(self.hold_state, HoldState::Held { .. }),
                    held_rps: match self.hold_state {
                        HoldState::Held { rps } => Some(rps),
                        HoldState::Released => None,
                    },
                };
                let _ = response_tx.send(view);
            }
        }

        // Legacy rate override (deprecated — translates to hold).
        if let Some(ref rx) = self.rate_override_rx {
            let mut latest = None;
            while let Ok(rps) = rx.try_recv() {
                latest = Some(rps);
            }
            if let Some(rps) = latest {
                tracing::warn!(
                    rps,
                    "rate_override_rx is deprecated, use hold_command_rx instead"
                );
                self.hold_state = HoldState::Held { rps };
            }
        }
    }

    fn aggregate_metrics(&self) -> MetricsSummary {
        let total_requests: u64 = self.node_metrics.values().map(|m| m.total_requests).sum();
        let total_errors: u64 = self.node_metrics.values().map(|m| m.total_errors).sum();

        let p50 = self
            .node_metrics
            .values()
            .map(|m| m.latency_p50_ms)
            .fold(0.0f64, f64::max);
        let p90 = self
            .node_metrics
            .values()
            .map(|m| m.latency_p90_ms)
            .fold(0.0f64, f64::max);
        let p99 = self
            .node_metrics
            .values()
            .map(|m| m.latency_p99_ms)
            .fold(0.0f64, f64::max);

        let request_rate: f64 = self.node_metrics.values().map(|m| m.current_rps).sum();
        let error_rate = if total_requests > 0 {
            total_errors as f64 / total_requests as f64
        } else {
            0.0
        };

        MetricsSummary {
            total_requests,
            total_errors,
            request_rate,
            error_rate,
            latency_p50_ns: (p50 * 1_000_000.0) as u64,
            latency_p90_ns: (p90 * 1_000_000.0) as u64,
            latency_p99_ns: (p99 * 1_000_000.0) as u64,
            window_duration: self.config.control_interval,
            bytes_sent: 0,
            bytes_received: 0,
            throughput_send_bps: 0.0,
            throughput_recv_bps: 0.0,
            external_signals: Vec::new(),
            timeout_count: self.node_metrics.values().map(|m| m.timeout_count).sum(),
            in_flight_drops: self.node_metrics.values().map(|m| m.in_flight_drops).sum(),
        }
    }

    fn compute_weights(&mut self, nodes: &[NodeInfo]) {
        let total_cores: usize = nodes.iter().map(|n| n.cores).sum();
        if total_cores == 0 {
            return;
        }
        self.node_weights.clear();
        for node in nodes {
            let weight = node.cores as f64 / total_cores as f64;
            self.node_weights.insert(node.id.clone(), weight);
        }
    }

    async fn distribute_rate(&self, nodes: &[NodeInfo], total_rps: f64) {
        for node in nodes {
            let weight = self.node_weights.get(&node.id).copied().unwrap_or(0.0);
            let node_rps = total_rps * weight;
            if let Err(e) = self.commander.set_rate(node, node_rps).await {
                tracing::warn!(node = %node.id, rps = node_rps, "failed to set rate: {e}");
            }
        }
    }
}

/// Result of a distributed test.
#[derive(Debug, Clone)]
pub struct DistributedTestResult {
    pub total_requests: u64,
    pub total_errors: u64,
    pub duration: Duration,
    pub nodes: Vec<(NodeId, RemoteMetrics)>,
}
