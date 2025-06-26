# Distributed Coordination System with CRDT-Based State Management

## 1. Introduction

The Distributed Coordination system extends the load testing framework to support multi-node test execution with strong consistency guarantees, automatic failover, and dynamic load rebalancing. This design implements a gossip-based protocol with CRDT state management, providing eventual consistency without requiring centralized coordination.

Key features include:

- **Dual-Role Architecture**: Nodes can dynamically switch between coordinator and worker roles
- **CRDT-Based State**: Conflict-free replicated data types ensure consistent state across nodes
- **Gossip Protocol**: Efficient state propagation with bounded network overhead
- **Leader Election**: Automatic coordinator selection with fencing tokens for split-brain prevention
- **Epoch Management**: Coordinated global state changes with causal ordering
- **Failure Detection**: Probabilistic failure detection with automatic load redistribution
- **Load Balancing**: Dynamic load distribution based on node capacity and health
- **Clock Synchronization**: Vector clocks for causal ordering of events

## 2. Core Interfaces and Types

### 2.1 Node Identity and Roles

```rust
/// Node role in the distributed system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Coordinator role - manages global test state
    Coordinator,
    /// Worker role - generates load
    Worker,
}

/// Node state in the system lifecycle
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// Node is joining the cluster
    Joining,
    /// Node is ready to receive load
    Ready,
    /// Node is actively generating load
    Active,
    /// Node is suspected to be failing
    Failing,
    /// Node has failed
    Failed,
    /// Node is gracefully leaving
    Leaving,
}

/// Complete node information
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Unique node identifier
    pub id: String,
    /// Current role
    pub role: NodeRole,
    /// Current state
    pub state: NodeState,
    /// Node capabilities
    pub capabilities: NodeCapabilities,
    /// Vector clock for this node
    pub vector_clock: VectorClock,
    /// Fencing token (if coordinator)
    pub fencing_token: Option<u64>,
}

/// Extended node capabilities for distributed testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// Maximum RPS this node can generate
    pub max_rps: u64,
    /// Current available capacity (0.0-1.0)
    pub available_capacity: f64,
    /// CPU cores available
    pub cpu_cores: u32,
    /// Memory in MB
    pub memory_mb: u64,
    /// Network bandwidth in Mbps
    pub bandwidth_mbps: u64,
    /// Geographic region
    pub region: Option<String>,
    /// Network latency to target (ms)
    pub target_latency_ms: Option<f64>,
}
```

### 2.2 Distributed Coordinator Interface

```rust
/// Enhanced distributed coordinator with CRDT support
pub trait DistributedCoordinator: Send + Sync {
    /// Initialize node and join cluster
    async fn initialize(&mut self, config: DistributedConfig) -> Result<NodeInfo>;
    
    /// Start gossip protocol
    async fn start_gossip(&self) -> Result<()>;
    
    /// Participate in leader election
    async fn participate_in_election(&self) -> Result<ElectionResult>;
    
    /// Get current coordinator information
    async fn get_coordinator(&self) -> Result<Option<NodeInfo>>;
    
    /// Update node capabilities
    async fn update_capabilities(&self, capabilities: NodeCapabilities) -> Result<()>;
    
    /// Get load assignment for this node
    async fn get_load_assignment(&self) -> Result<LoadAssignment>;
    
    /// Report node metrics
    async fn report_metrics(&self, metrics: NodeMetrics) -> Result<()>;
    
    /// Create new epoch (coordinator only)
    async fn create_epoch(&self, event: EpochEvent) -> Result<EpochId>;
    
    /// Get current epoch
    async fn get_current_epoch(&self) -> Result<Epoch>;
    
    /// Gracefully leave cluster
    async fn leave_cluster(&self) -> Result<()>;
}

/// Load assignment for a node
#[derive(Debug, Clone)]
pub struct LoadAssignment {
    /// Target RPS for this node
    pub target_rps: u64,
    /// Load proportion (0.0-1.0)
    pub load_proportion: f64,
    /// Assigned URL patterns
    pub url_patterns: Vec<String>,
    /// Test parameters
    pub test_parameters: TestParameters,
    /// Assignment timestamp
    pub assigned_at: Instant,
    /// Assigning coordinator's fencing token
    pub fencing_token: u64,
}
```

### 2.3 Message Types

```rust
/// Messages exchanged between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DistributedMessage {
    /// Gossip protocol messages
    Gossip(GossipMessage),
    /// Leader election messages
    Election(ElectionMessage),
    /// Load control messages
    LoadControl(LoadControlMessage),
    /// Node lifecycle messages
    Lifecycle(LifecycleMessage),
    /// Clock synchronization
    ClockSync(ClockSyncMessage),
}

/// Gossip protocol messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    /// State synchronization
    StateSync {
        node_id: String,
        state: CRDTState,
        vector_clock: VectorClock,
    },
    /// Acknowledgment with state
    StateAck {
        node_id: String,
        state: CRDTState,
        vector_clock: VectorClock,
    },
}

/// Leader election messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ElectionMessage {
    /// Propose self as leader
    Proposal {
        node_id: String,
        priority: f64,
        fencing_token: u64,
    },
    /// Accept leadership
    Accept {
        node_id: String,
        leader_id: String,
    },
    /// Reject leadership
    Reject {
        node_id: String,
        leader_id: String,
        reason: String,
    },
    /// Announce leadership
    Announce {
        leader_id: String,
        fencing_token: u64,
    },
}
```

