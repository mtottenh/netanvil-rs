# CRDT Implementation with crdts Library

## 1. Introduction

This document shows how to leverage the `crdts` library to implement distributed state management for our load testing system. By using this battle-tested library, we can focus on our domain-specific logic rather than reimplementing CRDT algorithms.

## 2. Library Overview

The `crdts` crate provides:
- **Proven CRDT implementations**: GCounter, PNCounter, LWWReg, MVReg, Orswot, Map, etc.
- **Actor-based design**: Each CRDT operation is tagged with an actor ID
- **Causal consistency**: Built-in dot/version tracking
- **Efficient merging**: Optimized implementations
- **Type safety**: Strongly typed operations

## 3. Mapping to Our Requirements

### 3.1 CRDT Type Mapping

```rust
use crdts::{
    CmRDT, CvRDT, Dot, Map, Orswot, VClock,
    lwwreg::LWWReg, gcounter::GCounter
};

/// Our domain-specific CRDT collection
pub struct DistributedState {
    /// Node registry - Observed-Remove Set without Tombstones
    pub nodes: Orswot<NodeInfo, String>,
    
    /// Load assignments - Last-Write-Wins Register per node
    pub load_assignments: Map<String, LWWReg<LoadAssignment>, String>,
    
    /// Test parameters - Map of LWW registers
    pub parameters: Map<String, LWWReg<f64>, String>,
    
    /// Total RPS counter - Grow-only counter
    pub total_rps: GCounter<String>,
    
    /// Vector clock for causality
    pub clock: VClock<String>,
    
    /// Actor ID (node ID)
    actor: String,
}

/// Node information stored in CRDT
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: String,
    pub state: NodeState,
    pub capabilities: NodeCapabilities,
    pub last_seen: u64, // Unix timestamp
}

/// Load assignment for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadAssignment {
    pub target_rps: u64,
    pub url_patterns: Vec<String>,
    pub assigned_by: String,
    pub fencing_token: u64,
}
```

### 3.2 Basic Operations

```rust
impl DistributedState {
    /// Create new state for a node
    pub fn new(actor_id: String) -> Self {
        Self {
            nodes: Orswot::new(),
            load_assignments: Map::new(),
            parameters: Map::new(),
            total_rps: GCounter::new(),
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
    pub fn remove_node(&mut self, node_id: &str) {
        // Find the node
        if let Some(node) = self.nodes.read().val.iter()
            .find(|n| n.id == node_id)
            .cloned() {
            
            // Get removal context
            let ctx = self.nodes.read_ctx().derive_rm_ctx();
            
            // Remove from set
            self.nodes.apply(self.nodes.rm(node, ctx));
            
            // Also remove its load assignment
            self.load_assignments.apply(
                self.load_assignments.rm(node_id.to_string(), self.load_assignments.read_ctx())
            );
            
            // Update clock
            self.clock.apply(self.clock.inc(self.actor.clone()));
        }
    }
    
    /// Update load assignment
    pub fn update_load_assignment(&mut self, node_id: String, assignment: LoadAssignment) {
        // Create LWW register with assignment
        let mut reg = LWWReg::new();
        let dot = self.clock.dot(self.actor.clone());
        reg.apply(reg.update(assignment, dot));
        
        // Insert into map
        let ctx = self.load_assignments.read_ctx().derive_add_ctx(self.actor.clone());
        self.load_assignments.apply(
            self.load_assignments.update(node_id, reg, ctx)
        );
        
        // Update clock
        self.clock.apply(self.clock.inc(self.actor.clone()));
    }
    
    /// Update parameter
    pub fn update_parameter(&mut self, key: String, value: f64) {
        // Create LWW register
        let mut reg = LWWReg::new();
        let dot = self.clock.dot(self.actor.clone());
        reg.apply(reg.update(value, dot));
        
        // Update in map
        let ctx = self.parameters.read_ctx().derive_add_ctx(self.actor.clone());
        self.parameters.apply(
            self.parameters.update(key, reg, ctx)
        );
        
        // Update clock
        self.clock.apply(self.clock.inc(self.actor.clone()));
    }
    
    /// Get active nodes
    pub fn get_active_nodes(&self) -> Vec<&NodeInfo> {
        self.nodes.read().val.iter()
            .filter(|n| matches!(n.state, NodeState::Active | NodeState::Ready))
            .collect()
    }
    
    /// Get load assignment for a node
    pub fn get_load_assignment(&self, node_id: &str) -> Option<&LoadAssignment> {
        self.load_assignments.get(node_id)
            .and_then(|reg| reg.read().val.as_ref())
    }
}
```

## 4. Merging and Synchronization

### 4.1 State Merging

