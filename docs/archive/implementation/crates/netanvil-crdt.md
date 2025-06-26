# netanvil-crdt Implementation Guide

## Overview

The `netanvil-crdt` crate implements distributed state management using the proven `crdts` library. It provides eventually consistent state synchronization across nodes without requiring coordination.

## Related Design Documents

- [CRDT Library Integration](../../section-4-4-crdt-library-integration.md) - Primary design reference
- [CRDT Implementation Design](../../section-4-3-crdt-implementation-design.md) - Theoretical background
- [Distributed Coordination](../../section-4-2-distributed-coordination-design.md) - Integration context

## Key Components

### Core State Structure

Based on the CRDT library integration design:

```rust
use crdts::{CmRDT, Map, Orswot, VClock, lwwreg::LWWReg, gcounter::GCounter};

/// Distributed state using proven CRDT implementations
pub struct DistributedState {
    /// Node registry - Observed-Remove Set without Tombstones
    pub nodes: Orswot<NodeInfo, ActorId>,
    
    /// Load assignments - Last-Write-Wins Register per node
    pub load_assignments: Map<NodeId, LWWReg<LoadAssignment>, ActorId>,
    
    /// Test parameters - Map of LWW registers
    pub parameters: Map<String, LWWReg<f64>, ActorId>,
    
    /// Total RPS counter - Grow-only counter
    pub total_rps: GCounter<ActorId>,
    
    /// Custom epoch log (built on Map)
    pub epochs: EpochLog,
    
    /// Vector clock for causality
    pub clock: VClock<ActorId>,
    
    /// Actor ID (node ID)
    actor: ActorId,
}

/// Node information stored in CRDT
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub role: NodeRole,
    pub state: NodeState,
    pub capabilities: NodeCapabilities,
    pub last_heartbeat: u64, // Unix timestamp
}

/// Load assignment for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadAssignment {
    pub target_rps: u64,
    pub load_proportion: f64,
    pub url_patterns: Vec<String>,
    pub assigned_by: ActorId,
    pub fencing_token: u64,
    pub assigned_at: u64, // Unix timestamp
}
```

### State Operations

Clean, simple operations thanks to the `crdts` library:

```rust
impl DistributedState {
    /// Create new state for a node
    pub fn new(actor_id: ActorId) -> Self {
        Self {
            nodes: Orswot::new(),
            load_assignments: Map::new(),
            parameters: Map::new(),
            total_rps: GCounter::new(),
            epochs: EpochLog::new(actor_id.clone()),
            clock: VClock::new(),
            actor: actor_id,
        }
    }
    
    /// Add or update a node
    pub fn add_node(&mut self, node: NodeInfo) {
        // Get context for causal consistency
        let ctx = self.nodes.read_ctx().derive_add_ctx(self.actor.clone());
        
        // Add to set
        self.nodes.apply(self.nodes.add(node, ctx));
        
        // Update vector clock
        self.clock.apply(self.clock.inc(self.actor.clone()));
    }
    
    /// Remove a node
    pub fn remove_node(&mut self, node_id: &NodeId) {
        // Find the node
        if let Some(node) = self.nodes.read().val.iter()
            .find(|n| &n.id == node_id)
            .cloned() {
            
            // Get removal context
            let ctx = self.nodes.read_ctx().derive_rm_ctx();
            
            // Remove from set
            self.nodes.apply(self.nodes.rm(node, ctx));
            
            // Also remove its load assignment
            let rm_ctx = self.load_assignments.read_ctx().derive_rm_ctx();
            self.load_assignments.apply(
                self.load_assignments.rm(node_id.clone(), rm_ctx)
            );
            
            // Update clock
            self.clock.apply(self.clock.inc(self.actor.clone()));
        }
    }
    
    /// Update load assignment for a node
    pub fn update_load_assignment(&mut self, node_id: NodeId, assignment: LoadAssignment) {
        // Create LWW register with assignment
        let dot = self.clock.inc(self.actor.clone());
        let mut reg = LWWReg::new();
        reg.apply(reg.update(assignment, dot.clone()));
        
        // Insert into map
        let ctx = self.load_assignments.read_ctx().derive_add_ctx(self.actor.clone());
        self.load_assignments.apply(
            self.load_assignments.update(node_id, reg, ctx)
        );
        
        // Update clock
        self.clock.apply(dot);
    }
    
    /// The magic merge operation - handles all conflicts automatically!
    pub fn merge(&mut self, other: &Self) {
        self.nodes.merge(other.nodes.clone());
        self.load_assignments.merge(other.load_assignments.clone());
        self.parameters.merge(other.parameters.clone());
        self.total_rps.merge(other.total_rps.clone());
        self.epochs.merge(&other.epochs);
        self.clock.merge(other.clock.clone());
    }
}
```