## 3. CRDT State Management

### 3.1 CRDT Implementation with crdts Library

We use the proven `crdts` library for our distributed state management, which provides battle-tested CRDT implementations:

```rust
use crdts::{CmRDT, Map, Orswot, VClock, lwwreg::LWWReg, gcounter::GCounter};

/// Distributed state using the crdts library
pub struct DistributedState {
    /// Node registry - Observed-Remove Set without Tombstones
    pub nodes: Orswot<NodeInfo, String>,
    
    /// Load assignments - Last-Write-Wins Register per node
    pub load_assignments: Map<String, LWWReg<LoadAssignment>, String>,
    
    /// Test parameters - Map of LWW registers
    pub parameters: Map<String, LWWReg<f64>, String>,
    
    /// Epoch log - Custom implementation on Map
    pub epochs: EpochLog,
    
    /// Vector clock for causality
    pub clock: VClock<String>,
    
    /// Actor ID (node ID)
    actor: String,
}

impl DistributedState {
    /// Merge with another state (simple!)
    pub fn merge(&mut self, other: &Self) {
        // All CRDTs handle merge correctly
        self.nodes.merge(other.nodes.clone());
        self.load_assignments.merge(other.load_assignments.clone());
        self.parameters.merge(other.parameters.clone());
        self.epochs.merge(&other.epochs);
        self.clock.merge(other.clock.clone());
    }
    
    /// Add or update a node
    pub fn update_node(&mut self, node: NodeInfo) {
        let ctx = self.nodes.read_ctx().derive_add_ctx(self.actor.clone());
        self.nodes.apply(self.nodes.add(node, ctx));
        self.clock.apply(self.clock.inc(self.actor.clone()));
    }
}
```

### 3.2 Node Registry CRDT (OR-Set)

```rust
/// Node registry using OR-Set CRDT
pub struct NodeRegistryCRDT {
    /// Set of node entries
    entries: HashMap<String, Vec<NodeEntry>>,
}

#[derive(Debug, Clone)]
struct NodeEntry {
    node_id: String,
    state: NodeState,
    capabilities: NodeCapabilities,
    timestamp: Instant,
    unique_tag: Uuid,
}

impl NodeRegistryCRDT {
    /// Add or update node
    pub fn update_node(&mut self, node_id: String, state: NodeState, capabilities: NodeCapabilities) {
        let entry = NodeEntry {
            node_id: node_id.clone(),
            state,
            capabilities,
            timestamp: Instant::now(),
            unique_tag: Uuid::new_v4(),
        };
        
        self.entries.entry(node_id)
            .or_insert_with(Vec::new)
            .push(entry);
    }
    
    /// Get current node state
    pub fn get_node(&self, node_id: &str) -> Option<(NodeState, NodeCapabilities)> {
        self.entries.get(node_id)
            .and_then(|entries| entries.iter()
                .max_by_key(|e| e.timestamp)
                .map(|e| (e.state, e.capabilities.clone())))
    }
    
    /// Get all active nodes
    pub fn get_active_nodes(&self) -> Vec<(String, NodeCapabilities)> {
        self.entries.iter()
            .filter_map(|(id, entries)| {
                entries.iter()
                    .max_by_key(|e| e.timestamp)
                    .filter(|e| matches!(e.state, NodeState::Active | NodeState::Ready))
                    .map(|e| (id.clone(), e.capabilities.clone()))
            })
            .collect()
    }
    
    /// Merge with another registry
    pub fn merge(&mut self, other: &NodeRegistryCRDT) {
        for (node_id, entries) in &other.entries {
            let local_entries = self.entries.entry(node_id.clone())
                .or_insert_with(Vec::new);
            
            // Add entries not already present
            for entry in entries {
                if !local_entries.iter().any(|e| e.unique_tag == entry.unique_tag) {
                    local_entries.push(entry.clone());
                }
            }
        }
    }
}
```

### 3.3 Load Assignments CRDT (LWW-Map)

