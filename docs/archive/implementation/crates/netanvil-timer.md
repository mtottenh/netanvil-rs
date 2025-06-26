# netanvil-timer Implementation Guide

## Overview

The `netanvil-timer` crate provides high-precision timing primitives essential for microsecond-accurate request scheduling. This crate implements platform-specific optimizations to achieve <1μs timing jitter.

## Related Design Documents

- [Request Scheduler Design](../../section-2-3-request-scheduler.md) - Precision timing requirements
- [Architecture Overview](../../section-1-architecture.md) - Performance principles

## Key Components

### Core Timer Trait

The foundation for all timer implementations:

```rust
pub trait PrecisionTimer: Send + Sync {
    /// Get current monotonic time with nanosecond precision
    fn now(&self) -> MonotonicInstant;
    
    /// Sleep until the specified deadline
    /// May wake up slightly early for spin-wait completion
    async fn sleep_until(&self, deadline: MonotonicInstant);
    
    /// Busy-wait until deadline for maximum precision
    /// Should only be used for short durations (<100μs)
    fn spin_wait_until(&self, deadline: MonotonicInstant);
    
    /// Get the timer's characteristics
    fn characteristics(&self) -> TimerCharacteristics;
}

#[derive(Debug, Clone)]
pub struct TimerCharacteristics {
    /// Minimum reliable sleep duration
    pub min_sleep_duration: Duration,
    
    /// Expected jitter for sleep operations
    pub sleep_jitter: Duration,
    
    /// Cost of calling now() in nanoseconds
    pub now_overhead_ns: u64,
    
    /// Whether TSC is available for ultra-fast time
    pub has_tsc: bool,
}
```

### Platform-Specific Implementations

#### Linux Timer

Leverages Linux-specific features for optimal performance:

```rust
pub struct LinuxTimer {
    /// Clock source for monotonic time
    clock_id: libc::clockid_t,
    
    /// TSC calibration data if available
    tsc_calibration: Option<TscCalibration>,
    
    /// Cached characteristics
    characteristics: TimerCharacteristics,
}

impl LinuxTimer {
    pub fn new() -> Result<Self> {
        // Use CLOCK_MONOTONIC_RAW to avoid NTP adjustments
        let clock_id = libc::CLOCK_MONOTONIC_RAW;
        
        // Calibrate TSC if available
        let tsc_calibration = TscCalibration::try_new()?;
        
        // Measure timer characteristics
        let characteristics = Self::measure_characteristics(clock_id);
        
        Ok(Self {
            clock_id,
            tsc_calibration,
            characteristics,
        })
    }
    
    /// Ultra-fast time using TSC when available
    #[inline(always)]
    fn now_fast(&self) -> MonotonicInstant {
        if let Some(tsc) = &self.tsc_calibration {
            // Read TSC and convert to nanoseconds
            unsafe {
                let tsc_value = core::arch::x86_64::_rdtsc();
                MonotonicInstant::from_nanos(
                    tsc.tsc_to_nanos(tsc_value)
                )
            }
        } else {
            self.now_syscall()
        }
    }
}

/// TSC calibration for ultra-low overhead timing
struct TscCalibration {
    tsc_freq_hz: u64,
    reference_tsc: u64,
    reference_time: MonotonicInstant,
}
```

#### Adaptive Timer

Combines sleep and spin-wait for optimal precision:

```rust
pub struct AdaptiveTimer<T: PrecisionTimer> {
    /// Underlying timer implementation
    inner: T,
    
    /// Threshold for switching to spin-wait
    spin_threshold: Duration,
    
    /// Early wake adjustment
    wake_early: Duration,
}

impl<T: PrecisionTimer> AdaptiveTimer<T> {
    pub fn new(inner: T) -> Self {
        let characteristics = inner.characteristics();
        
        Self {
            inner,
            // Start spinning when we're within 10μs
            spin_threshold: Duration::from_micros(10),
            // Wake up 50μs early to account for scheduler
            wake_early: Duration::from_micros(50),
        }
    }
}

impl<T: PrecisionTimer> PrecisionTimer for AdaptiveTimer<T> {
    async fn sleep_until(&self, deadline: MonotonicInstant) {
        let now = self.inner.now();
        let duration = deadline.duration_since(now);
        
        if duration <= self.spin_threshold {
            // Short duration: just spin
            self.inner.spin_wait_until(deadline);
        } else {
            // Long duration: sleep then spin
            let sleep_until = deadline - self.wake_early;
            self.inner.sleep_until(sleep_until).await;
            
            // Final spin for precision
            self.inner.spin_wait_until(deadline);
        }
    }
}
```

