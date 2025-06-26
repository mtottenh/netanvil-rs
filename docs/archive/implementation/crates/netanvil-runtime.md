# netanvil-runtime Implementation Guide

## Overview

The `netanvil-runtime` crate provides a Glommio-based runtime optimized for thread-per-core architecture. It manages CPU core assignment, executor configuration, and provides the foundation for high-performance I/O operations.

## Related Design Documents

- [Architecture Overview](../../section-1-architecture.md) - Thread-per-core principles
- [Request Scheduler Design](../../section-2-3-request-scheduler.md) - Core affinity requirements

## Key Components

### Core Runtime Structure

```rust
pub struct NetAnvilRuntime {
    /// Executors mapped to CPU cores
    executors: HashMap<CoreId, ExecutorHandle>,
    
    /// Core assignment strategy
    core_layout: CoreLayout,
    
    /// Shared channels for cross-core communication
    channels: RuntimeChannels,
    
    /// Runtime metrics
    metrics: RuntimeMetrics,
}

#[derive(Debug, Clone)]
pub struct CoreLayout {
    /// Total available cores
    total_cores: usize,
    
    /// Core assignments by role
    assignments: CoreAssignments,
}

#[derive(Debug, Clone)]
pub enum CoreAssignments {
    /// Fixed cores for each role
    Dedicated {
        timer_cores: Vec<CoreId>,
        scheduler_cores: Vec<CoreId>,
        executor_cores: Vec<CoreId>,
        metrics_core: CoreId,
        gossip_cores: Vec<CoreId>,
    },
    /// Dynamic assignment based on load
    Dynamic {
        reserved_cores: HashSet<CoreId>,
        pool_cores: Vec<CoreId>,
    },
}
```

### Executor Management

Creating and managing Glommio executors:

```rust
impl NetAnvilRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self> {
        let core_layout = Self::detect_core_layout(&config)?;
        let mut executors = HashMap::new();
        
        // Create executors for each assigned core
        for (role, cores) in core_layout.assignments.iter() {
            for &core_id in cores {
                let executor = Self::create_executor(core_id, role)?;
                executors.insert(core_id, executor);
            }
        }
        
        Ok(Self {
            executors,
            core_layout,
            channels: RuntimeChannels::new(),
            metrics: RuntimeMetrics::new(),
        })
    }
    
    fn create_executor(core_id: CoreId, role: CoreRole) -> Result<ExecutorHandle> {
        // Configure based on role
        let config = match role {
            CoreRole::Timer => ExecutorConfig {
                ring_depth: 4096,      // Large ring for timer operations
                sqpoll: true,          // Kernel polling for low latency
                thread_pool_size: 0,   // No thread pool needed
            },
            CoreRole::Executor => ExecutorConfig {
                ring_depth: 8192,      // Large ring for I/O operations
                sqpoll: false,         // Regular polling
                thread_pool_size: 4,   // Small thread pool for blocking ops
            },
            CoreRole::Gossip => ExecutorConfig {
                ring_depth: 2048,      // Moderate ring size
                sqpoll: false,
                thread_pool_size: 2,
            },
            _ => ExecutorConfig::default(),
        };
        
        let executor = LocalExecutorBuilder::new()
            .pin_to_cpu(core_id.0)
            .ring_depth(config.ring_depth)
            .sqpoll(config.sqpoll)
            .build()?;
        
        Ok(ExecutorHandle::new(executor))
    }
}
```

### Task Spawning

Spawning tasks on specific cores:

```rust
impl NetAnvilRuntime {
    /// Spawn a task on a specific core
    pub fn spawn_on_core<F, T>(&self, core: CoreId, task: F) -> TaskHandle<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let executor = self.executors.get(&core)
            .expect("Invalid core ID");
        
        executor.spawn_local(task)
    }
    
    /// Spawn on the most appropriate core for the role
    pub fn spawn_for_role<F, T>(&self, role: CoreRole, task: F) -> TaskHandle<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let core = self.select_core_for_role(role);
        self.spawn_on_core(core, task)
    }
    
    /// Load-balanced spawn across executor cores
    pub fn spawn_balanced<F, T>(&self, task: F) -> TaskHandle<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let core = self.select_least_loaded_executor();
        self.spawn_on_core(core, task)
    }
}
```

### Cross-Core Communication

Efficient channels for shared-nothing architecture:

```rust
pub struct RuntimeChannels {
    /// SPSC channels between specific cores
    direct_channels: HashMap<(CoreId, CoreId), DirectChannel>,
    
    /// MPMC broadcast channels
    broadcast_channels: HashMap<ChannelId, BroadcastChannel>,
}

/// Zero-copy channel using shared memory
pub struct DirectChannel {
    ring_buffer: Arc<RingBuffer>,
    sender_core: CoreId,
    receiver_core: CoreId,
}

impl DirectChannel {
    pub async fn send<T: Send>(&self, value: T) -> Result<()> {
        // Serialize to shared memory
        let bytes = bincode::serialize(&value)?;
        
        // Wait for space in ring buffer
        while !self.ring_buffer.has_space(bytes.len()) {
            glommio::yield_now().await;
        }
        
        // Write to ring buffer
        self.ring_buffer.write(&bytes);
        
        Ok(())
    }
}
```