```rust
impl DistributedState {
    /// Merge with another state
    pub fn merge(&mut self, other: &Self) {
        // Merge all CRDTs
        self.nodes.merge(other.nodes.clone());
        self.load_assignments.merge(other.load_assignments.clone());
        self.parameters.merge(other.parameters.clone());
        self.total_rps.merge(other.total_rps.clone());
        self.clock.merge(other.clock.clone());
    }
    
    /// Create a delta state for efficient sync
    pub fn create_delta(&self, their_clock: &VClock<String>) -> DeltaState {
        // The crdts library doesn't have built-in delta support,
        // so we'll implement a simple version based on vector clocks
        
        DeltaState {
            // Include items that have dots newer than their_clock
            nodes: self.filter_newer_nodes(their_clock),
            load_assignments: self.filter_newer_assignments(their_clock),
            parameters: self.filter_newer_parameters(their_clock),
            clock: self.clock.clone(),
        }
    }
    
    fn filter_newer_nodes(&self, their_clock: &VClock<String>) -> Orswot<NodeInfo, String> {
        // Create new Orswot with only newer items
        // This is a simplified approach - in practice you'd track dots per item
        let mut newer = Orswot::new();
        
        for node in self.nodes.read().val.iter() {
            // Check if this node was added after their_clock
            if self.is_newer_than(their_clock) {
                let ctx = newer.read_ctx().derive_add_ctx(self.actor.clone());
                newer.apply(newer.add(node.clone(), ctx));
            }
        }
        
        newer
    }
    
    fn is_newer_than(&self, their_clock: &VClock<String>) -> bool {
        // Check if our clock has any events they haven't seen
        self.clock.partial_cmp(their_clock) == Some(std::cmp::Ordering::Greater)
    }
}
```

### 4.2 Serialization for Network Transport

```rust
use serde::{Serialize, Deserialize};

/// Serializable state for network transport
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableState {
    pub nodes: Vec<u8>,
    pub load_assignments: Vec<u8>,
    pub parameters: Vec<u8>,
    pub total_rps: Vec<u8>,
    pub clock: VClock<String>,
    pub actor: String,
}

impl DistributedState {
    /// Serialize for network transport
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let serializable = SerializableState {
            nodes: bincode::serialize(&self.nodes)?,
            load_assignments: bincode::serialize(&self.load_assignments)?,
            parameters: bincode::serialize(&self.parameters)?,
            total_rps: bincode::serialize(&self.total_rps)?,
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
            clock: ser.clock,
            actor: ser.actor,
        })
    }
}
```

## 5. Epoch Management Using CRDTs

Since the `crdts` library doesn't have a built-in append-only log, we can build one using the primitives:

```rust
use crdts::{Map, Dot, CmRDT};
use std::cmp::Ordering;

/// Epoch event stored in the log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Epoch {
    pub id: Uuid,
    pub timestamp: u64,
    pub event: EpochEvent,
    pub created_by: String,
    pub dot: Dot<String>, // For causal ordering
}

/// Append-only log built on Map CRDT
pub struct EpochLog {
    /// Map from epoch ID to epoch
    epochs: Map<Uuid, Epoch, String>,
    /// Ordered index for efficient queries
    ordered_index: Vec<Uuid>,
}

impl EpochLog {
    pub fn new() -> Self {
        Self {
            epochs: Map::new(),
            ordered_index: Vec::new(),
        }
    }
    
    /// Append new epoch
    pub fn append(&mut self, event: EpochEvent, actor: String, clock: &VClock<String>) {
        let epoch = Epoch {
            id: Uuid::new_v4(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            event,
            created_by: actor.clone(),
            dot: clock.dot(actor.clone()),
        };
        
        // Add to map
        let ctx = self.epochs.read_ctx().derive_add_ctx(actor);
        self.epochs.apply(
            self.epochs.update(epoch.id, epoch.clone(), ctx)
        );
        
        // Update ordered index
        self.rebuild_index();
    }
    
    /// Rebuild ordered index based on causality
    fn rebuild_index(&mut self) {
        let mut epochs: Vec<(Uuid, &Epoch)> = self.epochs.entries()
            .map(|(id, e)| (*id, e))
            .collect();
        
        // Sort by causality and timestamp
        epochs.sort_by(|a, b| {
            // First compare dots for causality
            match a.1.dot.partial_cmp(&b.1.dot) {
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
        
        self.ordered_index = epochs.into_iter()
            .map(|(id, _)| id)
            .collect();
    }
    
    /// Get epochs in causal order
    pub fn get_epochs(&self) -> Vec<&Epoch> {
        self.ordered_index.iter()
            .filter_map(|id| self.epochs.get(id))
            .collect()
    }
}
```

## 6. Integration with Existing System

### 6.1 Gossip Protocol Integration

