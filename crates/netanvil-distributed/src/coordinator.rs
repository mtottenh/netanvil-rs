//! Distributed coordinator: orchestrates load testing across multiple agent nodes.
//!
//! Mirrors the local `Coordinator`'s tick-based pattern but operates over the
//! network. Each tick: discover nodes → fetch metrics → aggregate → rate control
//! → distribute rate.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use netanvil_types::{
    MetricsFetcher, MetricsSummary, NodeCommander, NodeDiscovery, NodeId, NodeInfo, RateController,
    RateDecision, RemoteMetrics, TestConfig,
};

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
    signal_source: Option<Box<dyn FnMut() -> Vec<(String, f64)> + Send>>,
    on_progress: Option<Box<dyn FnMut(&DistributedProgressUpdate)>>,
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
        }
    }

    pub fn set_signal_source(&mut self, f: impl FnMut() -> Vec<(String, f64)> + Send + 'static) {
        self.signal_source = Some(Box::new(f));
    }

    pub fn on_progress(&mut self, f: impl FnMut(&DistributedProgressUpdate) + 'static) {
        self.on_progress = Some(Box::new(f));
    }

    /// Run the distributed test. Blocks until completion.
    pub fn run(&mut self) -> DistributedTestResult {
        let start = Instant::now();

        // Discover nodes and compute initial weights
        let nodes = self.discovery.discover();
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
        // initial rate baked into the config. This avoids a race between
        // the engine starting and a follow-up PUT /rate arriving.
        let initial_rps = self.rate_controller.current_rate();
        for node in &nodes {
            let weight = self.node_weights.get(&node.id).copied().unwrap_or(0.0);
            let node_rps = initial_rps * weight;

            let mut agent_config = self.config.clone();
            agent_config.rate = netanvil_types::RateConfig::Static { rps: node_rps };

            if let Err(e) = self.commander.start_test(node, &agent_config) {
                tracing::error!(node = %node.id, "failed to start test: {e}");
                self.discovery.mark_failed(&node.id);
            }
        }

        // Re-discover after startup to exclude any agents that failed,
        // then recompute weights for the tick loop.
        let nodes = self.discovery.discover();
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
        self.distribute_rate(&nodes, initial_rps);

        // Tick loop
        loop {
            std::thread::sleep(self.config.control_interval);

            let nodes = self.discovery.discover();
            if nodes.is_empty() {
                tracing::error!("all nodes failed, stopping");
                break;
            }

            let decision = self.tick(&nodes);

            let elapsed = start.elapsed();
            if elapsed >= self.config.duration {
                break;
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
                };
                callback(&update);
            }
        }

        // Stop all nodes
        let nodes = self.discovery.discover();
        for node in &nodes {
            let _ = self.commander.stop_test(node);
        }

        // Wait a moment for final metrics
        std::thread::sleep(Duration::from_millis(500));

        // Collect final metrics
        let nodes = self.discovery.discover();
        for node in &nodes {
            if let Some(metrics) = self.fetcher.fetch_metrics(node) {
                self.node_metrics.insert(node.id.clone(), metrics);
            }
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

    fn tick(&mut self, nodes: &[NodeInfo]) -> RateDecision {
        // Fetch metrics from all nodes
        for node in nodes {
            match self.fetcher.fetch_metrics(node) {
                Some(metrics) => {
                    self.node_metrics.insert(node.id.clone(), metrics);
                }
                None => {
                    tracing::warn!(node = %node.id, "metrics fetch failed");
                    self.discovery.mark_failed(&node.id);
                }
            }
        }

        // Aggregate into MetricsSummary for the rate controller
        let mut summary = self.aggregate_metrics();

        // Inject external signals
        if let Some(ref mut source) = self.signal_source {
            summary.external_signals = source();
        }

        // Rate controller decision
        let decision = self.rate_controller.update(&summary);

        tracing::debug!(
            total_requests = summary.total_requests,
            target_rps = decision.target_rps,
            active_nodes = nodes.len(),
            "distributed coordinator tick"
        );

        // Recompute weights (nodes may have changed)
        self.compute_weights(nodes);

        // Distribute rate
        self.distribute_rate(nodes, decision.target_rps);

        decision
    }

    fn aggregate_metrics(&self) -> MetricsSummary {
        let total_requests: u64 = self.node_metrics.values().map(|m| m.total_requests).sum();
        let total_errors: u64 = self.node_metrics.values().map(|m| m.total_errors).sum();

        // Conservative aggregation: take max of percentiles
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

    fn distribute_rate(&self, nodes: &[NodeInfo], total_rps: f64) {
        for node in nodes {
            let weight = self.node_weights.get(&node.id).copied().unwrap_or(0.0);
            let node_rps = total_rps * weight;
            if let Err(e) = self.commander.set_rate(node, node_rps) {
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
