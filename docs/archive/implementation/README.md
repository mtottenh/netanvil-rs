# Comprehensive Crate Architecture and Implementation Plan for NetAnvil-RS

## Executive Summary

Based on the extensive design documentation, I propose a modular crate architecture that separates concerns, maximizes testability, and enables both local and distributed load testing. The design standardizes on Glommio for optimal thread-per-core execution and includes a dedicated high-precision timer subsystem.

The architecture comprises 20 specialized crates organized in layers:
- **Foundation Layer**: Core types, wire protocols, and timing infrastructure
- **Runtime Layer**: Glommio-based thread-per-core execution
- **Metrics Layer**: Lock-free collection and profiling
- **Load Testing Core**: HTTP clients, rate control, and session management
- **Distributed Components**: CRDT state, gossip protocol, and coordination
- **Interface Layer**: External API, job scheduling, client SDKs, and CLI
- **Testing Support**: Shared utilities and simulation frameworks

## Crate Architecture

### Foundation Layer (Zero or Minimal Dependencies)

#### `netanvil-types` (v0.1.0)
**Purpose**: Core types, traits, and error definitions shared across all crates.
**Dependencies**: None (only std)
**Key Contents**:
```rust
- Core traits: RateController, RequestScheduler, RequestExecutor, etc.
- Common types: RequestTicket, RequestMetrics, CompletedRequest
- Error types: NetAnvilError, Result<T>
- Time abstractions: MonotonicInstant, Duration helpers
- Configuration types: LoadTestConfig, DistributedConfig
```

#### `netanvil-wire` (v0.1.0)
**Purpose**: Wire protocol definitions and serialization for distributed components.
**Dependencies**: `serde`, `bincode`
**Key Contents**:
```rust
- Protocol messages: GossipMessage, ElectionMessage, LoadControlMessage
- Serialization helpers
- Version negotiation
- Message framing
```

### Core Infrastructure Layer

#### `netanvil-timer` (v0.1.0)
**Purpose**: High-precision timer implementation for microsecond-accurate scheduling.
**Dependencies**: `libc` (for platform-specific calls), `netanvil-types`
**Key Contents**:
```rust
// Core timer abstraction
pub trait PrecisionTimer: Send + Sync {
    fn now(&self) -> MonotonicInstant;
    fn sleep_until(&self, deadline: MonotonicInstant);
    fn spin_wait_until(&self, deadline: MonotonicInstant);
}

// Platform-specific implementations
pub struct LinuxTimer {
    // CLOCK_MONOTONIC_RAW for stability
    // CPU TSC for ultra-low overhead when available
}

pub struct AdaptiveTimer {
    // Combines sleep + spin-wait for optimal precision
    // Configurable spin threshold (e.g., 10μs)
}

// Integration with Glommio
pub struct GlommioTimer {
    // Leverages io_uring for timer operations
    // Integrates with Glommio's reactor
}
```

### Runtime Layer

#### `netanvil-runtime` (v0.1.0)
**Purpose**: Glommio-based runtime with thread-per-core architecture.
**Dependencies**: `glommio`, `netanvil-types`, `netanvil-timer`
**Key Contents**:
```rust
// Thread-per-core runtime management
pub struct NetAnvilRuntime {
    executors: Vec<LocalExecutor>,
    core_mapping: HashMap<CoreId, ExecutorHandle>,
}

impl NetAnvilRuntime {
    pub fn new(core_config: CoreConfig) -> Result<Self> {
        // Pin executors to specific cores
        // Set up io_uring with optimal parameters
        // Configure CPU affinity
    }
    
    pub fn spawn_on_core<F>(&self, core: CoreId, task: F) -> TaskHandle
    where F: Future + 'static;
    
    pub fn spawn_timer_on_core(&self, core: CoreId) -> Result<()> {
        // Dedicated timer executor on specific core
    }
}

// Core assignment strategies
pub enum CoreAssignment {
    // Dedicate cores to specific roles
    Dedicated {
        timer_cores: Vec<CoreId>,
        scheduler_cores: Vec<CoreId>,
        executor_cores: Vec<CoreId>,
        gossip_cores: Vec<CoreId>,
    },
    // Share cores with priorities
    Shared {
        core_weights: HashMap<CoreId, CoreWeight>,
    },
}
```

