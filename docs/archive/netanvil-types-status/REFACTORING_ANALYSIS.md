# Type Location Refactoring Analysis

## Design Principle: netanvil-types should only contain:
1. **Core traits** that define contracts between crates
2. **Fundamental types** used across multiple crates  
3. **Common enums/constants** shared everywhere
4. **No implementation details** or complex logic

## Types That Should MOVE OUT of netanvil-types

### 1. Profiling Implementation Details → netanvil-profile
Currently in netanvil-types:
- `TransactionAnalysis` (very detailed)
- `KernelEvent`, `KernelEventType` 
- `NetworkEvent`, `NetworkEventType`
- `SyscallTiming`
- `ProfilingSession`, `ProfilingReport`
- `FunctionProfile`, `SyscallStats`

Should keep in netanvil-types:
- `ProfilingCapability` trait (just the interface)
- `ProfilingContext` (simple shared type)

### 2. Session Implementation Details → netanvil-session  
Currently in netanvil-types:
- `Session` (full implementation struct)
- `SessionConfig` (complex configuration)
- `ClientBehavior` (detailed enum with patterns)
- `ThinkTimeConfig`, `ThinkTimeDistribution`
- `PagePattern`, `RetryConfig`, `BackoffStrategy`
- `SessionEvent` (detailed events)

Should keep in netanvil-types:
- `ClientSessionManager` trait
- `SessionId` (simple identifier)
- `SessionState` enum (simple states)

### 3. Connection Implementation Details → netanvil-http
Currently in netanvil-types:
- `ConnectionLifecycleConfig` (detailed config)
- `ConnectionPoolConfig` (detailed config)
- `TlsConfig` (very detailed)
- `Http2Config` (protocol specific)
- `TlsVersion` enum

Should keep in netanvil-types:
- `MaxRequestsSpec` enum (used by multiple crates)
- Basic connection lifecycle enums

### 4. Request Processing Details → Respective Crates
Currently in netanvil-types:
- `RequestBody` with variants → netanvil-http
- `MultipartPart` → netanvil-http
- `TransactionAnalysis` → netanvil-profile
- Detailed error variants → respective crates

Should keep in netanvil-types:
- `RequestSpec` (basic structure)
- `RequestContext` (shared context)
- `ClientIpSpec` (shared concept)

### 5. Results/Metrics Details → netanvil-metrics
Currently in netanvil-types:
- `TestResults` (detailed structure)
- `LatencyPercentiles` (detailed)
- `ErrorBreakdown` (detailed)
- `SaturationSummary` (detailed)

Should keep in netanvil-types:
- Basic metric traits
- Simple metric enums

### 6. Distributed Implementation Details → Respective Crates
Currently in netanvil-types:
- `LoadAssignment` → netanvil-distributed
- `NodeHealth` → netanvil-distributed  
- `PeerInfo`, `PeerState` → netanvil-gossip

Should keep in netanvil-types:
- `NodeId`, `ActorId` (fundamental identifiers)
- `NodeRole`, `NodeState` (simple enums)
- `NodeInfo` (basic info needed everywhere)

## Types That SHOULD STAY in netanvil-types

### 1. Core Traits (Interfaces)
- `ProfilingCapability` 
- `RateController`
- `RequestScheduler`
- `RequestExecutor`
- `RequestGenerator`
- `RequestTransformer`
- `ClientSessionManager`
- `TransactionProfiler`
- `ResultsCollector`
- `SampleRecorder`

### 2. Fundamental Identifiers
- `NodeId`
- `ActorId` 
- `SessionId`
- `MonotonicInstant`

### 3. Core Request Types
- `RequestContext` (needed by all pipeline stages)
- `RequestSpec` (basic spec without body details)
- `ClientContext` (correlation info)
- `ClientIpSpec` (shared across components)
- `UrlSpec` (basic enum)

### 4. Simple Shared Enums
- `NodeRole`, `NodeState`
- `ExecutorType`
- `SaturationAction`
- `MaxRequestsSpec`

### 5. Basic Error Types
- `NetAnvilError` (with basic variants only)
- `Result<T>` type alias

## Proposed New Structure

### netanvil-types (minimal core)
```rust
// Just traits
pub trait RateController: ProfilingCapability + Send + Sync { ... }
pub trait RequestExecutor: ProfilingCapability + Send + Sync { ... }
// ... other traits

// Just identifiers  
pub struct NodeId(String);
pub struct SessionId(String);

// Just basic shared types
pub struct RequestContext { ... }
pub enum ClientIpSpec { ... }

// Just simple enums
pub enum NodeRole { Leader, Worker, Observer }
pub enum SaturationAction { ReduceRate, HoldRate, Stop }
```

### netanvil-profile
```rust
// All profiling implementation details
pub struct TransactionAnalysis { ... }
pub struct KernelEvent { ... }
pub struct ProfilingReport { ... }
// eBPF types, etc.
```

### netanvil-session
```rust
// All session implementation details
pub struct Session { ... }
pub struct SessionConfig { ... }
pub enum ClientBehavior { ... }
// Think time, patterns, etc.
```

### netanvil-http
```rust
// All HTTP implementation details
pub struct ConnectionLifecycleConfig { ... }
pub struct TlsConfig { ... }
pub enum RequestBody { ... }
// Connection pools, etc.
```

## Benefits of This Refactoring

1. **Reduced Coupling**: netanvil-types becomes truly foundational
2. **Faster Compilation**: Smaller core crate compiles faster
3. **Clearer Dependencies**: Each crate owns its implementation details
4. **Better Encapsulation**: Implementation details stay private
5. **Easier Testing**: Test implementation details in their own crates

## Migration Impact

This is a significant refactoring that would:
- Make netanvil-types much smaller and focused
- Require moving many types to their proper crates
- Need careful coordination of imports
- But result in a much cleaner architecture

## Recommendation

**Yes, many complex types should move out of netanvil-types.** The crate should only define:
1. Trait interfaces (contracts)
2. Basic identifiers (NodeId, SessionId)
3. Minimal shared types (RequestContext)
4. Simple enums (NodeRole, etc.)

Everything else should live in the crate that implements it.