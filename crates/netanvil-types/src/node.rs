use serde::{Deserialize, Serialize};

/// Unique identifier for a node in the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Information about a node in the cluster.
/// Reported by agents, consumed by the leader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    /// "host:port" for the agent's API
    pub addr: String,
    /// Number of CPU cores available for load generation
    pub cores: usize,
    /// Current lifecycle state
    pub state: NodeState,
}

/// Agent lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    /// Ready to accept a test.
    Idle,
    /// Currently running a test.
    Running,
    /// Test completed, results available.
    Completed,
    /// Node failed or unreachable.
    Failed,
}