### Metrics and Monitoring Layer

#### `netanvil-metrics` (v0.1.0)
**Purpose**: High-performance metrics collection and aggregation.
**Dependencies**: `hdrhistogram`, `atomic`, `netanvil-types`
**Key Contents**:
```rust
- Lock-free metrics collectors
- Histogram management
- Time-series aggregation
- Metric exporters (Prometheus, StatsD)
- Sampling strategies
```

#### `netanvil-profile` (v0.2.0)
**Purpose**: Advanced profiling capabilities including eBPF support.
**Dependencies**: `libbpf-rs` (optional), `netanvil-types`, `netanvil-metrics`
**Key Contents**:
```rust
- Transaction profiler trait
- Runtime profiler (async task tracking)
- eBPF profiler (Linux-only, feature-gated)
- Profiling data aggregation
- Saturation detection
```

### Load Testing Core Components

#### `netanvil-http` (v0.1.0)
**Purpose**: HTTP client implementation optimized for load testing.
**Dependencies**: `hyper`, `rustls`, `netanvil-types`, `netanvil-runtime`
**Key Contents**:
```rust
- Connection pool management
- HTTP/1.1 and HTTP/2 support
- Request/response handling
- TLS configuration
- Platform-specific optimizations
```

#### `netanvil-control` (v0.1.0)
**Purpose**: Rate controllers and request schedulers.
**Dependencies**: `netanvil-types`, `netanvil-runtime`, `netanvil-metrics`, `netanvil-timer`
**Key Contents**:
```rust
// Rate Controllers
- StaticController
- LinearRampController
- StepController
- PIDController
- SaturationAwarePIDController

// Schedulers with microsecond precision
pub struct PrecisionScheduler {
    timer: Arc<dyn PrecisionTimer>,
    rate_controller: Arc<dyn RateController>,
}

impl PrecisionScheduler {
    async fn scheduling_loop(&self) {
        let mut next_tick = self.timer.now();
        loop {
            // Calculate next request time
            let interval = self.calculate_interval();
            next_tick = next_tick + interval;
            
            // Use adaptive timer for precision
            self.timer.sleep_until(next_tick).await;
            
            // Dispatch request ticket
            self.dispatch_ticket(next_tick);
        }
    }
}
```

#### `netanvil-session` (v0.1.0)
**Purpose**: Client session management and behavior modeling.
**Dependencies**: `netanvil-types`, `rand`
**Key Contents**:
```rust
- Session lifecycle management
- Client behavior models (Browser, Mobile, API)
- Think time simulation
- Navigation patterns
- Connection limits
```

### Distributed System Components

#### `netanvil-crdt` (v0.1.0)
**Purpose**: CRDT-based distributed state management.
**Dependencies**: `crdts`, `netanvil-types`, `netanvil-wire`
**Key Contents**:
```rust
pub struct DistributedState {
    // Node registry using Orswot
    nodes: Orswot<NodeInfo, ActorId>,
    
    // Load assignments using LWWReg
    assignments: Map<NodeId, LWWReg<LoadAssignment>, ActorId>,
    
    // Parameters using LWWReg
    parameters: Map<String, LWWReg<f64>, ActorId>,
    
    // Custom epoch log
    epochs: EpochLog,
}

// Clean merge operation
impl DistributedState {
    pub fn merge(&mut self, other: &Self) {
        self.nodes.merge(other.nodes.clone());
        self.assignments.merge(other.assignments.clone());
        // ...
    }
}
```

