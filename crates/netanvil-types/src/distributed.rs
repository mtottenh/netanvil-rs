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
// All methods are synchronous, matching the coordinator's sync tick loop.
// ---------------------------------------------------------------------------

/// Discovers nodes in the cluster.
///
/// MVP: static list from CLI args.
/// Future: gossip-based Orswot CRDT for node registry.
pub trait NodeDiscovery: Send + Sync {
    /// Returns the current set of known nodes.
    fn discover(&self) -> Vec<NodeInfo>;

    /// Mark a node as failed (excluded from subsequent discover() calls).
    fn mark_failed(&self, id: &NodeId);
}

/// Fetches metrics from an agent node.
///
/// MVP: HTTP GET /metrics, parses JSON into RemoteMetrics.
/// Future: reads from local CRDT state (gossip-pushed, no HTTP call).
pub trait MetricsFetcher: Send + Sync {
    /// Fetch the latest metrics from the given node.
    /// Returns None if the node is unreachable or has no data.
    fn fetch_metrics(&self, node: &NodeInfo) -> Option<RemoteMetrics>;
}

/// Sends commands to an agent node.
///
/// MVP: HTTP POST/PUT to agent endpoints.
/// Future: stays HTTP/gRPC (point-to-point commands don't benefit from gossip).
pub trait NodeCommander: Send + Sync {
    /// Start a test on the given agent.
    fn start_test(&self, node: &NodeInfo, config: &TestConfig) -> Result<(), NetAnvilError>;

    /// Update the agent's target request rate.
    fn set_rate(&self, node: &NodeInfo, rps: f64) -> Result<(), NetAnvilError>;

    /// Stop the test on the given agent.
    fn stop_test(&self, node: &NodeInfo) -> Result<(), NetAnvilError>;
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