### Glommio Integration

Specialized timer leveraging io_uring:

```rust
pub struct GlommioTimer {
    /// Shared ring for timer operations
    ring: Rc<RefCell<IoUring>>,
    
    /// Pre-allocated timer entries
    timer_pool: Vec<TimerEntry>,
    
    /// Platform timer for fallback
    platform_timer: LinuxTimer,
}

impl GlommioTimer {
    /// Submit timer operation to io_uring
    pub async fn sleep_until_uring(&self, deadline: MonotonicInstant) {
        let sqe = opcode::Timeout::new(deadline.as_timespec())
            .build()
            .user_data(TIMER_USER_DATA);
        
        // Submit and await completion
        let _cqe = self.ring.borrow_mut()
            .submission()
            .push(&sqe)
            .submit_and_wait(1)
            .await
            .expect("io_uring timer");
    }
}
```

## Performance Optimizations

### CPU Core Affinity

Timer accuracy improves with dedicated CPU cores:

```rust
pub fn configure_timer_thread() -> Result<()> {
    // Pin to specific CPU
    let cpu_set = CpuSet::single(0);
    sched_setaffinity(0, &cpu_set)?;
    
    // Set real-time priority
    let param = sched_param { sched_priority: 99 };
    sched_setscheduler(0, SCHED_FIFO, &param)?;
    
    // Disable CPU frequency scaling
    set_cpu_governor("performance")?;
    
    Ok(())
}
```

### Spin-Wait Implementation

Optimized busy-waiting that's CPU-friendly:

```rust
#[inline(always)]
fn spin_wait_until(&self, deadline: MonotonicInstant) {
    // Initial check
    let mut now = self.now();
    if now >= deadline {
        return;
    }
    
    // Exponential backoff for CPU efficiency
    let mut spins = 1;
    
    loop {
        // Pause instruction to reduce CPU power
        for _ in 0..spins {
            core::hint::spin_loop();
        }
        
        now = self.now();
        if now >= deadline {
            break;
        }
        
        // Exponential backoff up to a limit
        spins = (spins * 2).min(32);
    }
}
```

## Testing Strategy

### Precision Tests

Verify timing accuracy across different scenarios:

```rust
#[test]
fn test_timer_precision() {
    let timer = AdaptiveTimer::new(LinuxTimer::new().unwrap());
    let mut jitters = Vec::new();
    
    // Measure jitter for various sleep durations
    for duration_us in [10, 100, 1000, 10000] {
        for _ in 0..1000 {
            let start = timer.now();
            let deadline = start + Duration::from_micros(duration_us);
            
            block_on(timer.sleep_until(deadline));
            
            let actual = timer.now();
            let jitter = actual.duration_since(deadline);
            jitters.push(jitter);
        }
        
        // Verify 99th percentile < 1μs
        jitters.sort();
        let p99 = jitters[jitters.len() * 99 / 100];
        assert!(p99 < Duration::from_micros(1));
    }
}
```

### Platform Compatibility

Test across different configurations:
- With/without TSC
- Different CPU frequencies
- Under system load
- With/without real-time privileges

## Usage Guidelines

### Choosing a Timer

1. **LinuxTimer**: When you need platform-specific optimizations
2. **AdaptiveTimer**: Best general-purpose choice for scheduling
3. **GlommioTimer**: When already using Glommio runtime

### Best Practices

- Calibrate timers at startup
- Use dedicated CPU cores for timer threads
- Avoid timers in tight loops (cache `now()` calls)
- Test on target hardware for accuracy

## Future Enhancements

- Windows high-precision timer support
- macOS mach_absolute_time integration
- Hardware timer interrupt integration
- Dynamic calibration based on system load