#### `netanvil-gossip` (v0.1.0)
**Purpose**: Efficient gossip protocol implementation optimized for Glommio.
**Dependencies**: `netanvil-types`, `netanvil-wire`, `netanvil-runtime`, `netanvil-crdt`
**Key Contents**:
```rust
pub struct GossipEngine {
    // Peer management with shared-nothing design
    peer_manager: Rc<RefCell<PeerManager>>,
    
    // Zero-copy message handling
    message_handler: MessageHandler,
    
    // State synchronization
    state: Rc<RefCell<DistributedState>>,
    
    // Differential gossip optimization
    differential: DifferentialGossip,
    
    // Glommio networking
    network: GlommioNet,
}

impl GossipEngine {
    pub async fn run_on_core(&self, core: CoreId) -> Result<()> {
        // Run gossip protocol on dedicated core
        // Use Glommio's efficient UDP support
        // Leverage io_uring for batched sends
    }
}
```

#### `netanvil-discovery` (v0.1.0)
**Purpose**: Node discovery mechanisms.
**Dependencies**: `netanvil-types`, `netanvil-wire`
**Key Contents**:
```rust
- Static discovery (configuration-based)
- mDNS discovery
- Cloud provider integration (AWS, GCP, Azure)
- Kubernetes discovery
```

#### `netanvil-election` (v0.1.0)
**Purpose**: Leader election and role management.
**Dependencies**: `netanvil-types`, `netanvil-wire`, `netanvil-crdt`
**Key Contents**:
```rust
- Raft-based leader election
- Priority calculation
- Fencing tokens
- Split-brain prevention
```

### Orchestration Layer

#### `netanvil-core` (v0.1.0)
**Purpose**: Local load test orchestration.
**Dependencies**: All local testing components
**Key Contents**:
```rust
pub struct LoadTest {
    controller: Arc<dyn RateController>,
    scheduler: Arc<dyn RequestScheduler>,
    executor: Arc<dyn RequestExecutor>,
    collector: Arc<ResultsCollector>,
    
    // Optional components
    profiler: Option<Arc<dyn TransactionProfiler>>,
    session_manager: Option<Arc<ClientSessionManager>>,
}

// Builder pattern for construction
pub struct LoadTestBuilder { /* ... */ }
```

#### `netanvil-distributed` (v0.2.0)
**Purpose**: Distributed load test coordination.
**Dependencies**: All distributed components, `netanvil-core`
**Key Contents**:
```rust
pub struct DistributedCoordinator {
    // Local components
    local_test: LoadTest,
    
    // Distributed components
    gossip: GossipEngine,
    state: DistributedState,
    election: LeaderElection,
    
    // Role management
    role: AtomicCell<NodeRole>,
}
```

### Interface Layer

#### `netanvil-api` (v0.1.0)
**Purpose**: External API server providing gRPC and REST interfaces.
**Dependencies**: `tonic`, `warp`, `netanvil-core`, `netanvil-distributed`, `netanvil-scheduler`
**Key Contents**:
```rust
// gRPC services
- TestManagementService
- ClusterManagementService
- MetricsService
- ResourceMonitoringService
- JobManagementService

// REST gateway with WebSocket support
- OpenAPI specification
- Server-sent events for streaming
- Authentication/authorization middleware
- Rate limiting
```

#### `netanvil-scheduler` (v0.1.0)
**Purpose**: Job scheduling and lifecycle management.
**Dependencies**: `netanvil-types`, `netanvil-crdt`, `netanvil-core`, `netanvil-distributed`
**Key Contents**:
```rust
// Job scheduling with CRDT-based queue
pub struct JobScheduler {
    state: Arc<RwLock<JobSchedulerState>>,
    executor: Arc<JobExecutor>,
    resource_manager: Arc<ResourceManager>,
}

// Features
- Single test execution guarantee
- Priority-based scheduling
- Recurring job support
- Job dependencies
- Resource validation
- Notification system
```

#### `netanvil-client` (v0.1.0)
**Purpose**: Rust client SDK for programmatic access.
**Dependencies**: `tonic`, `tokio-tungstenite`, `netanvil-types`
**Key Contents**:
```rust
// Core client interface
pub struct NetAnvilClient {
    grpc_client: GrpcClient,
    rest_client: Option<RestClient>,
    ws_manager: WebSocketManager,
}

// Features
- Fluent API with builder pattern
- Automatic reconnection
- Streaming support
- Multi-transport fallback
```

