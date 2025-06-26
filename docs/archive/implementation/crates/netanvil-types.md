# netanvil-types Implementation Guide

## Overview

The `netanvil-types` crate provides the foundational types, traits, and error definitions used throughout the NetAnvil-RS framework. This is a zero-dependency crate that defines the contract between all other components.

## Related Design Documents

- [System Design Overview](../../SystemsDesign.md) - Core trait definitions (Section 2)
- [Architecture Patterns](../../section-1-architecture.md) - Type design principles
- [Distributed Types](../../section-4-2-distributed-coordination-design.md) - Distributed system types

## Key Components

### Core Traits

These traits define the primary abstractions of the load testing framework:

```rust
// From SystemsDesign.md Section 2.1
pub trait RateController: Send + Sync {
    fn get_current_rps(&self) -> u64;
    fn update(&self, metrics: &RequestMetrics);
    fn update_with_saturation(&self, metrics: &RequestMetrics, saturation: &SaturationMetrics);
    fn get_rate_history(&self) -> Vec<(u64, u64)>;
    fn get_saturation_adjustments(&self) -> Vec<(u64, SaturationAction)>;
}

// From SystemsDesign.md Section 2.2
pub trait RequestScheduler: Send + Sync {
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<dyn RateController>,
        ticket_tx: mpsc::Sender<RequestTicket>,
        metrics_rx: mpsc::Receiver<RequestMetrics>,
        profiler: Option<Arc<dyn TransactionProfiler>>,
    );
}

// Additional traits from SystemsDesign.md
pub trait RequestExecutor: Send + Sync { /* ... */ }
pub trait SampleRecorder: Send + Sync { /* ... */ }
pub trait ResultReporter: Send + Sync { /* ... */ }
pub trait TransactionProfiler: Send + Sync { /* ... */ }
```

### Common Types

Fundamental data structures used across the system:

```rust
// Request scheduling and execution
#[derive(Debug, Clone)]
pub struct RequestTicket {
    pub id: u64,
    pub scheduled_time: MonotonicInstant,
    pub url_index: usize,
    pub is_warmup: bool,
}

#[derive(Debug, Clone)]
pub struct RequestMetrics {
    pub timestamp: u64,
    pub window_size_ms: u64,
    pub request_count: u64,
    pub p50_latency_us: u64,
    pub p90_latency_us: u64,
    pub p99_latency_us: u64,
    pub error_rate: f64,
    pub current_throughput: f64,
}

// Saturation detection (from eBPFProfiling.md)
#[derive(Debug, Clone)]
pub struct SaturationMetrics {
    pub avg_overhead_percentage: f64,
    pub p95_overhead_percentage: f64,
    pub avg_read_delay_us: f64,
    pub p95_read_delay_us: u64,
    pub level: SaturationLevel,
    pub recommendation: SaturationAction,
}
```

### Time Abstractions

Critical for microsecond-precision scheduling:

```rust
/// Monotonic instant that is guaranteed to never go backwards
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonotonicInstant(u64); // nanoseconds since arbitrary epoch

impl MonotonicInstant {
    pub fn now() -> Self;
    pub fn elapsed(&self) -> Duration;
    pub fn duration_since(&self, earlier: Self) -> Duration;
    pub fn checked_add(&self, duration: Duration) -> Option<Self>;
}

/// Duration helpers for common operations
pub mod duration_utils {
    pub fn micros(n: u64) -> Duration;
    pub fn nanos(n: u64) -> Duration;
    pub fn as_micros_f64(d: Duration) -> f64;
}
```

### Configuration Types

From various design documents:

```rust
// Core load test configuration
#[derive(Debug, Clone)]
pub struct LoadTestConfig {
    pub initial_rps: u64,
    pub min_rps: u64,
    pub max_rps: u64,
    pub test_duration_secs: u64,
    pub max_concurrent: usize,
    pub urls: Vec<String>,
    pub cache_warmup_time: u64,
    pub rate_control_mode: RateControlMode,
    pub sampling_config: SamplingConfig,
    pub profiling_config: Option<ProfilingConfig>,
    pub distributed_config: Option<DistributedConfig>,
}

// From section-4-2-distributed-coordination-design.md
#[derive(Debug, Clone)]
pub struct DistributedConfig {
    pub node_id: String,
    pub gossip: GossipConfig,
    pub election: ElectionConfig,
    pub failure_detection: FailureDetectionConfig,
}
```

### Error Types

Comprehensive error handling:

```rust
#[derive(Debug, thiserror::Error)]
pub enum NetAnvilError {
    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),
    
    #[error("Client saturation detected: {0}")]
    ClientSaturation(String),
    
    #[error("Network error: {0}")]
    Network(#[from] std::io::Error),
    
    #[error("Configuration error: {0}")]
    Configuration(String),
    
    #[error("Distributed coordination error: {0}")]
    Coordination(String),
    
    // Additional variants...
}

pub type Result<T> = std::result::Result<T, NetAnvilError>;
```

## Implementation Guidelines

### Zero Dependencies

This crate must remain dependency-free (except `std`) to:
- Minimize compilation time
- Avoid version conflicts
- Ensure it can be used everywhere

### Trait Design

Follow these principles from the architecture docs:
1. Use associated types for performance-critical paths
2. Avoid object-safe traits where static dispatch is preferred
3. Include default implementations where sensible
4. Document invariants clearly

### Type Safety

Leverage Rust's type system:
- Use newtypes for domain concepts (e.g., `MonotonicInstant`)
- Phantom types for compile-time guarantees
- Builder patterns for complex configurations

## Testing

All types should have:
- Property-based tests for invariants
- Serialization round-trip tests
- Examples demonstrating usage

## Future Considerations

- May need to add `#[non_exhaustive]` to enums for backwards compatibility
- Consider adding a prelude module for common imports
- May need feature flags for optional trait implementations