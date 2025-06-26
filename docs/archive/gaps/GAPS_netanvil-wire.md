# Gap Analysis: netanvil-wire

## Critical Missing Components

### 1. Gossip Protocol Messages (from section-4-5-gossip-protocol-design.md)

#### GossipMessage
**Current**:
```rust
pub struct GossipMessage {
    pub id: u64,
    pub sender_id: String,
    pub payload: GossipPayload,
}
```

**Should have**:
```rust
pub struct GossipMessage {
    pub id: MessageId,
    pub sender_id: NodeId,
    pub timestamp: u64,
    pub vector_clock: VClock<ActorId>,
    pub payload: GossipPayload,
}
```

#### GossipPayload
**Current**: Enum variants have no data
**Missing fields**:
- `StateSync` should have `state: DistributedState` and `is_delta: bool`
- `Heartbeat` should have `health: NodeHealth` and `known_peers: Vec<PeerInfo>`
- `StateDigest` should have `digests: HashMap<String, u64>`
- Missing `PriorityUpdate` variant entirely

### 2. Election Protocol Messages (from section-4-6-leader-election-design.md)

#### Missing Fencing Token Support
- No fencing token fields in any election messages
- Critical for preventing split-brain scenarios

#### RequestVote Message
**Missing**:
- `last_log_index: u64`
- `last_log_term: u64`
- `candidate_id: NodeId`

#### Heartbeat Message
**Missing**:
- `leader_id: NodeId`
- `fencing_token: u64`
- `commit_index: u64`

### 3. Missing Message Types

#### Node Discovery Messages
- `DiscoveryBeacon`
- `DiscoveryResponse`
- `NodeAnnouncement`

#### Load Distribution Messages
- `LoadAssignment`
- `LoadReport`
- `LoadQuery`

#### Coordination Messages
- `EpochChange`
- `ParameterUpdate`
- `TestStateChange`

### 4. Missing Common Types

#### Identifiers
- `NodeId` - Should be a newtype, not String
- `MessageId` - Should be UUID or similar
- `ActorId` - For CRDT actor identification

#### State Types
- `DistributedState` - CRDT state representation
- `NodeHealth` - Health status information
- `PeerInfo` - Peer information structure
- `VClock<T>` - Vector clock implementation

### 5. Serialization Issues

- Using basic Serde without schema evolution support
- No versioning strategy for messages
- No compression for large state transfers
- No zero-copy serialization optimization

### 6. Missing Protocol Negotiation

- No capability exchange messages
- No version negotiation support
- No feature discovery mechanism

## Recommendations

1. **Type Safety**:
   - Replace String IDs with proper newtype wrappers
   - Add proper timestamp types (not raw u64)
   - Use strongly typed enums for all state representations

2. **Complete Message Definitions**:
   - Add all fields specified in design docs
   - Include vector clocks for causality tracking
   - Add priority and QoS fields where needed

3. **Protocol Evolution**:
   - Add schema versioning
   - Implement forward/backward compatibility
   - Support gradual rollouts with feature flags

4. **Performance Optimizations**:
   - Implement zero-copy serialization where possible
   - Add compression for large messages
   - Support delta encoding for state synchronization