```rust
/// Load assignments using Last-Write-Wins Map
pub struct LoadAssignmentsCRDT {
    assignments: HashMap<String, TimestampedAssignment>,
}

#[derive(Debug, Clone)]
struct TimestampedAssignment {
    load: f64,
    timestamp: Instant,
    fencing_token: u64,
    vector_clock: VectorClock,
}

impl LoadAssignmentsCRDT {
    /// Update load assignment
    pub fn update_assignment(&mut self, node_id: String, load: f64, fencing_token: u64, vc: VectorClock) {
        let assignment = TimestampedAssignment {
            load,
            timestamp: Instant::now(),
            fencing_token,
            vector_clock: vc,
        };
        
        // Only update if newer or higher fencing token
        match self.assignments.get(&node_id) {
            Some(existing) if existing.fencing_token > fencing_token => return,
            Some(existing) if existing.fencing_token == fencing_token && 
                              existing.timestamp >= assignment.timestamp => return,
            _ => {}
        }
        
        self.assignments.insert(node_id, assignment);
    }
    
    /// Get current assignments
    pub fn get_assignments(&self) -> HashMap<String, f64> {
        self.assignments.iter()
            .map(|(id, a)| (id.clone(), a.load))
            .collect()
    }
    
    /// Merge with another assignments map
    pub fn merge(&mut self, other: &LoadAssignmentsCRDT) {
        for (node_id, assignment) in &other.assignments {
            match self.assignments.get(node_id) {
                Some(existing) => {
                    // Keep assignment with higher fencing token
                    if assignment.fencing_token > existing.fencing_token ||
                       (assignment.fencing_token == existing.fencing_token && 
                        assignment.timestamp > existing.timestamp) {
                        self.assignments.insert(node_id.clone(), assignment.clone());
                    }
                }
                None => {
                    self.assignments.insert(node_id.clone(), assignment.clone());
                }
            }
        }
    }
}
```

## 4. Gossip Protocol Implementation

### 4.1 Gossip Handler

```rust
/// Gossip protocol handler
pub struct GossipProtocol {
    /// Node ID
    node_id: String,
    /// Peer list
    peers: Arc<RwLock<Vec<PeerInfo>>>,
    /// Local CRDT state
    state: Arc<RwLock<CRDTState>>,
    /// Message sender
    message_sender: Arc<dyn MessageSender>,
    /// Gossip configuration
    config: GossipConfig,
    /// Metrics
    metrics: Arc<GossipMetrics>,
}

/// Gossip configuration
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Gossip interval
    pub interval: Duration,
    /// Number of peers to gossip with
    pub fanout: usize,
    /// Enable differential gossip
    pub differential: bool,
    /// Message size limit
    pub max_message_size: usize,
}

impl GossipProtocol {
    /// Start gossip protocol
    pub async fn start(&self) -> Result<()> {
        let protocol = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(protocol.config.interval);
            
            loop {
                interval.tick().await;
                if let Err(e) = protocol.gossip_round().await {
                    error!("Gossip round failed: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    /// Perform one gossip round
    async fn gossip_round(&self) -> Result<()> {
        // Select random peers
        let peers = self.select_peers()?;
        
        // Get current state
        let state = self.state.read().await.clone();
        
        // Send state to selected peers
        for peer in peers {
            let message = if self.config.differential {
                self.create_differential_message(&peer, &state)?
            } else {
                self.create_full_message(&state)?
            };
            
            self.message_sender.send_message(
                &peer.address,
                DistributedMessage::Gossip(message)
            ).await?;
        }
        
        self.metrics.gossip_rounds_completed.increment();
        Ok(())
    }
    
    /// Handle received gossip message
    pub async fn handle_gossip(&self, message: GossipMessage) -> Result<()> {
        match message {
            GossipMessage::StateSync { node_id, state, vector_clock } => {
                // Merge received state
                self.state.write().await.merge(&state);
                
                // Send acknowledgment with our state
                let our_state = self.state.read().await.clone();
                let ack = GossipMessage::StateAck {
                    node_id: self.node_id.clone(),
                    state: our_state,
                    vector_clock: self.get_vector_clock().await,
                };
                
                self.message_sender.send_message(
                    &node_id,
                    DistributedMessage::Gossip(ack)
                ).await?;
            }
            GossipMessage::StateAck { node_id, state, vector_clock } => {
                // Merge acknowledgment state
                self.state.write().await.merge(&state);
            }
        }
        
        Ok(())
    }
}
```

### 4.2 Differential Gossip

```rust
/// Differential gossip state tracking
pub struct DifferentialGossip {
    /// State versions sent to each peer
    peer_versions: HashMap<String, StateVersion>,
}

#[derive(Debug, Clone)]
struct StateVersion {
    nodes_version: u64,
    assignments_version: u64,
    epochs_version: u64,
    parameters_version: u64,
    controller_version: u64,
    last_sync: Instant,
}

impl GossipProtocol {
    /// Create differential message for peer
    fn create_differential_message(&self, peer: &PeerInfo, state: &CRDTState) -> Result<GossipMessage> {
        let version = self.get_peer_version(&peer.id)?;
        
        // Build differential state
        let diff_state = CRDTState {
            nodes: self.get_nodes_diff(&state.nodes, version.nodes_version)?,
            assignments: self.get_assignments_diff(&state.assignments, version.assignments_version)?,
            epochs: self.get_epochs_diff(&state.epochs, version.epochs_version)?,
            parameters: self.get_parameters_diff(&state.parameters, version.parameters_version)?,
            controller: self.get_controller_diff(&state.controller, version.controller_version)?,
        };
        
        Ok(GossipMessage::StateSync {
            node_id: self.node_id.clone(),
            state: diff_state,
            vector_clock: self.get_vector_clock().await,
        })
    }
}
```

## 5. Leader Election Protocol

### 5.1 Election Manager