#### `netanvil-cli` (v0.1.0)
**Purpose**: Command-line interface.
**Dependencies**: `clap`, `netanvil-client`, `tui`
**Key Contents**:
```rust
- CLI argument parsing
- Configuration file support
- Interactive TUI dashboard
- Result visualization
- Job management commands
- Authentication management
- Plugin system
```

### Testing Support

#### `netanvil-test-utils` (v0.1.0)
**Purpose**: Shared testing utilities.
**Dependencies**: `proptest`, `tokio-test`, `netanvil-types`
**Key Contents**:
```rust
- Mock implementations of core traits
- Deterministic schedulers for testing
- Network simulation helpers
- CRDT property testing utilities
- Discrete event simulation framework
```

## Phased Implementation Plan

### Phase 1: Foundation (Weeks 1-3)
**Goal**: Establish core types, timer system, and Glommio runtime.

**Deliverables**:
1. `netanvil-types` crate with all trait definitions
2. `netanvil-timer` with platform-specific implementations
3. `netanvil-runtime` with Glommio thread-per-core setup
4. `netanvil-metrics` with lock-free collectors
5. `netanvil-wire` with protocol definitions

**Testing**:
- Unit tests for all types
- Timer precision benchmarks (<1μs jitter target)
- Core affinity verification
- Lock-free metrics benchmarks

### Phase 2: Local Load Testing Core (Weeks 4-7)
**Goal**: Implement single-node load testing capabilities.

**Deliverables**:
1. `netanvil-http` with connection pooling
2. `netanvil-control` with basic controllers
3. `netanvil-session` with client models
4. `netanvil-core` orchestration

**Testing**:
- Integration tests for HTTP client
- Controller behavior tests
- End-to-end local load tests
- Performance benchmarks

### Phase 3: Distributed Foundation (Weeks 8-10)
**Goal**: Implement CRDT state and gossip protocol.

**Deliverables**:
1. `netanvil-crdt` with state management
2. `netanvil-gossip` with push-pull protocol
3. `netanvil-discovery` with static discovery

**Testing**:
- CRDT property tests
- Gossip convergence tests
- Network partition tests
- Discrete event simulation

### Phase 4: Distributed Coordination (Weeks 11-13)
**Goal**: Complete distributed system implementation.

**Deliverables**:
1. `netanvil-election` with leader election
2. `netanvil-distributed` coordinator
3. Integration with local components

**Testing**:
- Leader election scenarios
- Distributed load test integration
- Failure recovery tests
- Scale testing

### Phase 5: External API and Job Scheduling (Weeks 14-16)
**Goal**: Implement external interfaces and job management.

**Deliverables**:
1. `netanvil-api` with gRPC and REST services
2. `netanvil-scheduler` with job queue management
3. `netanvil-client` Rust SDK
4. Basic CLI using client SDK

**Testing**:
- API integration tests
- Job scheduling scenarios
- Client SDK tests
- End-to-end job lifecycle tests

### Phase 6: Advanced Features (Weeks 17-19)
**Goal**: Add profiling and advanced capabilities.

**Deliverables**:
1. `netanvil-profile` with eBPF support
2. Advanced schedulers in `netanvil-control`
3. Cloud discovery in `netanvil-discovery`
4. Enhanced CLI features with TUI dashboard
5. Multi-language SDK bindings (Python, Go, TypeScript)

**Testing**:
- Profiling accuracy tests
- Platform-specific tests
- Cloud integration tests
- SDK language binding tests

### Phase 7: Optimization and Polish (Weeks 20-22)
**Goal**: Performance optimization and production readiness.

**Activities**:
1. Performance profiling and optimization
2. Documentation completion
3. Example scenarios
4. Release preparation
5. Docker/Kubernetes deployment manifests
6. CI/CD pipeline setup

