# netanvil-gossip Implementation Guide

## Overview

The `netanvil-gossip` crate implements an efficient epidemic-style gossip protocol optimized for Glommio's thread-per-core architecture. It provides reliable state propagation with differential updates and priority message handling.

## Related Design Documents

- [Gossip Protocol Design](../../section-4-5-gossip-protocol-design.md) - Complete protocol specification
- [Distributed Coordination](../../section-4-2-distributed-coordination-design.md) - Integration context
- [CRDT Integration](../../section-4-4-crdt-library-integration.md) - State management

## Key Components

### Core Gossip Engine

Optimized for Glommio's shared-nothing architecture:

```rust
use glommio::prelude::*;

pub struct GossipEngine {
    /// Node identity
    node_id: NodeId,
    
    /// Peer management (single-threaded access)
    peer_manager: Rc<RefCell<PeerManager>>,
    
    /// CRDT state (single-threaded access)
    state: Rc<RefCell<DistributedState>>,
    
    /// Message handler
    message_handler: MessageHandler,
    
    /// Network layer using Glommio
    network: GlommioNet,
    
    /// Differential gossip optimizer
    differential: Rc<RefCell<DifferentialGossip>>,
    
    /// Configuration
    config: GossipConfig,
    
    /// Metrics
    metrics: GossipMetrics,
}

/// Gossip configuration
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Base gossip interval
    pub interval: Duration,
    
    /// Number of peers per round
    pub fanout: usize,
    
    /// Enable differential gossip
    pub differential: bool,
    
    /// Maximum message size
    pub max_message_size: usize,
    
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    
    /// Priority queue depth
    pub priority_queue_size: usize,
}
```

### Glommio Network Layer

Zero-copy networking with io_uring:

```rust
pub struct GlommioNet {
    /// UDP socket for gossip
    socket: Rc<UdpSocket>,
    
    /// Pre-allocated buffers
    buffer_pool: BufferPool,
    
    /// Pending sends
    send_queue: VecDeque<SendRequest>,
    
    /// Receive buffer
    recv_buffer: BytesMut,
}

impl GlommioNet {
    pub async fn new(bind_addr: SocketAddr) -> Result<Self> {
        // Create UDP socket with SO_REUSEPORT for multi-core
        let socket = UdpSocket::bind(bind_addr)?;
        
        // Pre-allocate buffers for zero-copy
        let buffer_pool = BufferPool::new(1024, 65536);
        
        Ok(Self {
            socket: Rc::new(socket),
            buffer_pool,
            send_queue: VecDeque::new(),
            recv_buffer: BytesMut::with_capacity(65536),
        })
    }
    
    /// Send message with zero-copy
    pub async fn send_to(&self, addr: SocketAddr, message: &GossipMessage) -> Result<()> {
        // Get buffer from pool
        let mut buffer = self.buffer_pool.acquire();
        
        // Serialize directly into buffer
        bincode::serialize_into(&mut buffer, message)?;
        
        // Send using io_uring
        self.socket.send_to(buffer.freeze(), addr).await?;
        
        Ok(())
    }
    
    /// Receive with batching
    pub async fn recv_batch(&mut self) -> Result<Vec<(SocketAddr, GossipMessage)>> {
        let mut messages = Vec::new();
        
        // Try to receive multiple messages in one syscall
        loop {
            match self.socket.recv_from(&mut self.recv_buffer).await {
                Ok((len, addr)) => {
                    let message: GossipMessage = 
                        bincode::deserialize(&self.recv_buffer[..len])?;
                    messages.push((addr, message));
                    
                    // Continue if more data available
                    if !self.socket.may_recv() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        
        Ok(messages)
    }
}
```

### Message Types

From the gossip protocol design:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipMessage {
    /// Message ID for deduplication
    pub id: MessageId,
    
    /// Sender node ID
    pub sender_id: NodeId,
    
    /// Message timestamp
    pub timestamp: u64,
    
    /// Vector clock
    pub vector_clock: VClock<ActorId>,
    
    /// Message payload
    pub payload: GossipPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipPayload {
    /// State synchronization
    StateSync {
        state: DistributedState,
        is_delta: bool,
    },
    
    /// State digest for comparison
    StateDigest {
        digests: HashMap<String, u64>,
    },
    
    /// Heartbeat with health info
    Heartbeat {
        health: NodeHealth,
        known_peers: Vec<PeerInfo>,
    },
    
    /// Priority update
    PriorityUpdate {
        update_type: PriorityUpdateType,
        data: Vec<u8>,
    },
}
```

### Peer Management

Efficient peer selection and tracking:

```rust
pub struct PeerManager {
    /// Known peers
    peers: HashMap<NodeId, Peer>,
    
    /// Peer selection strategy
    selection_strategy: Box<dyn PeerSelectionStrategy>,
    
    /// Recent interactions
    interaction_history: HashMap<NodeId, InteractionHistory>,
}

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: NodeId,
    pub address: SocketAddr,
    pub state: PeerState,
    pub last_seen: Instant,
    pub latency_ms: Option<f64>,
}