```rust
/// Leader election manager
pub struct LeaderElection {
    /// Node ID
    node_id: String,
    /// Current role
    role: Arc<AtomicCell<NodeRole>>,
    /// Current coordinator
    coordinator: Arc<RwLock<Option<CoordinatorInfo>>>,
    /// Election state
    election_state: Arc<RwLock<ElectionState>>,
    /// Fencing token generator
    fencing_token: Arc<AtomicU64>,
    /// Message sender
    message_sender: Arc<dyn MessageSender>,
    /// Node capabilities for priority calculation
    capabilities: Arc<RwLock<NodeCapabilities>>,
    /// Election configuration
    config: ElectionConfig,
}

/// Election configuration
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    /// Election timeout
    pub timeout: Duration,
    /// Priority weights
    pub priority_weights: PriorityWeights,
    /// Minimum nodes for election
    pub min_nodes: usize,
}

/// Priority calculation weights
#[derive(Debug, Clone)]
pub struct PriorityWeights {
    /// Weight for available resources
    pub resources: f64,
    /// Weight for stability (uptime)
    pub stability: f64,
    /// Weight for network centrality
    pub centrality: f64,
}

/// Election state
#[derive(Debug, Clone)]
struct ElectionState {
    /// Current election round
    round: u64,
    /// Votes received
    votes: HashMap<String, Vote>,
    /// Start time
    started_at: Instant,
    /// Our priority
    our_priority: f64,
}

impl LeaderElection {
    /// Trigger new election
    pub async fn trigger_election(&self) -> Result<ElectionResult> {
        // Calculate our priority
        let priority = self.calculate_priority().await?;
        
        // Initialize election state
        let mut state = self.election_state.write().await;
        *state = ElectionState {
            round: state.round + 1,
            votes: HashMap::new(),
            started_at: Instant::now(),
            our_priority: priority,
        };
        
        // Generate new fencing token
        let token = self.fencing_token.fetch_add(1, Ordering::SeqCst) + 1;
        
        // Broadcast proposal
        let proposal = ElectionMessage::Proposal {
            node_id: self.node_id.clone(),
            priority,
            fencing_token: token,
        };
        
        self.broadcast_message(DistributedMessage::Election(proposal)).await?;
        
        // Wait for votes
        self.wait_for_votes().await
    }
    
    /// Calculate node priority
    async fn calculate_priority(&self) -> Result<f64> {
        let capabilities = self.capabilities.read().await;
        let weights = &self.config.priority_weights;
        
        // Resource score (normalized)
        let resource_score = capabilities.available_capacity;
        
        // Stability score (based on uptime)
        let stability_score = self.calculate_stability_score()?;
        
        // Network centrality (based on latency)
        let centrality_score = self.calculate_centrality_score(&capabilities)?;
        
        Ok(weights.resources * resource_score +
           weights.stability * stability_score +
           weights.centrality * centrality_score)
    }
    
    /// Handle election message
    pub async fn handle_election(&self, message: ElectionMessage) -> Result<()> {
        match message {
            ElectionMessage::Proposal { node_id, priority, fencing_token } => {
                self.handle_proposal(node_id, priority, fencing_token).await?;
            }
            ElectionMessage::Accept { node_id, leader_id } => {
                self.handle_accept(node_id, leader_id).await?;
            }
            ElectionMessage::Reject { node_id, leader_id, reason } => {
                self.handle_reject(node_id, leader_id, reason).await?;
            }
            ElectionMessage::Announce { leader_id, fencing_token } => {
                self.handle_announce(leader_id, fencing_token).await?;
            }
        }
        Ok(())
    }
    
    /// Handle leadership proposal
    async fn handle_proposal(&self, node_id: String, priority: f64, fencing_token: u64) -> Result<()> {
        let state = self.election_state.read().await;
        
        // Compare priorities
        let response = if priority > state.our_priority ||
                         (priority == state.our_priority && node_id > self.node_id) {
            // Accept their leadership
            ElectionMessage::Accept {
                node_id: self.node_id.clone(),
                leader_id: node_id.clone(),
            }
        } else {
            // Reject their leadership
            ElectionMessage::Reject {
                node_id: self.node_id.clone(),
                leader_id: node_id.clone(),
                reason: "LOWER_PRIORITY".to_string(),
            }
        };
        
        self.message_sender.send_message(
            &node_id,
            DistributedMessage::Election(response)
        ).await
    }
}
```

## 6. Load Management and Distribution

### 6.1 Load Distributor