### Epoch Log Implementation

Custom CRDT built on top of the library primitives:

```rust
/// Append-only log of epochs using Map CRDT
pub struct EpochLog {
    /// Map from epoch ID to epoch data
    epochs: Map<EpochId, Epoch, ActorId>,
    
    /// Ordered index for efficient queries
    ordered_index: RefCell<Vec<EpochId>>,
    
    /// Actor ID
    actor: ActorId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Epoch {
    pub id: EpochId,
    pub timestamp: u64,
    pub event: EpochEvent,
    pub created_by: ActorId,
    pub vector_clock: VClock<ActorId>,
}

impl EpochLog {
    pub fn new(actor: ActorId) -> Self {
        Self {
            epochs: Map::new(),
            ordered_index: RefCell::new(Vec::new()),
            actor,
        }
    }
    
    /// Append new epoch
    pub fn append(&mut self, event: EpochEvent, clock: &VClock<ActorId>) -> EpochId {
        let epoch_id = EpochId::new();
        let epoch = Epoch {
            id: epoch_id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            event,
            created_by: self.actor.clone(),
            vector_clock: clock.clone(),
        };
        
        // Add to map
        let ctx = self.epochs.read_ctx().derive_add_ctx(self.actor.clone());
        self.epochs.apply(
            self.epochs.update(epoch_id.clone(), epoch, ctx)
        );
        
        // Update ordered index
        self.rebuild_index();
        
        epoch_id
    }
    
    /// Rebuild ordered index based on causality
    fn rebuild_index(&self) {
        let mut epochs: Vec<(EpochId, &Epoch)> = self.epochs
            .entries()
            .map(|(id, e)| (id.clone(), e))
            .collect();
        
        // Sort by causality and timestamp
        epochs.sort_by(|a, b| {
            // First compare vector clocks for causality
            match a.1.vector_clock.partial_cmp(&b.1.vector_clock) {
                Some(Ordering::Less) => Ordering::Less,
                Some(Ordering::Greater) => Ordering::Greater,
                _ => {
                    // Concurrent - order by timestamp then ID
                    match a.1.timestamp.cmp(&b.1.timestamp) {
                        Ordering::Equal => a.0.cmp(&b.0),
                        other => other,
                    }
                }
            }
        });
        
        *self.ordered_index.borrow_mut() = epochs.into_iter()
            .map(|(id, _)| id)
            .collect();
    }
    
    /// Get epochs in causal order
    pub fn get_epochs(&self) -> Vec<Epoch> {
        self.ordered_index.borrow()
            .iter()
            .filter_map(|id| self.epochs.get(id).cloned())
            .collect()
    }
    
    /// Merge with another epoch log
    pub fn merge(&mut self, other: &Self) {
        self.epochs.merge(other.epochs.clone());
        self.rebuild_index();
    }
}
```

### Serialization for Network Transport

Efficient serialization for gossip protocol:

```rust
/// Serializable state wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableState {
    pub nodes: Vec<u8>,
    pub load_assignments: Vec<u8>,
    pub parameters: Vec<u8>,
    pub total_rps: Vec<u8>,
    pub epochs: Vec<u8>,
    pub clock: VClock<ActorId>,
    pub actor: ActorId,
}

impl DistributedState {
    /// Serialize for network transport
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let serializable = SerializableState {
            nodes: bincode::serialize(&self.nodes)?,
            load_assignments: bincode::serialize(&self.load_assignments)?,
            parameters: bincode::serialize(&self.parameters)?,
            total_rps: bincode::serialize(&self.total_rps)?,
            epochs: bincode::serialize(&self.epochs)?,
            clock: self.clock.clone(),
            actor: self.actor.clone(),
        };
        
        bincode::serialize(&serializable)
    }
    
    /// Deserialize from network
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let ser: SerializableState = bincode::deserialize(bytes)?;
        
        Ok(Self {
            nodes: bincode::deserialize(&ser.nodes)?,
            load_assignments: bincode::deserialize(&ser.load_assignments)?,
            parameters: bincode::deserialize(&ser.parameters)?,
            total_rps: bincode::deserialize(&ser.total_rps)?,
            epochs: bincode::deserialize(&ser.epochs)?,
            clock: ser.clock,
            actor: ser.actor,
        })
    }
}
```

