# Type Distribution Guide for NetAnvil-RS

## Design Principle
Each crate should own its implementation details. The `netanvil-types` crate should only contain:
- **Trait definitions** (contracts between crates)
- **Basic identifiers** (NodeId, SessionId, ActorId)
- **Minimal shared types** (RequestContext)
- **Simple shared enums** (NodeRole, NodeState)

## Type Distribution by Crate

### netanvil-types (Foundation)
```rust
// Core traits only (no implementation)
trait RateController: ProfilingCapability + Send + Sync
trait RequestScheduler: ProfilingCapability + Send + Sync
trait RequestExecutor: ProfilingCapability + Send + Sync
// ... other traits

// Basic identifiers
struct NodeId(String)
struct ActorId(String)  
struct SessionId(String)

// Minimal shared types
struct RequestContext { request_id, timestamp, is_sampled, session_id }
enum ClientIpSpec { Static(IpAddr), SessionBased }
struct MonotonicInstant(u64)

// Simple shared enums
enum NodeRole { Leader, Worker, Observer }
enum NodeState { Starting, Active, Degraded, Draining }
enum SaturationAction { ReduceRate(f64), HoldRate, Stop }
enum MaxRequestsSpec { Unlimited, Fixed(u32), Range(u32, u32) }
```

### netanvil-profile (Profiling Implementation)
```rust
// All profiling details
struct TransactionAnalysis { kernel_events, network_events, syscalls, ... }
struct KernelEvent { timestamp, event_type, context }
enum KernelEventType { TcpStateChange, SocketOp, ContextSwitch, ... }
struct NetworkEvent { timestamp, event_type, bytes }
struct SyscallTiming { syscall, start, duration, return_value }
struct ProfilingReport { session_id, duration, transactions, ... }
struct ProfilingSession { id, started_at }

// eBPF specific types
struct BpfProgram { ... }
struct BpfEvent { ... }
```

### netanvil-session (Session Management)
```rust
// All session details
struct Session { id, state, data, created_at, ... }
struct SessionConfig { duration, think_time, behavior, ... }
enum ClientBehavior { Human { patterns, abandonment }, Bot { rps }, Api { retry } }
struct ThinkTimeConfig { Fixed(Duration), Random(min, max), Distribution(...) }
struct PagePattern { url_pattern, probability, dependencies }
struct RetryConfig { max_attempts, backoff }
enum BackoffStrategy { Fixed(Duration), Exponential { ... } }

// Session implementation
impl ClientSessionManager for SessionManagerImpl { ... }
```

### netanvil-http (HTTP Execution)
```rust
// HTTP-specific types
struct RequestSpec { method, url, headers, body, metadata }
enum RequestBody { Bytes(Vec<u8>), Json(String), Form(...), Multipart(...) }
struct MultipartPart { name, content_type, data }
struct CompletedRequest { id, start, end, status, headers, body, ... }

// Connection management
struct ConnectionLifecycleConfig { max_requests, timeouts, ... }
struct ConnectionPoolConfig { max_per_host, ttl, idle_timeout, ... }
struct TlsConfig { min_version, max_version, ciphers, certs, ... }
struct Http2Config { max_streams, window_size, ... }

// Executor implementation
impl RequestExecutor for HttpExecutor { ... }
```

### netanvil-metrics (Metrics Collection)
```rust
// All metrics details
struct RequestMetrics { total, successful, failed, latencies, ... }
struct SaturationMetrics { queue_depth, pending, dropped, ... }
struct TestResults { duration, requests, latencies, errors, ... }
struct LatencyPercentiles { p50, p90, p95, p99, p999, max }
struct ErrorBreakdown { connection, timeout, client, server, other }

// Metrics implementation
impl ResultsCollector for MetricsCollector { ... }
```

### netanvil-control (Rate Control & Scheduling)
```rust
// Controller implementations
struct StaticController { target_rps }
struct PidController { target, kp, ki, kd, integral, last_error }
struct AdaptiveController { base_controller, saturation_threshold }

// Scheduler implementations  
struct ConstantRateScheduler { rps, last_tick }
struct PoissonScheduler { mean_interval, rng }
struct BurstScheduler { burst_size, burst_interval }

// Implementations
impl RateController for StaticController { ... }
impl RequestScheduler for ConstantRateScheduler { ... }
```

### netanvil-crdt (Distributed State)
```rust
// CRDT-specific types
struct DistributedState { nodes: Orswot<NodeInfo>, assignments: Map<...>, ... }
struct NodeInfo { id, address, role, state, capabilities, heartbeat }
struct LoadAssignment { target_rps, proportion, patterns, fencing_token }
struct NodeCapabilities { max_rps, cpu_cores, memory_gb, region }
struct VectorClock<T> { ... }
struct CausalQueue<T> { ... }
```

### netanvil-gossip (Gossip Protocol)
```rust
// Gossip-specific types
struct GossipMessage { id, sender, timestamp, clock, payload }
enum GossipPayload { StateSync, Heartbeat, StateDigest, PriorityUpdate }
struct PeerInfo { node_id, address, last_seen, state }
enum PeerState { Healthy, Degraded, Suspected, Failed }
struct GossipConfig { interval, fanout, differential, ... }

// Implementation
struct GossipEngine { ... }
```

### netanvil-distributed (Coordination)
```rust
// Coordination types
struct DistributedCoordinator { node_id, role, state, gossip, election, ... }
struct LoadDistributor { strategy, current_distribution }
struct FailureDetector { heartbeat_interval, timeout, suspected_nodes }
struct NodeHealth { cpu, memory, network, connections, queue_depth }

// Implementation
impl DistributedCoordinator { ... }
```

## Benefits of This Distribution

1. **Clear Ownership**: Each crate owns its domain
2. **Minimal Dependencies**: netanvil-types has minimal deps
3. **Fast Compilation**: Changes don't cascade unnecessarily
4. **Better Encapsulation**: Implementation details are private
5. **Easier Testing**: Test close to implementation
6. **Type Safety**: Still maintain strong typing across boundaries

## Migration Strategy

1. Start with netanvil-types containing only traits and basic types
2. As each crate is implemented, move relevant types there
3. Use associated types in traits for flexibility
4. Keep public APIs minimal - expose only what's needed

## Example: How Types Flow

```
User Request → RequestContext (netanvil-types)
           ↓
Generator → RequestSpec (netanvil-http)
           ↓ 
Transformer → RequestSpec with session data (netanvil-session)
           ↓
Executor → CompletedRequest (netanvil-http)
           ↓
Collector → RequestMetrics (netanvil-metrics)
```

Each stage uses types from netanvil-types for coordination but owns its implementation types.