```rust
/// Wrapper for gossip protocol
pub struct CRDTGossipHandler {
    state: Arc<RwLock<DistributedState>>,
    epoch_log: Arc<RwLock<EpochLog>>,
}

impl CRDTGossipHandler {
    /// Handle incoming state sync
    pub async fn handle_state_sync(&self, remote_state: DistributedState) {
        let mut local = self.state.write().await;
        
        // Merge is commutative and idempotent
        local.merge(&remote_state);
        
        // No need for complex conflict resolution!
    }
    
    /// Create state for gossip
    pub async fn prepare_gossip_state(&self) -> Vec<u8> {
        let state = self.state.read().await;
        state.to_bytes().unwrap()
    }
}
```

### 6.2 Coordinator Integration

```rust
impl Coordinator {
    /// Update load distribution
    pub async fn update_load_distribution(&self, distribution: HashMap<String, u64>) {
        let mut state = self.state.write().await;
        
        for (node_id, target_rps) in distribution {
            let assignment = LoadAssignment {
                target_rps,
                url_patterns: self.get_url_patterns_for_node(&node_id),
                assigned_by: self.node_id.clone(),
                fencing_token: self.current_fencing_token(),
            };
            
            state.update_load_assignment(node_id, assignment);
        }
        
        // Changes will propagate via gossip automatically
    }
    
    /// Create new epoch
    pub async fn create_epoch(&self, event: EpochEvent) {
        let state = self.state.read().await;
        let mut epochs = self.epoch_log.write().await;
        
        epochs.append(event, self.node_id.clone(), &state.clock);
        
        // Epoch will propagate via gossip
    }
}
```

## 7. Advantages of Using crdts Library

### 7.1 Simplified Implementation

```rust
// Before: Complex manual CRDT implementation
pub struct NodeRegistryCRDT {
    entries: HashMap<String, Vec<NodeEntry>>,
    version: Version,
    tombstones: HashSet<(String, Uuid)>,
    // ... lots of manual merge logic
}

// After: Simple and correct
pub struct DistributedState {
    nodes: Orswot<NodeInfo, String>, // That's it!
}
```

### 7.2 Proven Correctness

- The library has been tested and used in production
- Implements proven CRDT algorithms correctly
- Handles edge cases we might miss

### 7.3 Better Performance

- Optimized implementations
- Efficient merge operations
- Minimal memory overhead

## 8. Migration Path

### 8.1 Gradual Migration

```rust
/// Adapter to use during migration
pub struct CRDTAdapter {
    /// New implementation
    new_state: DistributedState,
    /// Old implementation (for comparison)
    old_state: Option<OldCRDTState>,
}

impl CRDTAdapter {
    /// Add node using new implementation
    pub fn add_node(&mut self, node: NodeInfo) {
        self.new_state.add_node(node.clone());
        
        // During migration, also update old state
        if let Some(ref mut old) = self.old_state {
            old.update_node(/* ... */);
            
            // Verify consistency
            debug_assert_eq!(
                self.new_state.get_active_nodes().len(),
                old.get_active_nodes().len()
            );
        }
    }
}
```

### 8.2 Testing Strategy

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crdts::quickcheck::{quickcheck, TestResult};
    
    #[test]
    fn test_merge_commutative() {
        fn prop(ops1: Vec<TestOp>, ops2: Vec<TestOp>) -> TestResult {
            let mut state1 = DistributedState::new("node1".to_string());
            let mut state2 = DistributedState::new("node2".to_string());
            
            // Apply ops to different states
            for op in ops1 { apply_op(&mut state1, op); }
            for op in ops2 { apply_op(&mut state2, op); }
            
            // Merge both ways
            let mut merge1 = state1.clone();
            merge1.merge(&state2);
            
            let mut merge2 = state2.clone();
            merge2.merge(&state1);
            
            // Should be identical
            TestResult::from_bool(
                merge1.nodes.read() == merge2.nodes.read()
            )
        }
        
        quickcheck(prop as fn(Vec<TestOp>, Vec<TestOp>) -> TestResult);
    }
}
```

## 9. Configuration

```toml
# Cargo.toml
[dependencies]
crdts = "7.3"
serde = { version = "1.0", features = ["derive"] }
bincode = "1.3"
uuid = { version = "1.0", features = ["serde"] }
```

## 10. Summary

Using the `crdts` library provides:

1. **Reduced Complexity**: No need to implement CRDT algorithms
2. **Proven Correctness**: Battle-tested implementations
3. **Type Safety**: Strongly typed operations with actor IDs
4. **Easy Integration**: Simple merge operations for gossip
5. **Good Performance**: Optimized implementations

The main trade-offs are:
- Less control over internal implementation
- Need to adapt our data model to fit available CRDT types
- May need to implement some custom types (like epoch log)

Overall, using this library will significantly reduce implementation time and improve reliability.