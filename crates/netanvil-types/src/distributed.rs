use serde::{Deserialize, Serialize};

use crate::config::TestConfig;
use crate::node::{NodeId, NodeInfo};
use crate::NetAnvilError;

// ---------------------------------------------------------------------------
// Distributed control-plane traits.
//
// These define the seams where future implementations (gossip, CRDT, election)
// plug in without changing the DistributedCoordinator.
//
// All traits are Send + Sync because:
// - Send: the coordinator thread owns the impl
// - Sync: allows sharing with an API server on another thread (future)
//
// I/O-bound methods are async, enabling concurrent agent communication
// (e.g., fetching metrics from N agents in parallel with join_all).
// The coordinator runs these on a current_thread tokio runtime on its
// dedicated thread — no interaction with the hot-path timer thread.
// ---------------------------------------------------------------------------

/// Discovers nodes in the cluster.
///
/// MVP: static list from CLI args.
/// Future: gossip-based Orswot CRDT for node registry.
pub trait NodeDiscovery: Send + Sync {
    /// Returns the current set of known nodes.
    /// Async to support future discovery mechanisms that do I/O (e.g., DNS, gossip).
    fn discover(&self) -> impl std::future::Future<Output = Vec<NodeInfo>> + Send;

    /// Mark a node as failed (excluded from subsequent discover() calls).
    /// Sync — just a local state update, no I/O.
    fn mark_failed(&self, id: &NodeId);
}

/// Fetches metrics from an agent node.
///
/// MVP: HTTP GET /metrics, parses JSON into RemoteMetrics.
/// Future: reads from local CRDT state (gossip-pushed, no HTTP call).
pub trait MetricsFetcher: Send + Sync {
    /// Fetch the latest metrics from the given node.
    /// Returns None if the node is unreachable or has no data.
    fn fetch_metrics(
        &self,
        node: &NodeInfo,
    ) -> impl std::future::Future<Output = Option<RemoteMetrics>> + Send;
}

/// Sends commands to an agent node.
///
/// MVP: HTTP POST/PUT to agent endpoints.
/// Future: stays HTTP/gRPC (point-to-point commands don't benefit from gossip).
pub trait NodeCommander: Send + Sync {
    /// Start a test on the given agent.
    fn start_test(
        &self,
        node: &NodeInfo,
        config: &TestConfig,
    ) -> impl std::future::Future<Output = Result<(), NetAnvilError>> + Send;

    /// Update the agent's target request rate.
    fn set_rate(
        &self,
        node: &NodeInfo,
        rps: f64,
    ) -> impl std::future::Future<Output = Result<(), NetAnvilError>> + Send;

    /// Stop the test on the given agent.
    fn stop_test(
        &self,
        node: &NodeInfo,
    ) -> impl std::future::Future<Output = Result<(), NetAnvilError>> + Send;
}

/// Metrics reported by a remote agent.
/// This is what the leader receives when polling or receiving pushed metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteMetrics {
    pub node_id: NodeId,
    pub current_rps: f64,
    pub target_rps: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub error_rate: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p99_ms: f64,
}
