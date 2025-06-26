# Gap Analysis: netanvil-types

## Critical Missing Components

### 1. ProfilingCapability Trait
**Design Requirement**: All major traits must inherit from ProfilingCapability
**Current State**: ProfilingCapability trait does not exist
**Impact**: Cannot integrate profiling across all components as designed

### 2. Missing Core Types

#### Request Processing Types (from section-3-1-request-processing-overview.md)
- `RequestContext` - Runtime information for request generation
- `ClientContext` - Client-specific context for correlation
- `RequestSpec` - Full request specification
- `UrlSpec` - URL specification variants
- `ClientIpSpec` - Client IP simulation specification
- `RequestMetadata` - Request tracking metadata
- `RequestBody` - Request body variants
- `TransactionAnalysis` - Profiling results

#### Distributed Types (from various distributed sections)
- `NodeId` - Strongly typed node identifier
- `ActorId` - Actor identifier for CRDT operations
- `NodeInfo` - Node information structure
- `NodeRole` - Node role enumeration
- `NodeState` - Node state enumeration
- `NodeCapabilities` - Node capability description

### 3. Trait Deficiencies

#### RateController
**Current**:
```rust
pub trait RateController: Send + Sync
```
**Should be**:
```rust
pub trait RateController: ProfilingCapability + Send + Sync
```

#### RequestScheduler
**Missing Methods**:
- Profiling integration methods
- Microsecond precision support
- Glommio-specific scheduling hints

#### RequestExecutor
**Missing**:
- Connection lifecycle management methods
- Client IP preservation methods
- Network condition simulation support
- Detailed error types

### 4. Time Abstraction Issues

#### MonotonicInstant
**Current**: Basic u64 wrapper
**Missing**:
- Platform-specific high-precision timer integration
- Microsecond precision guarantees
- Integration with Glommio's timer facilities

### 5. Missing Async Runtime Types
- No Glommio-specific types or traits
- No thread-per-core affinity types
- No io_uring integration types

### 6. Missing Error Hierarchy
**Current**: Single `NetAnvilError` type
**Missing**:
- Detailed error variants for each subsystem
- Error context and tracing integration
- Retry-able vs non-retry-able error distinction

## Recommendations

1. **Immediate Actions**:
   - Define ProfilingCapability trait
   - Add all missing request processing types
   - Update all traits to inherit from ProfilingCapability

2. **Architecture Alignment**:
   - Add Glommio-specific types and traits
   - Implement proper microsecond-precision time types
   - Add distributed system types (NodeId, ActorId, etc.)

3. **Type Safety Improvements**:
   - Use newtype pattern for IDs (NodeId, ActorId, etc.)
   - Add phantom types for compile-time guarantees
   - Implement proper error types with context