### Performance Monitoring

Runtime metrics collection:

```rust
pub struct RuntimeMetrics {
    /// Per-core metrics
    core_metrics: HashMap<CoreId, CoreMetrics>,
    
    /// Global runtime stats
    global_stats: GlobalStats,
}

#[derive(Default)]
pub struct CoreMetrics {
    /// Tasks spawned
    pub tasks_spawned: AtomicU64,
    
    /// Tasks completed
    pub tasks_completed: AtomicU64,
    
    /// Current queue depth
    pub queue_depth: AtomicU64,
    
    /// IO operations
    pub io_ops: AtomicU64,
    
    /// CPU utilization (0-100)
    pub cpu_utilization: AtomicU8,
}

impl NetAnvilRuntime {
    /// Start background metrics collection
    pub fn start_metrics_collection(&self) {
        for (&core_id, executor) in &self.executors {
            let metrics = self.metrics.core_metrics.get(&core_id).unwrap();
            
            executor.spawn_local(async move {
                let mut interval = glommio::timer::interval(Duration::from_secs(1));
                
                loop {
                    interval.await;
                    
                    // Update metrics
                    let stats = glommio::executor::stats();
                    metrics.queue_depth.store(stats.queue_depth, Ordering::Relaxed);
                    metrics.io_ops.store(stats.io_ops, Ordering::Relaxed);
                    
                    // Calculate CPU utilization
                    let cpu_util = calculate_cpu_utilization();
                    metrics.cpu_utilization.store(cpu_util, Ordering::Relaxed);
                }
            });
        }
    }
}
```

## Core Assignment Strategies

### Dedicated Cores (Recommended)

For a 16-core system:

```rust
pub fn create_dedicated_layout() -> CoreLayout {
    CoreLayout {
        total_cores: 16,
        assignments: CoreAssignments::Dedicated {
            timer_cores: vec![CoreId(0)],          // 1 core for precision timing
            scheduler_cores: vec![CoreId(1), CoreId(2)], // 2 cores for scheduling
            executor_cores: (3..12).map(CoreId).collect(), // 9 cores for execution
            metrics_core: CoreId(12),              // 1 core for metrics
            gossip_cores: vec![CoreId(13), CoreId(14), CoreId(15)], // 3 for distributed
        },
    }
}
```

### Dynamic Assignment

For variable workloads:

```rust
pub fn create_dynamic_layout() -> CoreLayout {
    CoreLayout {
        total_cores: 16,
        assignments: CoreAssignments::Dynamic {
            reserved_cores: hashset![CoreId(0), CoreId(1)], // Timer and metrics
            pool_cores: (2..16).map(CoreId).collect(),      // Flexible pool
        },
    }
}
```

## Integration Example

Complete runtime setup:

```rust
pub async fn setup_runtime() -> Result<NetAnvilRuntime> {
    // Configure runtime
    let config = RuntimeConfig {
        core_layout: CoreLayoutStrategy::Dedicated,
        enable_cpu_affinity: true,
        enable_numa_awareness: true,
    };
    
    // Create runtime
    let runtime = NetAnvilRuntime::new(config)?;
    
    // Start core services
    runtime.start_metrics_collection();
    
    // Spawn timer on dedicated core
    runtime.spawn_for_role(CoreRole::Timer, async {
        let timer = GlommioTimer::new();
        timer.run_forever().await
    });
    
    // Spawn schedulers
    for core in runtime.core_layout.scheduler_cores() {
        runtime.spawn_on_core(core, async {
            let scheduler = PrecisionScheduler::new();
            scheduler.run().await
        });
    }
    
    Ok(runtime)
}
```

## Testing

### Core Affinity Verification

```rust
#[test]
fn test_core_affinity() {
    let runtime = NetAnvilRuntime::new(test_config()).unwrap();
    
    // Verify each executor is pinned correctly
    for (&core_id, executor) in &runtime.executors {
        let actual_core = executor.get_cpu_affinity();
        assert_eq!(actual_core, core_id.0);
    }
}
```

### Performance Benchmarks

```rust
#[bench]
fn bench_cross_core_communication() {
    let runtime = NetAnvilRuntime::new(bench_config()).unwrap();
    
    // Measure latency between cores
    let latencies = measure_channel_latencies(&runtime);
    
    // Verify < 1μs for direct channels
    assert!(latencies.p99() < Duration::from_micros(1));
}
```

## Best Practices

1. **Core Isolation**: Use `isolcpus` kernel parameter for dedicated cores
2. **NUMA Awareness**: Place related executors on same NUMA node
3. **Avoid Oversubscription**: Don't spawn more executors than physical cores
4. **Monitor Metrics**: Watch for queue depth and CPU saturation

## Future Enhancements

- NUMA-aware memory allocation
- Dynamic core rebalancing
- Integration with Linux cgroups
- Hardware accelerator support (DPDK, SPDK)