## Key Design Decisions

### 1. Glommio-First Architecture
- **Decision**: Standardize on Glommio for all async runtime needs
- **Rationale**: Thread-per-core model ideal for high-performance networking; io_uring provides superior I/O performance
- **Implementation**: Dedicated cores for timer, scheduling, execution, and gossip

### 2. Dedicated Timer Subsystem
- **Decision**: Separate `netanvil-timer` crate for precision timing
- **Rationale**: Microsecond-accurate scheduling requires platform-specific optimizations
- **Implementation**: Adaptive sleep+spin, TSC when available, io_uring timers

### 3. CRDT Library Usage
- **Decision**: Use proven `crdts` library instead of custom implementation
- **Rationale**: Correctness is critical; the library is well-tested
- **Trade-off**: Less control but much faster development

### 4. Modular Architecture
- **Decision**: Many small crates instead of monolithic design
- **Rationale**: Better testability, cleaner dependencies, easier to understand
- **Trade-off**: More complex build process

### 5. Performance-First Design
- **Decision**: Lock-free algorithms on hot paths
- **Rationale**: Load testing tools must not become the bottleneck
- **Implementation**: Atomic operations, careful memory ordering

### 6. Shared-Nothing Design
- **Decision**: Each core owns its data, minimize cross-core communication
- **Rationale**: Eliminates cache contention and synchronization overhead
- **Implementation**: Use Rc/RefCell within cores, channels between cores

## Testing Strategy

### Unit Testing
- Each crate has comprehensive unit tests
- Property-based testing for algorithms
- Mocking at trait boundaries

### Integration Testing
- Test crates interact correctly
- Network simulation for distributed components
- Failure injection testing

### Performance Testing
- Micro-benchmarks for hot paths
- End-to-end load test benchmarks
- Profiling to identify bottlenecks

### Simulation Testing
- Discrete event simulation for distributed algorithms
- Model checking for critical paths
- Chaos testing for failure scenarios

## Performance Considerations

### Hot Path Optimization
1. **Zero-allocation scheduling**: Pre-allocate all memory
2. **Lock-free metrics**: Atomic operations only
3. **Efficient serialization**: Custom binary protocol
4. **CPU affinity**: Pin threads to cores

### Glommio Core Architecture
```rust
// Example: Optimal core assignment for 16-core system
pub struct CoreLayout {
    // Timer on isolated core for best precision
    timer_core: CoreId(0),
    
    // Rate control and scheduling
    control_cores: vec![CoreId(1), CoreId(2)],
    
    // HTTP request execution (majority of cores)
    executor_cores: vec![CoreId(3)..CoreId(12)],
    
    // Metrics aggregation
    metrics_core: CoreId(13),
    
    // Distributed: gossip and state management
    gossip_core: CoreId(14),
    election_core: CoreId(15),
}

// High-performance scheduler with dedicated timer
pub async fn run_precision_scheduler(
    core: CoreId,
    timer: Arc<GlommioTimer>,
    rate_controller: Arc<dyn RateController>,
) -> Result<()> {
    LocalExecutorBuilder::new()
        .pin_to_cpu(core.0)
        .ring_depth(4096)  // Large ring for timer operations
        .build()?
        .run(async move {
            let scheduler = PrecisionScheduler::new(timer, rate_controller);
            scheduler.run_forever().await
        })
}
```

## Migration Strategy

1. Move existing code to `./legacy` directory
2. Create new workspace with proposed crate structure
3. Implement phase by phase, validating against legacy
4. Gradual migration of functionality
5. Deprecate legacy once feature parity achieved

## Success Metrics

1. **Performance**: 1M+ RPS from single node
2. **Accuracy**: <1μs scheduling jitter
3. **Scalability**: Linear scaling to 100+ nodes
4. **Reliability**: 99.99% uptime in distributed mode
5. **Usability**: <5 minute setup for basic test

This architecture provides a solid foundation for building a world-class load testing framework that can scale from simple single-node tests to massive distributed load generation while maintaining accuracy and performance.