impl PeerManager {
    /// Select peers for gossip round
    pub fn select_gossip_peers(&self, fanout: usize) -> Vec<&Peer> {
        self.selection_strategy.select(&self.peers, fanout)
    }
    
    /// Update peer state from heartbeat
    pub fn update_peer(&mut self, id: NodeId, heartbeat: &Heartbeat) {
        let peer = self.peers.entry(id.clone()).or_insert_with(|| {
            Peer::new(id, heartbeat.address)
        });
        
        peer.last_seen = Instant::now();
        peer.state = PeerState::from_health(&heartbeat.health);
        
        // Update interaction history
        self.interaction_history
            .entry(id)
            .or_default()
            .record_interaction(true);
    }
}

/// Peer selection strategies
pub trait PeerSelectionStrategy {
    fn select<'a>(&self, peers: &'a HashMap<NodeId, Peer>, count: usize) -> Vec<&'a Peer>;
}

/// Random selection with bias towards healthy peers
pub struct HealthyRandomSelection;

impl PeerSelectionStrategy for HealthyRandomSelection {
    fn select<'a>(&self, peers: &'a HashMap<NodeId, Peer>, count: usize) -> Vec<&'a Peer> {
        let mut candidates: Vec<_> = peers.values()
            .filter(|p| matches!(p.state, PeerState::Healthy | PeerState::Degraded))
            .collect();
        
        // Shuffle and take first `count`
        candidates.shuffle(&mut thread_rng());
        candidates.truncate(count);
        candidates
    }
}
```

### Differential Gossip

Optimize bandwidth with incremental updates:

```rust
pub struct DifferentialGossip {
    /// Version tracking per peer
    peer_versions: HashMap<NodeId, StateVersion>,
    
    /// Our current version
    current_version: StateVersion,
}

#[derive(Debug, Clone, Default)]
pub struct StateVersion {
    pub nodes_version: u64,
    pub assignments_version: u64,
    pub parameters_version: u64,
    pub epochs_version: u64,
}

impl DifferentialGossip {
    /// Create differential update for peer
    pub fn create_delta(
        &self,
        peer_id: &NodeId,
        state: &DistributedState,
    ) -> Option<DistributedState> {
        let peer_version = self.peer_versions.get(peer_id)?;
        
        // Only include changes since peer's version
        if peer_version == &self.current_version {
            return None; // Peer is up to date
        }
        
        // Create delta state
        let delta = state.create_delta(&peer_version.to_vclock());
        Some(delta)
    }
    
    /// Update peer version after successful sync
    pub fn update_peer_version(&mut self, peer_id: NodeId, version: StateVersion) {
        self.peer_versions.insert(peer_id, version);
    }
}
```

### Main Gossip Loop

The core gossip algorithm:

```rust
impl GossipEngine {
    pub async fn run_on_core(&self, core: CoreId) -> Result<()> {
        // Pin to core
        let executor = LocalExecutorBuilder::new()
            .pin_to_cpu(core.0)
            .build()?;
        
        executor.run(async move {
            // Start gossip rounds
            let gossip_task = self.clone().gossip_loop();
            
            // Start message receiver
            let receive_task = self.clone().receive_loop();
            
            // Start heartbeat sender
            let heartbeat_task = self.clone().heartbeat_loop();
            
            // Run all tasks
            futures::join!(gossip_task, receive_task, heartbeat_task);
        }).await
    }
    
    async fn gossip_loop(self: Rc<Self>) {
        let mut interval = Timer::interval(self.config.interval);
        
        loop {
            interval.await;
            
            if let Err(e) = self.gossip_round().await {
                error!("Gossip round failed: {}", e);
            }
        }
    }
    
    async fn gossip_round(&self) -> Result<()> {
        // Select peers
        let peers = self.peer_manager.borrow().select_gossip_peers(self.config.fanout);
        
        // Get current state
        let state = self.state.borrow();
        
        for peer in peers {
            // Create message (differential if enabled)
            let message = if self.config.differential {
                match self.differential.borrow().create_delta(&peer.id, &*state) {
                    Some(delta) => GossipMessage {
                        id: MessageId::new(),
                        sender_id: self.node_id.clone(),
                        timestamp: current_timestamp(),
                        vector_clock: state.clock.clone(),
                        payload: GossipPayload::StateSync {
                            state: delta,
                            is_delta: true,
                        },
                    },
                    None => continue, // Peer is up to date
                }
            } else {
                GossipMessage {
                    id: MessageId::new(),
                    sender_id: self.node_id.clone(),
                    timestamp: current_timestamp(),
                    vector_clock: state.clock.clone(),
                    payload: GossipPayload::StateSync {
                        state: state.clone(),
                        is_delta: false,
                    },
                }
            };
            
            // Send to peer
            self.network.send_to(peer.address, &message).await?;
        }
        
        self.metrics.gossip_rounds.increment();
        Ok(())
    }
    