```rust
/// Load distribution manager for coordinators
pub struct LoadDistributor {
    /// Node registry
    node_registry: Arc<RwLock<NodeRegistryCRDT>>,
    /// Load assignments
    assignments: Arc<RwLock<LoadAssignmentsCRDT>>,
    /// Test parameters
    parameters: Arc<RwLock<TestParametersCRDT>>,
    /// Distribution strategy
    strategy: LoadDistributionStrategy,
    /// Metrics provider
    metrics: Arc<dyn MetricsProvider>,
    /// Fencing token
    fencing_token: Arc<AtomicU64>,
}

/// Load distribution strategies
#[derive(Debug, Clone)]
pub enum LoadDistributionStrategy {
    /// Even distribution across nodes
    Even,
    /// Weighted by capacity
    WeightedCapacity,
    /// Weighted by capacity and latency
    WeightedOptimal,
    /// Geographic awareness
    GeographicAware {
        target_region: String,
        regional_weight: f64,
    },
}

impl LoadDistributor {
    /// Calculate and apply new load distribution
    pub async fn redistribute_load(&self) -> Result<()> {
        // Get active nodes
        let nodes = self.node_registry.read().await.get_active_nodes();
        if nodes.is_empty() {
            return Ok(());
        }
        
        // Get target load
        let target_rps = self.parameters.read().await
            .get_parameter("target_rps")
            .map(|(v, _)| v)
            .unwrap_or(0.0);
        
        // Calculate distribution
        let distribution = match self.strategy {
            LoadDistributionStrategy::Even => {
                self.calculate_even_distribution(&nodes, target_rps)
            }
            LoadDistributionStrategy::WeightedCapacity => {
                self.calculate_weighted_distribution(&nodes, target_rps)
            }
            LoadDistributionStrategy::WeightedOptimal => {
                self.calculate_optimal_distribution(&nodes, target_rps).await?
            }
            LoadDistributionStrategy::GeographicAware { ref target_region, regional_weight } => {
                self.calculate_geographic_distribution(&nodes, target_rps, target_region, regional_weight)
            }
        };
        
        // Apply distribution
        let token = self.fencing_token.load(Ordering::SeqCst);
        let mut assignments = self.assignments.write().await;
        
        for (node_id, load) in distribution {
            assignments.update_assignment(
                node_id,
                load,
                token,
                self.get_vector_clock().await,
            );
        }
        
        Ok(())
    }
    
    /// Calculate weighted distribution by capacity
    fn calculate_weighted_distribution(
        &self,
        nodes: &[(String, NodeCapabilities)],
        target_rps: f64
    ) -> HashMap<String, f64> {
        let total_capacity: f64 = nodes.iter()
            .map(|(_, cap)| cap.max_rps as f64 * cap.available_capacity)
            .sum();
        
        let mut distribution = HashMap::new();
        let mut assigned_total = 0.0;
        
        for (node_id, capabilities) in nodes {
            let node_capacity = capabilities.max_rps as f64 * capabilities.available_capacity;
            let base_load = (node_capacity / total_capacity) * target_rps;
            
            // Apply health factor
            let health_factor = self.calculate_health_factor(node_id);
            let adjusted_load = base_load * health_factor;
            
            // Ensure within limits
            let final_load = adjusted_load.min(node_capacity);
            
            distribution.insert(node_id.clone(), final_load);
            assigned_total += final_load;
        }
        
        // Redistribute remainder if needed
        if assigned_total < target_rps {
            self.redistribute_remainder(&mut distribution, target_rps - assigned_total, nodes);
        }
        
        distribution
    }
    
    /// Calculate health factor for a node
    fn calculate_health_factor(&self, node_id: &str) -> f64 {
        // Get recent metrics for the node
        let metrics = self.metrics.get_node_metrics(node_id);
        
        // Factor in error rate
        let error_factor = 1.0 - metrics.error_rate;
        
        // Factor in saturation
        let saturation_factor = 1.0 - metrics.saturation_level;
        
        // Combined health score
        (error_factor * 0.7 + saturation_factor * 0.3).max(0.1)
    }
}
```

## 7. Epoch Management

### 7.1 Epoch Timeline Manager

```rust
/// Epoch timeline manager
pub struct EpochManager {
    /// Epoch timeline CRDT
    timeline: Arc<RwLock<EpochTimelineCRDT>>,
    /// Current epoch
    current_epoch: Arc<RwLock<Option<Epoch>>>,
    /// Epoch handlers
    handlers: Arc<RwLock<Vec<Box<dyn EpochHandler>>>>,
    /// Fencing token
    fencing_token: Arc<AtomicU64>,
}

/// Epoch event types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EpochEvent {
    /// Node joined
    NodeJoined { node_id: String },
    /// Node failed
    NodeFailed { node_id: String },
    /// Node left gracefully
    NodeLeft { node_id: String },
    /// Load redistribution
    LoadRedistribution { reason: String },
    /// Test phase change
    PhaseChange { from: String, to: String },
    /// Rate change
    RateChange { old_rate: f64, new_rate: f64 },
    /// Parameter update
    ParameterUpdate { param: String, old_value: f64, new_value: f64 },
}

/// Epoch information
#[derive(Debug, Clone)]
pub struct Epoch {
    /// Unique epoch ID
    pub id: Uuid,
    /// Creation timestamp
    pub timestamp: Instant,
    /// Triggering event
    pub event: EpochEvent,
    /// Vector clock at creation
    pub vector_clock: VectorClock,
    /// Creating coordinator's fencing token
    pub fencing_token: u64,
    /// System snapshot
    pub snapshot: SystemSnapshot,
}

/// System snapshot at epoch creation
#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    /// Active nodes
    pub active_nodes: Vec<String>,
    /// Load distribution
    pub load_distribution: HashMap<String, f64>,
    /// Total RPS
    pub total_rps: f64,
    /// Test parameters
    pub parameters: HashMap<String, f64>,
}

impl EpochManager {
    /// Create new epoch
    pub async fn create_epoch(&self, event: EpochEvent) -> Result<Uuid> {
        // Only coordinator can create epochs
        if !self.is_coordinator() {
            return Err("Only coordinator can create epochs".into());
        }
        
        // Create system snapshot
        let snapshot = self.create_snapshot().await?;
        
        // Create epoch
        let epoch = Epoch {
            id: Uuid::new_v4(),
            timestamp: Instant::now(),
            event: event.clone(),
            vector_clock: self.get_vector_clock().await,
            fencing_token: self.fencing_token.load(Ordering::SeqCst),
            snapshot,
        };
        
        // Add to timeline
        self.timeline.write().await.append(epoch.clone());
        
        // Update current epoch
        *self.current_epoch.write().await = Some(epoch.clone());
        
        // Notify handlers
        self.notify_handlers(&epoch).await?;
        
        Ok(epoch.id)
    }
    
    /// Register epoch handler
    pub async fn register_handler(&self, handler: Box<dyn EpochHandler>) {
        self.handlers.write().await.push(handler);
    }
}

/// Epoch handler trait
#[async_trait]
pub trait EpochHandler: Send + Sync {
    /// Handle new epoch
    async fn handle_epoch(&self, epoch: &Epoch) -> Result<()>;
}
```