### Delta State Support

Optimized synchronization with delta states:

```rust
impl DistributedState {
    /// Create a delta containing only changes since the given clock
    pub fn create_delta(&self, their_clock: &VClock<ActorId>) -> DeltaState {
        // The crdts library doesn't have built-in delta support,
        // so we implement a simple version based on vector clocks
        
        DeltaState {
            // Include items with dots newer than their_clock
            nodes: self.filter_newer_nodes(their_clock),
            load_assignments: self.filter_newer_assignments(their_clock),
            parameters: self.filter_newer_parameters(their_clock),
            clock: self.clock.clone(),
            actor: self.actor.clone(),
        }
    }
    
    /// Apply a delta to this state
    pub fn apply_delta(&mut self, delta: DeltaState) {
        // Merge all components
        self.nodes.merge(delta.nodes);
        self.load_assignments.merge(delta.load_assignments);
        self.parameters.merge(delta.parameters);
        self.clock.merge(delta.clock);
    }
}
```

## Usage Examples

### Basic State Management

```rust
// Create state for a node
let mut state = DistributedState::new(ActorId::from("node-1"));

// Add this node
state.add_node(NodeInfo {
    id: NodeId::from("node-1"),
    role: NodeRole::Worker,
    state: NodeState::Active,
    capabilities: NodeCapabilities {
        max_rps: 10_000,
        cpu_cores: 16,
        memory_gb: 32,
    },
    last_heartbeat: current_timestamp(),
});

// Update load assignment
state.update_load_assignment(
    NodeId::from("node-1"),
    LoadAssignment {
        target_rps: 5_000,
        load_proportion: 0.5,
        url_patterns: vec!["/api/*".to_string()],
        assigned_by: ActorId::from("coordinator"),
        fencing_token: 42,
        assigned_at: current_timestamp(),
    }
);

// Create epoch for significant event
state.epochs.append(
    EpochEvent::LoadRedistribution {
        reason: "Node added".to_string(),
    },
    &state.clock
);
```

### State Synchronization

```rust
// On gossip receive
let remote_bytes = receive_gossip_message().await?;
let remote_state = DistributedState::from_bytes(&remote_bytes)?;

// Simple merge - no conflict resolution needed!
local_state.merge(&remote_state);

// State is now consistent
```

## Testing

### Property-Based Tests

```rust
#[test]
fn test_merge_commutative() {
    proptest!(|(
        ops1: Vec<StateOperation>,
        ops2: Vec<StateOperation>
    )| {
        let mut state1 = DistributedState::new(ActorId::from("node-1"));
        let mut state2 = DistributedState::new(ActorId::from("node-2"));
        
        // Apply operations to different states
        for op in ops1 { apply_operation(&mut state1, op); }
        for op in ops2 { apply_operation(&mut state2, op); }
        
        // Merge both ways
        let mut merge1 = state1.clone();
        merge1.merge(&state2);
        
        let mut merge2 = state2.clone();
        merge2.merge(&state1);
        
        // Should be identical
        prop_assert_eq!(merge1.nodes.read(), merge2.nodes.read());
        prop_assert_eq!(merge1.parameters.read(), merge2.parameters.read());
    });
}
```

## Performance Considerations

1. **Merge Complexity**: O(n) where n is number of elements
2. **Memory Usage**: OR-Set can grow with additions
3. **Serialization**: Use compression for large states
4. **Delta Sync**: Reduces network bandwidth significantly

## Best Practices

1. **Actor IDs**: Use stable, unique node identifiers
2. **Timestamps**: Use synchronized clocks when possible
3. **Merge Frequency**: Balance consistency vs. overhead
4. **State Size**: Monitor and potentially garbage collect old epochs