    async fn receive_loop(self: Rc<Self>) {
        loop {
            // Receive batch of messages
            match self.network.recv_batch().await {
                Ok(messages) => {
                    for (addr, message) in messages {
                        if let Err(e) = self.handle_message(addr, message).await {
                            error!("Failed to handle message: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Receive error: {}", e);
                    Timer::new(Duration::from_millis(100)).await;
                }
            }
        }
    }
    
    async fn handle_message(&self, addr: SocketAddr, message: GossipMessage) -> Result<()> {
        // Deduplicate
        if self.message_handler.is_duplicate(&message.id) {
            return Ok(());
        }
        
        match message.payload {
            GossipPayload::StateSync { state, is_delta } => {
                // Merge state
                if is_delta {
                    self.state.borrow_mut().apply_delta(state);
                } else {
                    self.state.borrow_mut().merge(&state);
                }
                
                // Update peer version
                self.differential.borrow_mut().update_peer_version(
                    message.sender_id.clone(),
                    StateVersion::from_clock(&message.vector_clock),
                );
            }
            
            GossipPayload::Heartbeat { health, known_peers } => {
                // Update peer info
                self.peer_manager.borrow_mut().update_peer(
                    message.sender_id.clone(),
                    &Heartbeat { health, address: addr, known_peers },
                );
            }
            
            // Handle other message types...
        }
        
        Ok(())
    }
}
```

## Performance Optimizations

### Zero-Copy Message Handling

```rust
/// Pre-allocated buffer pool
pub struct BufferPool {
    buffers: RefCell<Vec<BytesMut>>,
    buffer_size: usize,
}

impl BufferPool {
    pub fn acquire(&self) -> BytesMut {
        self.buffers.borrow_mut()
            .pop()
            .unwrap_or_else(|| BytesMut::with_capacity(self.buffer_size))
    }
    
    pub fn release(&self, mut buffer: BytesMut) {
        buffer.clear();
        self.buffers.borrow_mut().push(buffer);
    }
}
```

### Batched Operations

```rust
impl GossipEngine {
    /// Send multiple messages efficiently
    pub async fn send_batch(&self, messages: Vec<(SocketAddr, GossipMessage)>) -> Result<()> {
        // Prepare all messages
        let mut sends = Vec::new();
        
        for (addr, msg) in messages {
            let mut buffer = self.network.buffer_pool.acquire();
            bincode::serialize_into(&mut buffer, &msg)?;
            sends.push((addr, buffer.freeze()));
        }
        
        // Submit all to io_uring in one batch
        for (addr, data) in sends {
            self.network.socket.send_to(data, addr).await?;
        }
        
        Ok(())
    }
}
```

## Testing

### Convergence Tests

```rust
#[test]
async fn test_gossip_convergence() {
    // Create cluster of nodes
    let nodes = create_test_cluster(10).await;
    
    // Make different changes on different nodes
    nodes[0].state.borrow_mut().update_parameter("test", 42.0);
    nodes[5].state.borrow_mut().update_parameter("test2", 100.0);
    
    // Run gossip for a while
    Timer::new(Duration::from_secs(5)).await;
    
    // Verify all nodes converged
    for node in &nodes {
        let state = node.state.borrow();
        assert_eq!(state.parameters.get("test"), Some(42.0));
        assert_eq!(state.parameters.get("test2"), Some(100.0));
    }
}
```

## Configuration Guidelines

```rust
// Recommended configuration for different scenarios
pub fn recommended_config(scenario: Scenario) -> GossipConfig {
    match scenario {
        Scenario::SmallCluster => GossipConfig {
            interval: Duration::from_millis(100),
            fanout: 3,
            differential: true,
            max_message_size: 64 * 1024,
            heartbeat_interval: Duration::from_secs(1),
            priority_queue_size: 100,
        },
        Scenario::LargeCluster => GossipConfig {
            interval: Duration::from_millis(500),
            fanout: 5,
            differential: true,
            max_message_size: 256 * 1024,
            heartbeat_interval: Duration::from_secs(5),
            priority_queue_size: 1000,
        },
    }
}
```