## 8. Integration with Rate Control

### 8.1 Distributed Rate Controller Adapter

```rust
/// Adapter to make rate controllers distributed-aware
pub struct DistributedRateControllerAdapter<C: RateController> {
    /// Underlying rate controller
    controller: Arc<C>,
    /// Distributed coordinator
    coordinator: Arc<dyn DistributedCoordinator>,
    /// Load assignment
    assignment: Arc<RwLock<Option<LoadAssignment>>>,
    /// Metrics reporter
    metrics_reporter: Arc<MetricsReporter>,
    /// Update interval
    update_interval: Duration,
}

impl<C: RateController> DistributedRateControllerAdapter<C> {
    /// Start distributed operation
    pub async fn start(&self) -> Result<()> {
        // Start metrics reporting
        let reporter = self.metrics_reporter.clone();
        let coordinator = self.coordinator.clone();
        let interval = self.update_interval;
        
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            
            loop {
                timer.tick().await;
                if let Err(e) = reporter.report_to_coordinator(&coordinator).await {
                    error!("Failed to report metrics: {}", e);
                }
            }
        });
        
        // Start assignment monitoring
        let adapter = self.clone();
        tokio::spawn(async move {
            adapter.monitor_assignments().await;
        });
        
        Ok(())
    }
    
    /// Monitor for assignment changes
    async fn monitor_assignments(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        
        loop {
            interval.tick().await;
            
            // Get current assignment
            match self.coordinator.get_load_assignment().await {
                Ok(new_assignment) => {
                    let mut assignment = self.assignment.write().await;
                    
                    // Check if assignment changed
                    let changed = match &*assignment {
                        Some(current) => current.fencing_token != new_assignment.fencing_token,
                        None => true,
                    };
                    
                    if changed {
                        // Apply new assignment
                        self.apply_assignment(&new_assignment).await;
                        *assignment = Some(new_assignment);
                    }
                }
                Err(e) => {
                    error!("Failed to get load assignment: {}", e);
                }
            }
        }
    }
    
    /// Apply load assignment
    async fn apply_assignment(&self, assignment: &LoadAssignment) {
        // Update controller parameters based on assignment
        if let Some(external_controller) = self.controller.as_external_signal_controller() {
            external_controller.update_signal("distributed_target_rps", assignment.target_rps as f64);
            external_controller.update_signal("load_proportion", assignment.load_proportion);
        }
        
        info!("Applied new load assignment: {} RPS ({}% of total)", 
              assignment.target_rps, 
              assignment.load_proportion * 100.0);
    }
}

impl<C: RateController> RateController for DistributedRateControllerAdapter<C> {
    fn get_current_rps(&self) -> u64 {
        match &*self.assignment.read().unwrap() {
            Some(assignment) => {
                // Use assigned RPS
                assignment.target_rps
            }
            None => {
                // Fall back to underlying controller
                self.controller.get_current_rps()
            }
        }
    }
    
    fn update(&self, metrics: &RequestMetrics) {
        // Update underlying controller
        self.controller.update(metrics);
        
        // Report metrics asynchronously
        let reporter = self.metrics_reporter.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let _ = reporter.record_metrics(metrics).await;
        });
    }
}
```

## 9. Clock Synchronization

### 9.1 Vector Clock Implementation

```rust
/// Vector clock for causal ordering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorClock {
    /// Clock values indexed by node ID
    clocks: HashMap<String, u64>,
}

impl VectorClock {
    /// Create new vector clock
    pub fn new(node_id: String) -> Self {
        let mut clocks = HashMap::new();
        clocks.insert(node_id, 0);
        Self { clocks }
    }
    
    /// Increment local clock
    pub fn increment(&mut self, node_id: &str) {
        *self.clocks.entry(node_id.to_string()).or_insert(0) += 1;
    }
    
    /// Update with received clock
    pub fn update(&mut self, other: &VectorClock, local_node_id: &str) {
        // Take maximum of each position
        for (node_id, &clock) in &other.clocks {
            let local = self.clocks.entry(node_id.clone()).or_insert(0);
            *local = (*local).max(clock);
        }
        
        // Increment local position
        self.increment(local_node_id);
    }
    
    /// Check if this happens-before other
    pub fn happens_before(&self, other: &VectorClock) -> bool {
        // All components must be <= and at least one <
        let mut all_leq = true;
        let mut exists_lt = false;
        
        for (node_id, &clock) in &self.clocks {
            match other.clocks.get(node_id) {
                Some(&other_clock) => {
                    if clock > other_clock {
                        all_leq = false;
                        break;
                    }
                    if clock < other_clock {
                        exists_lt = true;
                    }
                }
                None if clock > 0 => {
                    all_leq = false;
                    break;
                }
                _ => {}
            }
        }
        
        // Check for clocks in other not in self
        for (node_id, &clock) in &other.clocks {
            if !self.clocks.contains_key(node_id) && clock > 0 {
                exists_lt = true;
            }
        }
        
        all_leq && exists_lt
    }
    
    /// Check if concurrent with other
    pub fn concurrent_with(&self, other: &VectorClock) -> bool {
        !self.happens_before(other) && !other.happens_before(self)
    }
}
```

### 9.2 Clock Synchronization Protocol

```rust
/// Clock synchronization manager
pub struct ClockSynchronizer {
    /// Local node ID
    node_id: String,
    /// Vector clock
    vector_clock: Arc<RwLock<VectorClock>>,
    /// Physical time mapping
    time_mapping: Arc<RwLock<TimeMapping>>,
    /// Message sender
    message_sender: Arc<dyn MessageSender>,
    /// Synchronization interval
    sync_interval: Duration,
}

/// Mapping between vector clocks and physical time
#[derive(Debug, Clone)]
struct TimeMapping {
    /// Vector clock to timestamp mappings
    mappings: Vec<(VectorClock, Instant)>,
    /// Estimated clock skew with peers
    peer_skew: HashMap<String, Duration>,
}

impl ClockSynchronizer {
    /// Start synchronization
    pub async fn start(&self) -> Result<()> {
        let sync = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(sync.sync_interval);
            
            loop {
                interval.tick().await;
                if let Err(e) = sync.sync_round().await {
                    error!("Clock sync failed: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    /// Perform synchronization round
    async fn sync_round(&self) -> Result<()> {
        // Select random peers
        let peers = self.select_sync_peers()?;
        
        for peer in peers {
            let request = ClockSyncMessage::Request {
                node_id: self.node_id.clone(),
                local_time: Instant::now(),
                vector_clock: self.vector_clock.read().await.clone(),
            };
            
            self.message_sender.send_message(
                &peer,
                DistributedMessage::ClockSync(request)
            ).await?;
        }
        
        Ok(())
    }
}
```

## 10. Failure Detection and Recovery

### 10.1 Failure Detector

```rust
/// Failure detection with Phi Accrual
pub struct FailureDetector {
    /// Node ID
    node_id: String,
    /// Monitored nodes
    monitors: Arc<RwLock<HashMap<String, NodeMonitor>>>,
    /// Failure threshold (phi value)
    threshold: f64,
    /// Coordinator reference
    coordinator: Arc<dyn DistributedCoordinator>,
    /// Detection interval
    detection_interval: Duration,
}

/// Node monitoring state
#[derive(Debug, Clone)]
struct NodeMonitor {
    /// Heartbeat history
    heartbeats: VecDeque<Instant>,
    /// Last heartbeat time
    last_heartbeat: Instant,
    /// Phi value
    phi: f64,
    /// Suspected flag
    suspected: bool,
    /// Failure reports from other nodes
    failure_reports: HashSet<String>,
}

impl FailureDetector {
    /// Start failure detection
    pub async fn start(&self) -> Result<()> {
        let detector = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(detector.detection_interval);
            
            loop {
                interval.tick().await;
                if let Err(e) = detector.detect_failures().await {
                    error!("Failure detection error: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    /// Detect failures
    async fn detect_failures(&self) -> Result<()> {
        let mut monitors = self.monitors.write().await;
        let now = Instant::now();
        
        for (node_id, monitor) in monitors.iter_mut() {
            // Calculate phi value
            monitor.phi = self.calculate_phi(monitor, now);
            
            // Check if we should suspect failure
            if monitor.phi > self.threshold && !monitor.suspected {
                monitor.suspected = true;
                
                // Broadcast suspicion
                let message = LifecycleMessage::FailureSuspected {
                    reporter_id: self.node_id.clone(),
                    suspected_id: node_id.clone(),
                    evidence: FailureEvidence {
                        phi_value: monitor.phi,
                        last_heartbeat: monitor.last_heartbeat,
                        reporter_count: monitor.failure_reports.len(),
                    },
                };
                
                self.coordinator.broadcast_message(
                    DistributedMessage::Lifecycle(message)
                ).await?;
            }
        }
        
        Ok(())
    }
    
    /// Calculate Phi Accrual value
    fn calculate_phi(&self, monitor: &NodeMonitor, now: Instant) -> f64 {
        if monitor.heartbeats.len() < 2 {
            return 0.0;
        }
        
        // Calculate mean and variance of heartbeat intervals
        let intervals: Vec<f64> = monitor.heartbeats.windows(2)
            .map(|w| (w[1] - w[0]).as_secs_f64())
            .collect();
        
        let mean = intervals.iter().sum::<f64>() / intervals.len() as f64;
        let variance = intervals.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f64>() / intervals.len() as f64;
        
        let std_dev = variance.sqrt();
        
        // Time since last heartbeat
        let elapsed = (now - monitor.last_heartbeat).as_secs_f64();
        
        // Phi calculation
        -((elapsed - mean) / std_dev).ln()
    }
}
```

## 11. Usage Examples

### 11.1 Basic Distributed Test Setup

```rust
// Example: Setting up a distributed load test
async fn setup_distributed_test() -> Result<()> {
    // Create distributed configuration
    let config = DistributedConfig {
        node_id: format!("node-{}", Uuid::new_v4()),
        gossip: GossipConfig {
            interval: Duration::from_millis(100),
            fanout: 3,
            differential: true,
            max_message_size: 1024 * 1024, // 1MB
        },
        election: ElectionConfig {
            timeout: Duration::from_secs(5),
            priority_weights: PriorityWeights {
                resources: 0.5,
                stability: 0.3,
                centrality: 0.2,
            },
            min_nodes: 2,
        },
        failure_detection: FailureDetectionConfig {
            phi_threshold: 8.0,
            detection_interval: Duration::from_millis(500),
        },
    };
    
    // Create coordinator
    let mut coordinator = GossipBasedCoordinator::new(config);
    
    // Initialize and join cluster
    let node_info = coordinator.initialize().await?;
    println!("Joined cluster as: {:?}", node_info);
    
    // Start protocols
    coordinator.start_gossip().await?;
    
    // Participate in election
    let election_result = coordinator.participate_in_election().await?;
    println!("Election result: {:?}", election_result);
    
    // Create rate controller with distributed adapter
    let base_controller = create_pid_controller();
    let distributed_controller = DistributedRateControllerAdapter::new(
        base_controller,
        Arc::new(coordinator),
        Duration::from_secs(1),
    );
    
    // Start distributed operation
    distributed_controller.start().await?;
    
    Ok(())
}
```

### 11.2 Handling Node Failures

```rust
// Example: Coordinator handling node failure
async fn handle_node_failure(coordinator: &GossipBasedCoordinator) -> Result<()> {
    // Register failure handler
    let epoch_manager = coordinator.get_epoch_manager();
    
    struct FailureHandler {
        load_distributor: Arc<LoadDistributor>,
    }
    
    #[async_trait]
    impl EpochHandler for FailureHandler {
        async fn handle_epoch(&self, epoch: &Epoch) -> Result<()> {
            match &epoch.event {
                EpochEvent::NodeFailed { node_id } => {
                    println!("Node {} failed, redistributing load", node_id);
                    self.load_distributor.redistribute_load().await?;
                }
                _ => {}
            }
            Ok(())
        }
    }
    
    let handler = Box::new(FailureHandler {
        load_distributor: coordinator.get_load_distributor(),
    });
    
    epoch_manager.register_handler(handler).await;
    
    Ok(())
}
```

### 11.3 Dynamic Load Adjustment

```rust
// Example: Adjusting load based on global metrics
async fn adjust_load_dynamically(coordinator: &GossipBasedCoordinator) -> Result<()> {
    // Monitor global metrics
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    
    loop {
        interval.tick().await;
        
        // Get global status
        let status = coordinator.get_global_status().await?;
        
        // Check if we need to adjust
        if status.total_error_rate > 0.05 {
            // Reduce load by 10%
            let new_target = status.total_rps as f64 * 0.9;
            
            // Update parameters
            coordinator.update_parameter("target_rps", new_target).await?;
            
            // Create epoch for rate change
            coordinator.create_epoch(EpochEvent::RateChange {
                old_rate: status.total_rps as f64,
                new_rate: new_target,
            }).await?;
            
            println!("Reduced load to {} RPS due to high error rate", new_target);
        }
    }
}
```

## 12. Conclusion

This distributed coordination system provides a robust foundation for multi-node load testing with the following key benefits:

1. **Decentralized Architecture**: No single point of failure with automatic coordinator election
2. **Consistent State**: CRDT-based state management ensures eventual consistency
3. **Efficient Communication**: Gossip protocol with differential updates minimizes network overhead
4. **Automatic Failover**: Failure detection and automatic load redistribution
5. **Coordinated Changes**: Epoch management for synchronized global state changes
6. **Type-Safe Integration**: Seamless integration with existing rate control components
7. **Flexible Load Distribution**: Multiple strategies for optimal load allocation

The system scales from small clusters to large distributed deployments while maintaining consistency and providing strong operational guarantees through fencing tokens and vector clocks.