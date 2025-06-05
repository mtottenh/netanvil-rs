# Rate Control System with Client Session Integration and Distributed Coordination

## 1. Introduction

The Rate Control system is a core component of the load testing framework, determining the overall workload level during test execution. This design combines comprehensive metrics integration with sophisticated distributed load control capabilities, enabling precise monitoring, feedback-based control, and multi-node test coordination.

Key features include:

- **Unified Metrics Integration**: Lock-free metrics collection with registry integration
- **Client Session Management**: Translation of RPS targets to realistic client session behaviors
- **Advanced Control Algorithms**: PID controllers, step functions, and external signal-based control
- **Rate Smoothing**: Prevent abrupt changes that could skew results or cause unrealistic traffic patterns
- **Saturation Awareness**: Detect and adapt to client-side bottlenecks using Transaction Profiler data
- **Distributed Load Control**: Coordinate rate across multiple nodes with centralized control
- **Composable Design**: Mix and match controllers, smoothers, and adapters for different scenarios

## 2. Core Interfaces and Types

### 2.1 Generic Rate Controller with Static Dispatch

Following the system architecture's preference for static dispatch, we implement rate controllers using generic types rather than trait objects for performance-critical paths.

```rust
/// Generic rate controller interface with static dispatch
pub trait RateController<M, S, C> 
where 
    M: MetricsProvider,
    S: Signal,
    C: SessionControl,
{
    /// Get the current target RPS
    fn get_current_rps(&self) -> u64;

    /// Update controller state based on metrics from Results Collector
    fn update(&self, metrics: &RequestMetrics);
    
    /// Get session control signals for Client Session Manager
    fn get_session_control_signals(&self) -> C::SessionSignals;

    /// Called at the end of the test to get rate history
    fn get_rate_history(&self) -> Vec<(u64, u64)>;

    /// Get rate adjustments with reasons
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)>;
    
    /// Get metrics provider for monitoring
    fn metrics(&self) -> &M;
    
    /// Get signal source for this controller
    fn signal(&self) -> &S;
}

/// Signal source for controllers - provides metric values to control algorithm
pub trait Signal {
    /// Get the current signal value
    fn get_value(&self) -> f64;
    
    /// Get signal type (for display/metrics)
    fn get_type(&self) -> &'static str;
    
    /// Update signal based on metrics
    fn update(&mut self, metrics: &RequestMetrics);
    
    /// Process any external data if applicable
    fn process_external_data(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error>>;
}

/// Session control interface
pub trait SessionControl {
    /// Session signals type
    type SessionSignals;
    
    /// Create session signals from RPS target
    fn create_signals(&self, target_rps: u64, last_rps: u64) -> Self::SessionSignals;
    
    /// Update session control based on metrics
    fn update(&mut self, metrics: &RequestMetrics);
}

/// Metric values captured during the test execution (from Results Collector)
#[derive(Debug, Clone)]
pub struct RequestMetrics {
    /// Timestamp when metrics were collected (ms since test start)
    pub timestamp: u64,
    /// Size of the time window these metrics represent (ms)
    pub window_size_ms: u64,
    /// Number of requests captured in this window
    pub request_count: u64,
    /// 50th percentile latency in microseconds
    pub p50_latency_us: u64,
    /// 90th percentile latency in microseconds
    pub p90_latency_us: u64,
    /// 99th percentile latency in microseconds
    pub p99_latency_us: u64,
    /// Error rate (0.0 - 1.0)
    pub error_rate: f64,
    /// Current throughput (requests per second)
    pub current_throughput: f64,
}

/// Rate adjustment with reason
#[derive(Debug, Clone)]
pub struct RateAdjustment {
    /// New target RPS
    pub new_rps: u64,
    /// Previous target RPS
    pub old_rps: u64,
    /// Reason for adjustment
    pub reason: AdjustmentReason,
    /// Additional context for the adjustment
    pub context: String,
}

/// Reasons for rate adjustments
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdjustmentReason {
    /// Initial rate setting
    Initial,
    /// PID control adjustment
    PidControl,
    /// Step function change
    StepFunction,
    /// Manual adjustment
    Manual,
    /// External signal influence
    ExternalSignal,
    /// Client saturation detected
    Saturation,
    /// Critical error rate exceeded
    ErrorThreshold,
    /// Rate smoothing applied
    RateSmoothing,
    /// Distributed coordination adjustment
    DistributedControl,
    /// Session behavior adjustment
    SessionBehavior,
    /// Other reason
    Other,
}

/// Default session control signals from rate controller to session manager
#[derive(Debug, Clone)]
pub struct SessionControlSignals {
    /// Target RPS for the entire system
    pub target_rps: u64,
    
    /// Target session creation rate (new sessions per second)
    pub session_creation_rate: Option<f64>,
    
    /// Target active session count
    pub target_active_sessions: Option<u64>,
    
    /// Session type distribution adjustments (if any)
    pub session_type_adjustments: Option<HashMap<ClientType, f64>>,
    
    /// Whether to prioritize request rate or session fidelity
    pub prioritize_request_rate: bool,
    
    /// Rate change notification for session behavior adjustment
    pub rate_change_event: Option<RateChangeEvent>,
}

/// Rate change notification system
#[derive(Debug, Clone)]
pub struct RateChangeEvent {
    /// New target RPS
    pub new_rps: u64,
    
    /// Previous target RPS
    pub old_rps: u64,
    
    /// Reason for the change
    pub reason: AdjustmentReason,
    
    /// Session impact assessment
    pub session_impact: SessionImpact,
}

/// Impact of rate changes on session behavior
#[derive(Debug, Clone)]
pub enum SessionImpact {
    /// Add more sessions to increase load
    AddSessions {
        /// How many sessions to add
        count: u64,
        /// What types of sessions to prioritize
        priority_types: Vec<ClientType>,
    },
    
    /// Remove sessions to decrease load
    RemoveSessions {
        /// How many sessions to remove
        count: u64,
        /// Selection strategy for removal
        strategy: SessionRemovalStrategy,
    },
    
    /// Modify activity in existing sessions
    ModifyActivityLevel {
        /// Activity level multiplier (1.0 = no change)
        multiplier: f64,
    },
    
    /// Gradual change (ramp up/down)
    GradualChange {
        /// Duration of transition in milliseconds
        duration_ms: u64,
        /// Easing function for transition
        easing: EasingFunction,
    },
}

/// Strategy for removing sessions when decreasing load
#[derive(Debug, Clone)]
pub enum SessionRemovalStrategy {
    /// Remove oldest sessions first
    OldestFirst,
    /// Remove newest sessions first
    NewestFirst,
    /// Remove randomly
    Random,
    /// Remove least active sessions first
    LeastActiveFirst,
}

/// Easing function for gradual rate changes
#[derive(Debug, Clone)]
pub enum EasingFunction {
    /// Linear transition
    Linear,
    /// Exponential acceleration
    EaseIn,
    /// Exponential deceleration
    EaseOut,
    /// Smooth sigmoid curve (slow start, fast middle, slow end)
    EaseInOut,
}
```

### 2.2 Signal Implementations

```rust
/// Latency signal providing P99 latency measurements
pub struct LatencyP99Signal {
    /// Current value
    current_value: AtomicF64,
    /// Historical values
    history: Arc<RwLock<VecDeque<(u64, f64)>>>,
    /// Metrics for this signal
    metrics: Arc<SignalMetrics>,
}

impl Signal for LatencyP99Signal {
    fn get_value(&self) -> f64 {
        self.current_value.load(Ordering::Relaxed)
    }
    
    fn get_type(&self) -> &'static str {
        "latency_p99"
    }
    
    fn update(&mut self, metrics: &RequestMetrics) {
        let value = metrics.p99_latency_us as f64;
        self.current_value.store(value, Ordering::Relaxed);
        
        // Update history
        if let Ok(mut history) = self.history.write() {
            history.push_back((metrics.timestamp, value));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
        
        self.metrics.update_value(value);
    }
    
    fn process_external_data(&mut self, _data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        // No external data needed for latency signal
        Ok(())
    }
}

/// Error rate signal 
pub struct ErrorRateSignal {
    /// Current value
    current_value: AtomicF64,
    /// Historical values
    history: Arc<RwLock<VecDeque<(u64, f64)>>>,
    /// Metrics for this signal
    metrics: Arc<SignalMetrics>,
}

impl Signal for ErrorRateSignal {
    fn get_value(&self) -> f64 {
        self.current_value.load(Ordering::Relaxed)
    }
    
    fn get_type(&self) -> &'static str {
        "error_rate"
    }
    
    fn update(&mut self, metrics: &RequestMetrics) {
        let value = metrics.error_rate;
        self.current_value.store(value, Ordering::Relaxed);
        
        // Update history
        if let Ok(mut history) = self.history.write() {
            history.push_back((metrics.timestamp, value));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
        
        self.metrics.update_value(value);
    }
    
    fn process_external_data(&mut self, _data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        // No external data needed for error rate signal
        Ok(())
    }
}

/// Saturation signal that connects to Transaction Profiler
pub struct SaturationSignal<P: TransactionProfiler> {
    /// Current value
    current_value: AtomicF64,
    /// Historical values
    history: Arc<RwLock<VecDeque<(u64, f64)>>>,
    /// Metrics for this signal
    metrics: Arc<SignalMetrics>,
    /// Transaction profiler
    profiler: P,
}

impl<P: TransactionProfiler> Signal for SaturationSignal<P> {
    fn get_value(&self) -> f64 {
        self.current_value.load(Ordering::Relaxed)
    }
    
    fn get_type(&self) -> &'static str {
        "saturation"
    }
    
    fn update(&mut self, _metrics: &RequestMetrics) {
        // Get saturation metrics from profiler
        let saturation = self.profiler.get_saturation_metrics();
        
        // Map saturation level to a numerical value
        let value = match saturation.level {
            SaturationLevel::None => 0.0,
            SaturationLevel::Mild => 0.33,
            SaturationLevel::Moderate => 0.66,
            SaturationLevel::Severe => 1.0,
        };
        
        self.current_value.store(value, Ordering::Relaxed);
        
        // Update history
        if let Ok(mut history) = self.history.write() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
                
            history.push_back((now, value));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
        
        self.metrics.update_value(value);
    }
    
    fn process_external_data(&mut self, _data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        // No external data needed - profiler provides data
        Ok(())
    }
}

/// External signal that can be connected to any data source
pub struct ExternalSignal {
    /// Current value
    current_value: AtomicF64,
    /// Historical values
    history: Arc<RwLock<VecDeque<(u64, f64)>>>,
    /// Metrics for this signal
    metrics: Arc<SignalMetrics>,
    /// Signal name
    name: String,
}

impl Signal for ExternalSignal {
    fn get_value(&self) -> f64 {
        self.current_value.load(Ordering::Relaxed)
    }
    
    fn get_type(&self) -> &'static str {
        "external"
    }
    
    fn update(&mut self, _metrics: &RequestMetrics) {
        // External signals update through process_external_data
    }
    
    fn process_external_data(&mut self, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        // Parse external data as JSON value
        let value: f64 = serde_json::from_slice(data)?;
        
        self.current_value.store(value, Ordering::Relaxed);
        
        // Update history
        if let Ok(mut history) = self.history.write() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
                
            history.push_back((now, value));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
        
        self.metrics.update_value(value);
        
        Ok(())
    }
}
```

### 2.3 Generic PID Controller

```rust
/// Generic PID controller that can work with any signal type
pub struct PidController<M, S, C>
where
    M: MetricsProvider,
    S: Signal,
    C: SessionControl,
{
    /// Controller ID
    id: String,
    
    /// PID parameters
    kp: f64,
    ki: f64,
    kd: f64,
    
    /// Target value (setpoint)
    target: f64,
    
    /// Signal source for measured value
    signal: S,
    
    /// Session control
    session_control: C,
    
    /// Metrics provider
    metrics: M,
    
    /// Minimum RPS
    min_rps: u64,
    
    /// Maximum RPS
    max_rps: u64,
    
    /// Current RPS
    current_rps: AtomicU64,
    
    /// Integral term (accumulated error)
    integral: Mutex<f64>,
    
    /// Last error value for derivative calculation
    last_error: Mutex<f64>,
    
    /// Last update timestamp
    last_update: AtomicU64,
    
    /// Rate adjustment history
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Update interval
    update_interval_ms: u64,
}

impl<M, S, C> PidController<M, S, C>
where
    M: MetricsProvider,
    S: Signal,
    C: SessionControl,
{
    /// Create a new PID controller
    pub fn new(
        id: String,
        target: f64,
        kp: f64,
        ki: f64,
        kd: f64,
        signal: S,
        session_control: C,
        metrics: M,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        update_interval_ms: u64,
    ) -> Self {
        Self {
            id,
            kp,
            ki,
            kd,
            target,
            signal,
            session_control,
            metrics,
            min_rps,
            max_rps,
            current_rps: AtomicU64::new(initial_rps),
            integral: Mutex::new(0.0),
            last_error: Mutex::new(0.0),
            last_update: AtomicU64::new(0),
            adjustments: Mutex::new(vec![]),
            update_interval_ms,
        }
    }
}

impl<M, S, C> RateController<M, S, C> for PidController<M, S, C>
where
    M: MetricsProvider,
    S: Signal,
    C: SessionControl,
{
    fn get_current_rps(&self) -> u64 {
        self.current_rps.load(Ordering::Relaxed)
    }
    
    fn update(&self, metrics: &RequestMetrics) {
        // Only update at appropriate intervals
        let now = metrics.timestamp;
        let last = self.last_update.load(Ordering::Relaxed);
        
        if now - last < self.update_interval_ms {
            return;
        }
        
        // Update signal with latest metrics
        let mut signal = self.signal.clone();
        signal.update(metrics);
        
        // Get current value from signal
        let current_value = signal.get_value();
        
        // Calculate error (for most signals, target - current makes sense)
        // For some signals like error rate, we might need to invert this
        let error = if signal.get_type() == "error_rate" {
            // For error rate, lower is better, so invert
            current_value - self.target
        } else {
            // For latency and others, target - current
            self.target - current_value
        };
        
        // Update integral term
        let mut integral = self.integral.lock().unwrap();
        *integral += error * ((now - last) as f64 / 1000.0); // Scale based on time elapsed
        
        // Apply anti-windup by clamping integral term
        let max_integral = (self.max_rps - self.min_rps) as f64 / self.ki;
        *integral = integral.clamp(-max_integral, max_integral);
        
        // Calculate derivative term
        let derivative = {
            let mut last_error = self.last_error.lock().unwrap();
            let dt = (now - last) as f64 / 1000.0;
            let derivative = if dt > 0.0 {
                (error - *last_error) / dt
            } else {
                0.0
            };
            *last_error = error;
            derivative
        };
        
        // Calculate PID output
        let p_term = self.kp * error;
        let i_term = self.ki * *integral;
        let d_term = self.kd * derivative;
        let output = p_term + i_term + d_term;
        
        // Get current RPS
        let current_rps = self.current_rps.load(Ordering::Relaxed);
        
        // Calculate new RPS
        let new_rps = (current_rps as f64 + output).round() as u64;
        let new_rps = new_rps.clamp(self.min_rps, self.max_rps);
        
        // Skip update if minimal change
        if (new_rps as i64 - current_rps as i64).abs() < 2 {
            return;
        }
        
        // Create context string
        let context = format!(
            "Target: {:.2}, Current: {:.2}, Error: {:.2}, P: {:.2}, I: {:.2}, D: {:.2}",
            self.target, current_value, error, p_term, i_term, d_term
        );
        
        // Store new RPS
        self.current_rps.store(new_rps, Ordering::Relaxed);
        
        // Update timestamp
        self.last_update.store(now, Ordering::Relaxed);
        
        // Record adjustment
        let adjustment = RateAdjustment {
            new_rps,
            old_rps: current_rps,
            reason: AdjustmentReason::PidControl,
            context,
        };
        
        let mut adjustments = self.adjustments.lock().unwrap();
        adjustments.push((now, adjustment));
        
        // Update session control
        let mut session_control = self.session_control.clone();
        session_control.update(metrics);
    }
    
    fn get_session_control_signals(&self) -> C::SessionSignals {
        let current_rps = self.current_rps.load(Ordering::Relaxed);
        let last_rps = if let Some(adj) = self.adjustments.lock().unwrap().last() {
            adj.1.old_rps
        } else {
            current_rps
        };
        
        self.session_control.create_signals(current_rps, last_rps)
    }
    
    fn get_rate_history(&self) -> Vec<(u64, u64)> {
        let adjustments = self.adjustments.lock().unwrap();
        adjustments.iter()
            .map(|(time, adj)| (*time, adj.new_rps))
            .collect()
    }
    
    fn get_rate_adjustments(&self) -> Vec<(u64, RateAdjustment)> {
        let adjustments = self.adjustments.lock().unwrap();
        adjustments.clone()
    }
    
    fn metrics(&self) -> &M {
        &self.metrics
    }
    
    fn signal(&self) -> &S {
        &self.signal
    }
}
```

### 2.4 Control Configuration

```rust
/// Rate control configuration used by factory
#[derive(Debug, Clone)]
pub enum RateControlMode {
    /// Keep constant rate
    Static,
    /// Generic PID controller 
    Pid {
        /// Target metric value (e.g. 200ms for latency)
        target_metric: f64,
        /// Signal type for input
        signal_type: SignalType,
        /// Proportional gain
        kp: f64,
        /// Integral gain
        ki: f64,
        /// Derivative gain
        kd: f64,
        /// Update interval in milliseconds
        update_interval_ms: u64,
    },
    /// Step function (time in seconds -> RPS)
    StepFunction(Vec<(u64, u64)>),
    /// Custom controller (clients can implement their own)
    Custom,
}

/// Types of signals that can be used with PID controller
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    /// 50th percentile latency
    LatencyP50,
    /// 90th percentile latency
    LatencyP90,
    /// 95th percentile latency
    LatencyP95,
    /// 99th percentile latency
    LatencyP99,
    /// Error rate (0.0-1.0)
    ErrorRate,
    /// Session count
    ActiveSessions,
    /// Saturation level
    Saturation,
    /// Custom external signal
    External(String),
}

/// Session control configuration
#[derive(Debug, Clone)]
pub struct SessionControlConfig {
    /// Whether to prioritize request rate over session fidelity
    pub prioritize_request_rate: bool,
    /// How to ramp up/down load
    pub ramp_mode: RampMode,
    /// Whether to apply constant think time across load levels
    pub constant_think_time: bool,
    /// Maximum session count
    pub max_sessions: Option<u64>,
}

/// Load ramp behavior
#[derive(Debug, Clone)]
pub enum RampMode {
    /// Add/remove sessions to change load
    AdjustSessionCount,
    /// Modify think time within sessions
    AdjustThinkTime,
    /// Change request pattern within sessions
    AdjustRequestPattern,
    /// Combined approach (automatic)
    Automatic,
}
```

## 3. Integration with Client Session Manager

The Rate Controller's primary responsibility is determining the overall load level, but it must translate this into directives that the Client Session Manager can interpret and implement. This section details how the Rate Controller interfaces with the Client Session Manager.

### 3.1 Session Control Signal Flow

```
┌────────────────┐        ┌───────────────────┐        ┌──────────────────┐
│ Results        │        │ Rate              │        │ Client Session    │
│ Collector      │───────>│ Controller        │───────>│ Manager          │
└────────────────┘metrics └───────────────────┘signals └──────────────────┘
                                   ^                            │
                                   │                            │
                                   └────────────────────────────┘
                                      session metrics feedback
```

The Rate Controller sends `SessionControlSignals` to the Client Session Manager which contains:
- Base target RPS for the entire test
- Session-specific targets (creation rate, active count)
- Distribution adjustments for different client types
- Rate change notifications with behavioral hints

```rust
impl RateController {
    /// Get session control signals for Client Session Manager
    fn get_session_control_signals(&self) -> SessionControlSignals {
        let current_rps = self.get_current_rps();
        
        // Decide how to translate RPS to session behavior
        let session_creation_rate = self.calculate_session_creation_rate(current_rps);
        let active_sessions = self.calculate_target_active_sessions(current_rps);
        
        // Check if distribution needs adjustment
        let distribution_adjustments = if self.needs_distribution_adjustment() {
            Some(self.calculate_distribution_adjustments())
        } else {
            None
        };
        
        // Determine if this is a significant rate change that needs special handling
        let rate_change_event = if self.is_significant_rate_change() {
            Some(self.create_rate_change_event())
        } else {
            None
        };
        
        SessionControlSignals {
            target_rps: current_rps,
            session_creation_rate: Some(session_creation_rate),
            target_active_sessions: Some(active_sessions),
            session_type_adjustments: distribution_adjustments,
            prioritize_request_rate: self.should_prioritize_rate(),
            rate_change_event: rate_change_event,
        }
    }
    
    // Helper methods for calculating session parameters...
}
```

### 3.2 Session-Based Rate Control

For certain tests, it's more realistic to control the rate by manipulating virtual user (session) behavior rather than directly controlling RPS. The `SessionBased` rate control mode enables this approach:

```rust
/// Session-based rate controller implementation
pub struct SessionBasedRateController {
    /// Controller ID
    id: String,
    
    /// Base configuration
    config: Arc<LoadTestConfig>,
    
    /// Session parameters
    session_params: SessionParameters,
    
    /// Current session metrics
    current_metrics: Arc<RwLock<SessionMetrics>>,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl SessionBasedRateController {
    /// Create a new session-based rate controller
    pub fn new(
        id: String,
        initial_rps: u64,
        min_rps: u64,
        max_rps: u64,
        session_params: SessionParameters,
    ) -> Self {
        // Implementation...
    }
    
    /// Calculate RPS from session metrics
    fn calculate_rps_from_sessions(&self, metrics: &SessionMetrics) -> u64 {
        // Implementation...
    }
    
    /// Adjust session parameters based on target RPS
    fn adjust_session_params(&self, target_rps: u64) -> SessionParameters {
        // Implementation...
    }
}

impl RateController for SessionBasedRateController {
    // Implementation...
    
    fn get_session_control_signals(&self) -> SessionControlSignals {
        // Translate controller state to session signals
        // Implementation...
    }
}
```

### 3.3 Rate Change Propagation

When the Rate Controller makes significant adjustments to the target RPS, it needs to communicate not just the new target, but also how the Client Session Manager should implement the change. The Rate Controller creates `RateChangeEvent` objects that include contextual information about the change:

```rust
impl PidRateController {
    /// Create a rate change event when significant changes occur
    fn create_rate_change_event(&self) -> RateChangeEvent {
        let current_rps = self.metrics.current_rps.load(Ordering::Relaxed);
        let new_rps = self.calculate_new_target_rps();
        
        // Calculate relative change
        let relative_change = (new_rps as f64 - current_rps as f64) / current_rps as f64;
        
        // Determine appropriate session impact based on magnitude and direction of change
        let session_impact = if relative_change > 0.5 {
            // Large increase - add sessions quickly
            SessionImpact::AddSessions {
                count: self.calculate_sessions_to_add(new_rps, current_rps),
                priority_types: self.determine_priority_client_types(),
            }
        } else if relative_change > 0.1 {
            // Moderate increase - gradual ramp-up
            SessionImpact::GradualChange {
                duration_ms: 10000, // 10 seconds
                easing: EasingFunction::EaseIn,
            }
        } else if relative_change < -0.5 {
            // Large decrease - remove sessions
            SessionImpact::RemoveSessions {
                count: self.calculate_sessions_to_remove(current_rps, new_rps),
                strategy: SessionRemovalStrategy::LeastActiveFirst,
            }
        } else if relative_change < -0.1 {
            // Moderate decrease - gradual ramp-down
            SessionImpact::GradualChange {
                duration_ms: 5000, // 5 seconds
                easing: EasingFunction::EaseOut,
            }
        } else {
            // Small change - adjust activity levels
            SessionImpact::ModifyActivityLevel {
                multiplier: new_rps as f64 / current_rps as f64,
            }
        };
        
        RateChangeEvent {
            new_rps,
            old_rps: current_rps,
            reason: self.determine_adjustment_reason(),
            session_impact,
        }
    }
}
```

## 4. Rate Controller Metrics

### 4.1 Lock-Free Metrics Implementation

```rust
/// Lock-free metrics collection for rate controllers
pub struct RateControllerMetrics {
    /// Component ID
    id: String,
    
    /// Current target RPS
    current_rps: AtomicU64,
    
    /// Minimum RPS
    min_rps: AtomicU64,
    
    /// Maximum RPS
    max_rps: AtomicU64,
    
    /// Number of rate adjustments
    adjustment_count: AtomicU64,
    
    /// Last adjustment timestamp
    last_adjustment: AtomicU64,
    
    /// PID controller metrics (if applicable)
    error_value: AtomicF64,
    integral_term: AtomicF64,
    derivative_term: AtomicF64,
    
    /// Bounded history of rate changes
    /// Protected with RwLock but updated infrequently
    rate_history: Arc<RwLock<VecDeque<(u64, u64, String)>>>,
    
    /// Smoothing metrics (if applicable)
    smoothing_limits_applied: AtomicU64,
    max_smoothing_change: AtomicU64,
    
    /// Distributed control metrics (if applicable)
    distributed_proportion_factor: AtomicF64,
    rebalance_count: AtomicU64,
    
    /// Session control metrics
    session_creation_rate: AtomicF64,
    target_active_sessions: AtomicU64,
    rate_change_events: AtomicU64,
}

impl RateControllerMetrics {
    /// Create new rate controller metrics
    pub fn new(id: String, initial_rps: u64, min_rps: u64, max_rps: u64) -> Self {
        Self {
            id,
            current_rps: AtomicU64::new(initial_rps),
            min_rps: AtomicU64::new(min_rps),
            max_rps: AtomicU64::new(max_rps),
            adjustment_count: AtomicU64::new(0),
            last_adjustment: AtomicU64::new(current_timestamp()),
            error_value: AtomicF64::new(0.0),
            integral_term: AtomicF64::new(0.0),
            derivative_term: AtomicF64::new(0.0),
            rate_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
            smoothing_limits_applied: AtomicU64::new(0),
            max_smoothing_change: AtomicU64::new(0),
            distributed_proportion_factor: AtomicF64::new(1.0),
            rebalance_count: AtomicU64::new(0),
            session_creation_rate: AtomicF64::new(0.0),
            target_active_sessions: AtomicU64::new(0),
            rate_change_events: AtomicU64::new(0),
        }
    }
    
    /// Update the current RPS
    pub fn update_rps(&self, new_rps: u64, reason: &str) {
        // Store previous value for history
        let old_rps = self.current_rps.load(Ordering::Relaxed);
        
        // Update current RPS
        self.current_rps.store(new_rps, Ordering::Relaxed);
        
        // Increment adjustment count
        self.adjustment_count.fetch_add(1, Ordering::Relaxed);
        
        // Update timestamp
        let now = current_timestamp();
        self.last_adjustment.store(now, Ordering::Relaxed);
        
        // Record in history (with bounded size)
        let reason_str = reason.to_string();
        if let Ok(mut history) = self.rate_history.write() {
            history.push_back((now, new_rps, reason_str));
            
            // Keep bounded size
            while history.len() > 100 {
                history.pop_front();
            }
        }
    }
    
    /// Update session control metrics
    pub fn update_session_metrics(&self, signals: &SessionControlSignals) {
        if let Some(rate) = signals.session_creation_rate {
            self.session_creation_rate.store(rate as f64, Ordering::Relaxed);
        }
        
        if let Some(count) = signals.target_active_sessions {
            self.target_active_sessions.store(count, Ordering::Relaxed);
        }
        
        if signals.rate_change_event.is_some() {
            self.rate_change_events.fetch_add(1, Ordering::Relaxed);
        }
    }
    
    /// Update PID controller metrics
    pub fn update_pid_metrics(&self, error: f64, integral: f64, derivative: f64) {
        self.error_value.store(error, Ordering::Relaxed);
        self.integral_term.store(integral, Ordering::Relaxed);
        self.derivative_term.store(derivative, Ordering::Relaxed);
    }
    
    /// Record smoothing metrics
    pub fn record_smoothing(&self, requested_rps: u64, actual_rps: u64) {
        if requested_rps != actual_rps {
            self.smoothing_limits_applied.fetch_add(1, Ordering::Relaxed);
            
            let change = (requested_rps as i64 - actual_rps as i64).abs() as u64;
            let current_max = self.max_smoothing_change.load(Ordering::Relaxed);
            
            // Update max change if larger
            if change > current_max {
                let _ = self.max_smoothing_change.compare_exchange(
                    current_max, change, Ordering::Relaxed, Ordering::Relaxed
                );
            }
        }
    }
    
    /// Update distributed control metrics
    pub fn update_distributed_metrics(&self, proportion_factor: f64) {
        self.distributed_proportion_factor.store(proportion_factor, Ordering::Relaxed);
        self.rebalance_count.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();
        
        // Add atomic counter values
        metrics.insert("current_rps".to_string(), 
                      MetricValue::Gauge(self.current_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("min_rps".to_string(),
                      MetricValue::Gauge(self.min_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("max_rps".to_string(),
                      MetricValue::Gauge(self.max_rps.load(Ordering::Relaxed) as i64));
                      
        metrics.insert("adjustment_count".to_string(),
                      MetricValue::Counter(self.adjustment_count.load(Ordering::Relaxed)));
                      
        metrics.insert("last_adjustment".to_string(),
                      MetricValue::Counter(self.last_adjustment.load(Ordering::Relaxed)));
        
        // Add PID controller metrics if applicable
        metrics.insert("error_value".to_string(),
                      MetricValue::Float(self.error_value.load(Ordering::Relaxed)));
                      
        metrics.insert("integral_term".to_string(),
                      MetricValue::Float(self.integral_term.load(Ordering::Relaxed)));
                      
        metrics.insert("derivative_term".to_string(),
                      MetricValue::Float(self.derivative_term.load(Ordering::Relaxed)));
        
        // Add smoothing metrics if applicable
        metrics.insert("smoothing_limits_applied".to_string(),
                      MetricValue::Counter(self.smoothing_limits_applied.load(Ordering::Relaxed)));
                      
        metrics.insert("max_smoothing_change".to_string(),
                      MetricValue::Counter(self.max_smoothing_change.load(Ordering::Relaxed)));
        
        // Add distributed control metrics if applicable
        metrics.insert("distributed_proportion_factor".to_string(),
                      MetricValue::Float(self.distributed_proportion_factor.load(Ordering::Relaxed)));
                      
        metrics.insert("rebalance_count".to_string(),
                      MetricValue::Counter(self.rebalance_count.load(Ordering::Relaxed)));
        
        // Add session control metrics
        metrics.insert("session_creation_rate".to_string(),
                      MetricValue::Float(self.session_creation_rate.load(Ordering::Relaxed)));
                      
        metrics.insert("target_active_sessions".to_string(),
                      MetricValue::Counter(self.target_active_sessions.load(Ordering::Relaxed)));
                      
        metrics.insert("rate_change_events".to_string(),
                      MetricValue::Counter(self.rate_change_events.load(Ordering::Relaxed)));
        
        // Add recent history as a text metric
        if let Ok(history) = self.rate_history.read() {
            // Convert the last 5 adjustments to a compact string
            let recent_history = history.iter()
                .rev()
                .take(5)
                .map(|(ts, rps, reason)| format!("{}:{}:{}", ts, rps, reason))
                .collect::<Vec<_>>()
                .join("; ");
                
            metrics.insert("recent_adjustments".to_string(),
                          MetricValue::Text(recent_history));
        }
        
        metrics
    }
}

/// Helper function to get current timestamp
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
```

## 5. Control Signal System with Metrics

### 5.1 Control Signal Interface

```rust
/// External control signal interface
pub trait ControlSignal: MetricsProvider + Send + Sync {
    /// Get the current signal value
    fn get_current_value(&self) -> f64;

    /// Register a callback for signal updates
    fn register_update_callback(&mut self, callback: Box<dyn Fn(f64) + Send + Sync>);

    /// Get historical values with timestamps (for reporting)
    fn get_history(&self) -> Vec<(u64, f64)>;
}

/// Set of control signals available to controllers
#[derive(Clone)]
pub struct ControlSignalSet {
    /// Named signals
    signals: Arc<DashMap<String, Arc<dyn ControlSignal>>>,
}

impl ControlSignalSet {
    /// Create a new control signal set
    pub fn new() -> Self {
        Self {
            signals: Arc::new(DashMap::new()),
        }
    }

    /// Register a new control signal
    pub fn register_signal(&self, name: &str, signal: Arc<dyn ControlSignal>) {
        self.signals.insert(name.to_string(), signal);
    }

    /// Get a control signal by name
    pub fn get_signal(&self, name: &str) -> Option<Arc<dyn ControlSignal>> {
        self.signals.get(name).map(|s| s.clone())
    }

    /// Get all signal names
    pub fn get_signal_names(&self) -> Vec<String> {
        self.signals.iter().map(|s| s.key().clone()).collect()
    }
}
```

### 5.2 Signal Metrics Implementation

```rust
/// Metrics for control signals
pub struct ControlSignalMetrics {
    /// Component ID
    id: String,
    
    /// Current signal value
    current_value: AtomicF64,
    
    /// Signal update count
    update_count: AtomicU64,
    
    /// Last update timestamp
    last_update: AtomicU64,
    
    /// Minimum value observed
    min_value: AtomicF64,
    
    /// Maximum value observed
    max_value: AtomicF64,
    
    /// Callback count
    callback_count: AtomicU64,
    
    /// History of signal values
    /// Protected with RwLock but updated selectively
    value_history: Arc<RwLock<VecDeque<(u64, f64)>>>,
}
```

## 6. Saturation Awareness with Transaction Profiler Integration

The Rate Controller can adapt based on client-side saturation metrics provided by the Transaction Profiler. This section details the integration between these components.

### 6.1 Saturation Metrics

```rust
/// Client saturation metrics from Transaction Profiler
#[derive(Debug, Clone)]
pub struct SaturationMetrics {
    /// Average overhead percentage across recent transactions
    pub avg_overhead_percentage: f64,
    /// 95th percentile overhead percentage
    pub p95_overhead_percentage: f64,
    /// Average read delay in microseconds
    pub avg_read_delay_us: f64,
    /// 95th percentile read delay
    pub p95_read_delay_us: u64,
    /// Classification of current saturation
    pub level: SaturationLevel,
    /// Current client resource utilization metrics
    pub resource_utilization: ResourceUtilization,
    /// Recommended actions based on saturation level
    pub recommendation: SaturationAction,
    /// Detailed analysis if available
    pub analysis: Option<String>,
}

/// Client saturation level classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaturationLevel {
    /// Insignificant saturation (<10% overhead)
    None,
    /// Mild saturation (10-20% overhead)
    Mild,
    /// Moderate saturation (20-50% overhead)
    Moderate,
    /// Severe saturation (>50% overhead)
    Severe,
}
```

### 6.2 Integration with Transaction Profiler

```rust
/// Saturation signal provider that integrates with Transaction Profiler
pub struct SaturationSignalProvider {
    /// Signal ID
    id: String,
    
    /// Profiler for detailed transaction analysis
    profiler: Arc<dyn TransactionProfiler>,
    
    /// Current saturation metrics
    current_metrics: Arc<RwLock<SaturationMetrics>>,
    
    /// Saturation level history
    history: Arc<Mutex<Vec<(u64, SaturationLevel)>>>,
    
    /// Update callbacks
    callbacks: Arc<Mutex<Vec<Box<dyn Fn(f64) + Send + Sync>>>>,
    
    /// Start time
    start_time: Instant,
    
    /// Last update time
    last_update: Arc<AtomicU64>,
    
    /// Update interval
    update_interval: Duration,
    
    /// Signal metrics
    metrics: Arc<ControlSignalMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl SaturationSignalProvider {
    /// Create a new saturation signal provider that integrates with Transaction Profiler
    pub fn new(
        id: String,
        profiler: Arc<dyn TransactionProfiler>,
        update_interval: Duration,
    ) -> Self {
        // Initial signal value of 0.0 (no saturation)
        let metrics = Arc::new(ControlSignalMetrics::new(
            id.clone(), 0.0
        ));
        
        Self {
            id,
            profiler,
            current_metrics: Arc::new(RwLock::new(SaturationMetrics::default())),
            history: Arc::new(Mutex::new(Vec::new())),
            callbacks: Arc::new(Mutex::new(Vec::new())),
            start_time: Instant::now(),
            last_update: Arc::new(AtomicU64::new(0)),
            update_interval,
            metrics,
            registry_handle: Mutex::new(None),
        }
    }
    
    /// Integrate saturation signal with Transaction Profiler
    pub fn integrate_with_profiler(
        &self, 
        profiler: Arc<dyn TransactionProfiler>
    ) -> Arc<dyn ControlSignal> {
        let provider = SaturationSignalProvider::new(
            format!("{}-saturation", self.id),
            profiler,
            Duration::from_millis(500),
        );
        
        Arc::new(provider)
    }
    
    /// Start background task to update saturation metrics
    pub fn start_monitoring(&self) -> JoinHandle<()> {
        let profiler = self.profiler.clone();
        let current_metrics = self.current_metrics.clone();
        let history = self.history.clone();
        let callbacks = self.callbacks.clone();
        let start_time = self.start_time;
        let last_update = self.last_update.clone();
        let update_interval = self.update_interval;
        let metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(update_interval);
            
            loop {
                interval.tick().await;
                
                // Get saturation metrics from profiler
                let saturation = profiler.get_saturation_metrics();
                
                // Update current metrics
                let mut current = current_metrics.write().unwrap();
                *current = saturation.clone();
                
                // Update history
                let now = start_time.elapsed().as_millis() as u64;
                let mut hist = history.lock().unwrap();
                hist.push((now, saturation.level));
                
                // Update timestamp
                last_update.store(now, Ordering::Relaxed);
                
                // Map saturation level to a numerical value
                let value = match saturation.level {
                    SaturationLevel::None => 0.0,
                    SaturationLevel::Mild => 0.33,
                    SaturationLevel::Moderate => 0.66,
                    SaturationLevel::Severe => 1.0,
                };
                
                // Update signal metrics
                metrics.update_value(value);
                
                // Notify callbacks
                let callbacks_ref = callbacks.lock().unwrap();
                for callback in callbacks_ref.iter() {
                    callback(value);
                    metrics.record_callback();
                }
            }
        })
    }
}
```

### 6.3 Saturation-Aware Controller

```rust
/// Adapter that adjusts a rate controller based on saturation
pub struct SaturationRateAdapter {
    /// Controller ID
    id: String,
    
    /// Underlying controller
    controller: Arc<dyn RateController>,
    
    /// Saturation signal
    saturation_signal: Arc<dyn ControlSignal>,
    
    /// Threshold for moderate saturation
    moderate_threshold: f64,
    
    /// Threshold for severe saturation
    severe_threshold: f64,
    
    /// Rate adjustments due to saturation
    adjustments: Arc<Mutex<Vec<(u64, RateAdjustment)>>>,
    
    /// Start time
    start_time: Instant,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}
```

## 7. Rate Smoothing for Client Session Consistency

### 7.1 Rate Smoother with Session Awareness

```rust
/// Rate smoother to prevent abrupt rate changes that would create unrealistic session patterns
pub struct RateSmoother {
    /// Component ID
    id: String,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Smoothing configuration
    config: RateSmoothingConfig,
    
    /// Last adjustment time
    last_adjustment: Mutex<Instant>,
    
    /// Rate adjustment history
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Start time
    start_time: Instant,
    
    /// Session impact analysis
    session_impact: Arc<RwLock<Option<SessionImpact>>>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl RateSmoother {
    /// Apply smoothing to a target RPS value
    pub fn smooth_rate(&self, target_rps: u64) -> (u64, Option<SessionImpact>) {
        if !self.config.enabled {
            return (target_rps, None);
        }
        
        let current_rps = self.metrics.current_rps.load(Ordering::Relaxed);
        
        // If minimal change, return directly
        if (target_rps as i64 - current_rps as i64).abs() <= 1 {
            return (target_rps, None);
        }
        
        // Check if enough time has passed since last adjustment
        let mut last_adjustment = self.last_adjustment.lock().unwrap();
        let elapsed_ms = last_adjustment.elapsed().as_millis() as u64;
        
        if elapsed_ms < self.config.min_adjustment_interval_ms {
            // Too soon, return current RPS
            return (current_rps, None);
        }
        
        // Calculate maximum allowed change based on time elapsed
        let elapsed_seconds = elapsed_ms as f64 / 1000.0;
        
        // Relative limit
        let max_rel_change = self.config.max_relative_change_per_second * elapsed_seconds;
        let rel_limit = (current_rps as f64 * max_rel_change).round() as u64;
        
        // Absolute limit
        let abs_limit = (self.config.max_absolute_change_per_second as f64 * elapsed_seconds).round() as u64;
        
        // Apply the more restrictive limit
        let max_change = rel_limit.min(abs_limit).max(1); // Ensure at least 1 RPS change
        
        // Calculate direction and magnitude of change
        let is_increase = target_rps > current_rps;
        let raw_change = if is_increase {
            target_rps - current_rps
        } else {
            current_rps - target_rps
        };
        
        // Apply potentially different limits for increases vs decreases
        let limited_change = if !is_increase && !self.config.limit_decreases {
            // Don't limit decreases if configured that way
            raw_change
        } else {
            // Apply limit
            raw_change.min(max_change)
        };
        
        // Calculate new RPS
        let new_rps = if is_increase {
            current_rps + limited_change
        } else {
            current_rps - limited_change
        };
        
        // Create appropriate session impact information
        let session_impact = if limited_change > 0 {
            // Create session impact information based on magnitude of change
            let relative_change = limited_change as f64 / current_rps as f64;
            
            if relative_change > 0.2 {
                // Significant change - suggest specific session impacts
                Some(if is_increase {
                    SessionImpact::GradualChange {
                        duration_ms: (limited_change as f64 * 100.0) as u64, // Scale with change size
                        easing: EasingFunction::EaseIn,
                    }
                } else {
                    SessionImpact::GradualChange {
                        duration_ms: (limited_change as f64 * 100.0) as u64,
                        easing: EasingFunction::EaseOut,
                    }
                })
            } else {
                // Smaller change - simple activity adjustment
                Some(SessionImpact::ModifyActivityLevel {
                    multiplier: new_rps as f64 / current_rps as f64,
                })
            }
        } else {
            None
        };
        
        // Update metrics for smoothing
        self.metrics.record_smoothing(target_rps, new_rps);
        
        // Update last adjustment time
        *last_adjustment = Instant::now();
        
        // Record adjustment if significant
        if new_rps != current_rps {
            let context = format!(
                "Target: {}, Limited change: {}/{} RPS ({}%)",
                target_rps, limited_change, raw_change,
                (limited_change as f64 / current_rps as f64 * 100.0).round()
            );
            
            // Update metrics with new RPS
            self.metrics.update_rps(new_rps, &context);
            
            // Record adjustment
            let now = self.start_time.elapsed().as_millis() as u64;
            let adjustment = RateAdjustment {
                new_rps,
                old_rps: current_rps,
                reason: AdjustmentReason::RateSmoothing,
                context,
            };
            
            let mut adjustments = self.adjustments.lock().unwrap();
            adjustments.push((now, adjustment));
            
            // Store session impact
            if let Some(impact) = &session_impact {
                let mut impact_lock = self.session_impact.write().unwrap();
                *impact_lock = Some(impact.clone());
            }
        }
        
        (new_rps, session_impact)
    }
}
```

### 7.2 Smoothed Rate Controller with Session Impact

```rust
/// Rate controller that applies smoothing to another controller with session awareness
pub struct SmoothedRateController {
    /// Controller ID
    id: String,
    
    /// Underlying controller
    controller: Arc<dyn RateController>,
    
    /// Rate smoother
    smoother: Arc<RateSmoother>,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl RateController for SmoothedRateController {
    fn get_current_rps(&self) -> u64 {
        // Get target RPS from underlying controller
        let target_rps = self.controller.get_current_rps();
        
        // Apply smoothing
        let (smoothed_rps, _) = self.smoother.smooth_rate(target_rps);
        
        smoothed_rps
    }

    fn get_session_control_signals(&self) -> SessionControlSignals {
        // Get base signals from underlying controller
        let mut signals = self.controller.get_session_control_signals();
        
        // Get target RPS from underlying controller
        let target_rps = self.controller.get_current_rps();
        
        // Apply smoothing with session impact
        let (smoothed_rps, session_impact) = self.smoother.smooth_rate(target_rps);
        
        // Modify signals with smoothed RPS
        signals.target_rps = smoothed_rps;
        
        // Add session impact if available
        if let Some(impact) = session_impact {
            // Create a rate change event if one doesn't exist
            if signals.rate_change_event.is_none() {
                signals.rate_change_event = Some(RateChangeEvent {
                    new_rps: smoothed_rps,
                    old_rps: self.metrics.current_rps.load(Ordering::Relaxed),
                    reason: AdjustmentReason::RateSmoothing,
                    session_impact: impact,
                });
            } else {
                // Update existing rate change event
                if let Some(event) = &mut signals.rate_change_event {
                    event.new_rps = smoothed_rps;
                    event.session_impact = impact;
                }
            }
        }
        
        signals
    }

    // Other methods implementation...
}
```

## 8. Distributed Load Control with Session Coordination

### 8.1 Distributed Coordination Interface

```rust
/// Interface for coordinating distributed load testing
pub trait DistributedCoordinator: Send + Sync {
    /// Initialize coordination with other nodes
    async fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>>;

    /// Register this node with the coordinator
    async fn register_node(&mut self, node_info: NodeInfo) -> Result<NodeAssignment, Box<dyn std::error::Error>>;

    /// Report node status including saturation metrics
    async fn report_node_status(&self, status: NodeStatus) -> Result<(), Box<dyn std::error::Error>>;

    /// Get global test status and control signals
    async fn get_global_status(&self) -> Result<GlobalTestStatus, Box<dyn std::error::Error>>;

    /// Synchronize test start across all nodes
    async fn sync_test_start(&self) -> Result<Instant, Box<dyn std::error::Error>>;

    /// Report final node results
    async fn report_final_results(&self, results: NodeResults) -> Result<(), Box<dyn std::error::Error>>;

    /// Shutdown coordination
    async fn shutdown(&self) -> Result<(), Box<dyn std::error::Error>>;
}

/// Node information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique identifier for this node
    pub node_id: String,
    /// Network capabilities
    pub capabilities: NodeCapabilities,
    /// Maximum RPS this node can generate
    pub max_rps: u64,
    /// Maximum concurrent connections
    pub max_concurrent: usize,
}

/// Node assignment from coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAssignment {
    /// Target RPS for this node
    pub target_rps: u64,
    /// Assigned test parameters
    pub test_params: TestParameters,
    /// URL patterns to test (may be a subset)
    pub url_patterns: Vec<String>,
    /// Whether this node should collect detailed samples
    pub detailed_sampling: bool,
    /// Session creation parameters
    pub session_params: Option<SessionDistributionParams>,
}

/// Session distribution parameters for distributed testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDistributionParams {
    /// Target session creation rate for this node
    pub session_creation_rate: f64,
    /// Client type distribution for this node
    pub client_distribution: HashMap<String, f64>,
    /// Geographic distribution (if applicable)
    pub geo_distribution: Option<HashMap<String, f64>>,
}

/// Node status report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    /// Current RPS being generated
    pub current_rps: u64,
    /// Current saturation metrics
    pub saturation: SaturationMetrics,
    /// Success rate (0.0-1.0)
    pub success_rate: f64,
    /// Current P99 latency in milliseconds
    pub p99_latency_ms: f64,
    /// Session statistics
    pub session_stats: SessionStats,
}

/// Session statistics for node status reports
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    /// Number of active sessions
    pub active_sessions: u64,
    /// Session creation rate
    pub session_creation_rate: f64,
    /// Session distribution by type
    pub type_distribution: HashMap<String, f64>,
}
```

### 8.2 Distributed Rate Adapter with Session Coordination

```rust
/// Distributed rate controller adapter
pub struct DistributedRateAdapter {
    /// Controller ID
    id: String,
    
    /// Underlying local controller
    local_controller: Arc<dyn RateController>,
    
    /// Load proportion smoother
    load_smoother: Arc<LoadProportionSmoother>,
    
    /// Target metric updater
    target_updater: Arc<AtomicF64>,
    
    /// Coordinator for communication
    coordinator: Arc<dyn DistributedCoordinator>,
    
    /// Node status
    node_status: Arc<RwLock<NodeStatus>>,
    
    /// Session parameters
    session_params: Arc<RwLock<Option<SessionDistributionParams>>>,
    
    /// Rate adjustment history
    adjustments: Mutex<Vec<(u64, RateAdjustment)>>,
    
    /// Start time
    start_time: Instant,
    
    /// Metrics collection
    metrics: Arc<RateControllerMetrics>,
    
    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl RateController for DistributedRateAdapter {
    fn get_current_rps(&self) -> u64 {
        // Get base RPS from local controller
        let base_rps = self.local_controller.get_current_rps();
        
        // Apply load proportion factor with smoothing
        let proportion = self.load_smoother.get_current_factor();
        
        // Calculate adjusted RPS
        let adjusted_rps = (base_rps as f64 * proportion).round() as u64;
        
        // Update metrics
        let current_proportion = self.metrics.distributed_proportion_factor.load(Ordering::Relaxed);
        if (proportion - current_proportion).abs() > 0.01 {
            self.metrics.update_distributed_metrics(proportion);
        }
        
        adjusted_rps
    }
    
    fn get_session_control_signals(&self) -> SessionControlSignals {
        // Get base signals from local controller
        let mut signals = self.local_controller.get_session_control_signals();
        
        // Apply distributed adjustment to target RPS
        signals.target_rps = self.get_current_rps();
        
        // Apply session parameters from coordinator if available
        let session_params = self.session_params.read().unwrap().clone();
        if let Some(params) = session_params {
            // Override session creation rate
            signals.session_creation_rate = Some(params.session_creation_rate);
            
            // Apply client type distribution
            let mut adjustments = HashMap::new();
            for (type_str, weight) in params.client_distribution {
                // Convert string to ClientType
                if let Ok(client_type) = serde_json::from_str::<ClientType>(&type_str) {
                    adjustments.insert(client_type, weight);
                }
            }
            
            if !adjustments.is_empty() {
                signals.session_type_adjustments = Some(adjustments);
            }
        }
        
        signals
    }
    
    // Other methods implementation...
}
```

## 9. Rate Controller Factory with Static Dispatch

### 9.1 Factory Implementation with Generic Types

```rust
/// Factory for creating rate controllers with static dispatch
pub struct RateControllerFactory;

impl RateControllerFactory {
    /// Create a rate controller based on configuration
    pub fn create<M, C>(
        config: &LoadTestConfig,
        metrics: M,
        session_control: C,
        profiler: Option<impl TransactionProfiler>,
    ) -> impl RateController<M, impl Signal, C>
    where
        M: MetricsProvider,
        C: SessionControl,
    {
        // Generate a unique ID for this controller
        let id = format!("controller-{}", Uuid::new_v4());
        
        // Create base controller based on mode
        let controller = match &config.rate_control_mode {
            RateControlMode::Static => {
                StaticRateController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    metrics,
                    session_control,
                )
            },
            RateControlMode::Pid { 
                target_metric,
                signal_type,
                kp, ki, kd,
                update_interval_ms,
            } => {
                match signal_type {
                    SignalType::LatencyP99 => {
                        let signal = LatencyP99Signal::new();
                        PidController::new(
                            id.clone(),
                            *target_metric,
                            *kp, *ki, *kd,
                            signal,
                            session_control,
                            metrics,
                            config.initial_rps,
                            config.min_rps,
                            config.max_rps,
                            *update_interval_ms,
                        )
                    },
                    SignalType::ErrorRate => {
                        let signal = ErrorRateSignal::new();
                        PidController::new(
                            id.clone(),
                            *target_metric,
                            *kp, *ki, *kd,
                            signal,
                            session_control,
                            metrics,
                            config.initial_rps,
                            config.min_rps,
                            config.max_rps,
                            *update_interval_ms,
                        )
                    },
                    SignalType::Saturation => {
                        if let Some(prof) = profiler {
                            let provider = SaturationMetricsProvider::new(
                                format!("{}-saturation", id),
                                prof,
                                Duration::from_millis(*update_interval_ms),
                            );
                            
                            // Start monitoring
                            provider.start_monitoring();
                            
                            // Create signal
                            let signal = provider.create_signal();
                            
                            PidController::new(
                                id.clone(),
                                *target_metric,
                                *kp, *ki, *kd,
                                signal,
                                session_control,
                                metrics,
                                config.initial_rps,
                                config.min_rps,
                                config.max_rps,
                                *update_interval_ms,
                            )
                        } else {
                            // Fall back to latency signal if no profiler
                            let signal = LatencyP99Signal::new();
                            
                            PidController::new(
                                id.clone(),
                                *target_metric,
                                *kp, *ki, *kd,
                                signal,
                                session_control,
                                metrics,
                                config.initial_rps,
                                config.min_rps,
                                config.max_rps,
                                *update_interval_ms,
                            )
                        }
                    },
                    SignalType::External(name) => {
                        let signal = ExternalSignal::new(name.to_string());
                        
                        PidController::new(
                            id.clone(),
                            *target_metric,
                            *kp, *ki, *kd,
                            signal,
                            session_control,
                            metrics,
                            config.initial_rps,
                            config.min_rps,
                            config.max_rps,
                            *update_interval_ms,
                        )
                    },
                    // Other signal types...
                    _ => {
                        // Default to latency signal
                        let signal = LatencyP99Signal::new();
                        
                        PidController::new(
                            id.clone(),
                            *target_metric,
                            *kp, *ki, *kd,
                            signal,
                            session_control,
                            metrics,
                            config.initial_rps,
                            config.min_rps,
                            config.max_rps,
                            *update_interval_ms,
                        )
                    }
                }
            },
            RateControlMode::StepFunction(steps) => {
                StepFunctionController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    steps.clone(),
                    metrics,
                    session_control,
                )
            },
            // Custom controller or other types...
            _ => {
                // Default to static controller
                StaticRateController::new(
                    id.clone(),
                    config.initial_rps,
                    config.min_rps,
                    config.max_rps,
                    metrics,
                    session_control,
                )
            }
        };
        
        // Apply rate smoothing if enabled
        let controller = if config.rate_smoothing_config.enabled {
            let smoothing_metrics = DefaultMetrics::new(format!("smoother-{}", id));
            SmoothedRateController::new(
                format!("smoothed-{}", id),
                controller,
                config.rate_smoothing_config.clone(),
                smoothing_metrics,
            )
        } else {
            controller
        };
        
        // Return the controller
        controller
    }
    
    /// Create a controller for distributed testing
    pub fn create_distributed<M, C, P>(
        config: &LoadTestConfig,
        metrics: M,
        session_control: C,
        profiler: Option<P>,
        coordinator: impl DistributedCoordinator,
    ) -> impl RateController<M, impl Signal, C>
    where
        M: MetricsProvider,
        C: SessionControl,
        P: TransactionProfiler,
    {
        // First create base controller
        let controller = Self::create(config, metrics.clone(), session_control, profiler);
        
        // Apply distributed wrapper
        let distributed_metrics = DefaultMetrics::new(format!("distributed-{}", Uuid::new_v4()));
        DistributedRateController::new(
            format!("distributed-{}", Uuid::new_v4()),
            controller,
            coordinator,
            config.initial_rps,
            config.min_rps,
            config.max_rps,
            1.0, // Initial proportion
            0.1, // Maximum ramp rate
            distributed_metrics,
        )
    }
}
```

### 9.2 Builder Pattern with Static Dispatch

```rust
impl LoadTestBuilder {
    /// Use a generic PID controller with specified signal
    pub fn with_pid_controller<S: Signal>(
        mut self,
        target: f64,
        kp: f64, ki: f64, kd: f64,
        signal_type: SignalType,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::Pid {
            target_metric: target,
            signal_type,
            kp,
            ki,
            kd,
            update_interval_ms: 500,
        };
        self
    }
    
    /// Use a saturation-aware PID controller
    pub fn with_saturation_aware_pid(
        mut self,
        target_saturation: f64,
        kp: f64, ki: f64, kd: f64,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::Pid {
            target_metric: target_saturation,
            signal_type: SignalType::Saturation,
            kp,
            ki,
            kd,
            update_interval_ms: 500,
        };
        self
    }
    
    /// Use an external signal PID controller
    pub fn with_external_signal_pid(
        mut self,
        target: f64,
        kp: f64, ki: f64, kd: f64,
        signal_name: &str,
    ) -> Self {
        self.config.rate_control_mode = RateControlMode::Pid {
            target_metric: target,
            signal_type: SignalType::External(signal_name.to_string()),
            kp,
            ki,
            kd,
            update_interval_ms: 500,
        };
        self
    }
    
    /// With client session control configuration
    pub fn with_session_control(
        mut self,
        config: SessionControlConfig,
    ) -> Self {
        self.session_control_config = Some(config);
        self
    }
    
    /// Build the rate controller with static dispatch
    fn build_rate_controller<C: ClientSessionManager>(&self, session_manager: &C) -> impl RateController<DefaultMetrics, impl Signal, DefaultSessionControl> {
        // Create metrics provider
        let metrics = DefaultMetrics::new(format!("controller-{}", Uuid::new_v4()));
        
        // Create session control
        let session_control = if let Some(config) = &self.session_control_config {
            DefaultSessionControl::new(config.clone())
        } else {
            DefaultSessionControl::new(SessionControlConfig::default())
        };
        
        // Create controller
        RateControllerFactory::create(
            &self.config,
            metrics,
            session_control,
            self.get_profiler(),
        )
    }
    
    // Other builder methods...
}

/// Session control configuration
#[derive(Debug, Clone)]
pub struct SessionControlConfig {
    /// Whether to prioritize request rate over session fidelity
    pub prioritize_request_rate: bool,
    /// How to ramp up/down load
    pub ramp_mode: RampMode,
    /// Whether to apply constant think time across load levels
    pub constant_think_time: bool,
    /// Maximum session count
    pub max_sessions: Option<u64>,
}

/// Load ramp behavior
#[derive(Debug, Clone)]
pub enum RampMode {
    /// Add/remove sessions to change load
    AdjustSessionCount,
    /// Modify think time within sessions
    AdjustThinkTime,
    /// Change request pattern within sessions
    AdjustRequestPattern,
    /// Combined approach (automatic)
    Automatic,
}

/// Default session control implementation
pub struct DefaultSessionControl {
    /// Configuration
    config: SessionControlConfig,
}

impl DefaultSessionControl {
    /// Create a new default session control
    pub fn new(config: SessionControlConfig) -> Self {
        Self { config }
    }
}

impl SessionControl for DefaultSessionControl {
    type SessionSignals = SessionControlSignals;
    
    fn create_signals(&self, target_rps: u64, last_rps: u64) -> Self::SessionSignals {
        // Create session signals based on configuration
        let mut signals = SessionControlSignals {
            target_rps,
            session_creation_rate: None,
            target_active_sessions: None,
            session_type_adjustments: None,
            prioritize_request_rate: self.config.prioritize_request_rate,
            rate_change_event: None,
        };
        
        // Calculate session creation rate if needed
        if target_rps > 0 {
            // Estimate reasonable session creation rate based on RPS
            // This is a simplified example - real implementation would be more sophisticated
            let estimated_creation_rate = (target_rps as f64 / 20.0).max(1.0);
            signals.session_creation_rate = Some(estimated_creation_rate);
        }
        
        // Create rate change event if significant change
        if (target_rps as f64 - last_rps as f64).abs() / last_rps as f64 > 0.1 {
            // More than 10% change
            signals.rate_change_event = Some(self.create_rate_change_event(target_rps, last_rps));
        }
        
        signals
    }
    
    fn update(&mut self, _metrics: &RequestMetrics) {
        // Update based on metrics if needed
    }
}

impl DefaultSessionControl {
    /// Create a rate change event
    fn create_rate_change_event(&self, new_rps: u64, old_rps: u64) -> RateChangeEvent {
        let relative_change = (new_rps as f64 - old_rps as f64) / old_rps as f64;
        
        // Determine session impact based on ramp mode
        let session_impact = match self.config.ramp_mode {
            RampMode::AdjustSessionCount => {
                if relative_change > 0.0 {
                    // Increasing load
                    SessionImpact::AddSessions {
                        count: ((new_rps - old_rps) as f64 / 10.0).ceil() as u64,
                        priority_types: vec![],
                    }
                } else {
                    // Decreasing load
                    SessionImpact::RemoveSessions {
                        count: ((old_rps - new_rps) as f64 / 10.0).ceil() as u64,
                        strategy: SessionRemovalStrategy::LeastActiveFirst,
                    }
                }
            },
            RampMode::AdjustThinkTime => {
                // Modify activity level
                SessionImpact::ModifyActivityLevel {
                    multiplier: new_rps as f64 / old_rps as f64,
                }
            },
            RampMode::AdjustRequestPattern => {
                // Gradual change
                SessionImpact::GradualChange {
                    duration_ms: 5000, // 5 seconds
                    easing: EasingFunction::EaseInOut,
                }
            },
            RampMode::Automatic => {
                // Choose based on magnitude
                if relative_change.abs() > 0.3 {
                    // Large change - add/remove sessions
                    if relative_change > 0.0 {
                        SessionImpact::AddSessions {
                            count: ((new_rps - old_rps) as f64 / 10.0).ceil() as u64,
                            priority_types: vec![],
                        }
                    } else {
                        SessionImpact::RemoveSessions {
                            count: ((old_rps - new_rps) as f64 / 10.0).ceil() as u64,
                            strategy: SessionRemovalStrategy::LeastActiveFirst,
                        }
                    }
                } else {
                    // Smaller change - adjust think time
                    SessionImpact::ModifyActivityLevel {
                        multiplier: new_rps as f64 / old_rps as f64,
                    }
                }
            }
        };
        
        RateChangeEvent {
            new_rps,
            old_rps,
            reason: AdjustmentReason::PidControl,
            session_impact,
        }
    }
}

/// Default metrics implementation
pub struct DefaultMetrics {
    /// Component ID
    id: String,
    /// Metrics data
    data: Arc<DashMap<String, MetricValue>>,
}

impl DefaultMetrics {
    /// Create new default metrics
    pub fn new(id: String) -> Self {
        Self {
            id,
            data: Arc::new(DashMap::new()),
        }
    }
    
    /// Set a metric value
    pub fn set(&self, key: &str, value: MetricValue) {
        self.data.insert(key.to_string(), value);
    }
}

impl MetricsProvider for DefaultMetrics {
    fn get_metrics(&self) -> ComponentMetrics {
        let mut metrics = HashMap::new();
        
        // Copy all metrics from dashmap
        for entry in self.data.iter() {
            metrics.insert(entry.key().clone(), entry.value().clone());
        }
        
        ComponentMetrics {
            component_type: "RateController".to_string(),
            component_id: self.id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            metrics,
            status: None,
        }
    }
    
    fn get_component_type(&self) -> &str {
        "RateController"
    }
    
    fn get_component_id(&self) -> &str {
        &self.id
    }
}
```

## 10. Usage Examples with Static Dispatch

### 10.1 Basic Controller with Generic Types

```rust
// Example 1: Create a PID controller with static dispatch and client session integration
fn create_test_controller() -> impl RateController<DefaultMetrics, LatencyP99Signal, DefaultSessionControl> {
    // Create session control
    let session_control = DefaultSessionControl::new(SessionControlConfig {
        prioritize_request_rate: false,
        ramp_mode: RampMode::Automatic,
        constant_think_time: false,
        max_sessions: Some(10000),
    });
    
    // Create metrics provider
    let metrics = DefaultMetrics::new("controller-1".to_string());
    
    // Create latency signal
    let signal = LatencyP99Signal::new();
    
    // Create PID controller with static dispatch
    PidController::new(
        "controller-1".to_string(),
        100.0,  // Target 100ms latency
        0.1,    // kp
        0.01,   // ki
        0.001,  // kd
        signal,
        session_control,
        metrics,
        100,    // initial RPS
        10,     // min RPS
        1000,   // max RPS
        500,    // update interval ms
    )
}

// Use the controller with client session manager
fn run_test_with_controller() {
    let controller = create_test_controller();
    
    // Create client session manager
    let session_manager = ClientSessionManagerImpl::new(
        "session-manager-1".to_string(),
        ClientSessionConfig {
            session_creation_rate: 10.0,  // 10 new sessions per second
            avg_session_duration_secs: 300.0,  // 5 minute average session
            distribution: {
                let mut dist = HashMap::new();
                dist.insert(
                    ClientType::Browser {
                        browser_type: BrowserType::Chrome,
                        http2_enabled: true,
                        connection_limit: 6,
                    },
                    0.7,  // 70% Chrome browsers
                );
                dist.insert(
                    ClientType::Mobile {
                        platform: MobilePlatform::iOS,
                        network_type: NetworkType::WiFi,
                        http2_enabled: true,
                    },
                    0.3,  // 30% iOS mobile clients
                );
                dist
            },
        },
        controller,
    );
    
    // Create request scheduler
    let scheduler = PoissonScheduler::new(
        "scheduler-1".to_string(),
        SchedulerConfig {
            sampling_rate: 0.01,
            tick_resolution_us: 100,
            jitter_threshold_ns: 1000,
        },
    );
    
    // Create test pipeline
    let pipeline = LoadTestImpl::new(
        session_manager,
        scheduler,
        // Other components...
    );
    
    // Run the test
    pipeline.run();
}
```

### 10.2 Using Factory with Type Parameters

```rust
// Example 2: Using rate controller factory with type parameters
fn create_test() {
    // Create load test configuration
    let config = LoadTestConfig {
        initial_rps: 100,
        min_rps: 10,
        max_rps: 1000,
        rate_control_mode: RateControlMode::Pid {
            target_metric: 100.0,
            signal_type: SignalType::LatencyP99,
            kp: 0.1,
            ki: 0.01,
            kd: 0.001,
            update_interval_ms: 500,
        },
        rate_smoothing_config: RateSmoothingConfig {
            enabled: true,
            max_relative_change_per_second: 0.1,
            max_absolute_change_per_second: 50,
            min_adjustment_interval_ms: 100,
            limit_decreases: true,
        },
        // Other configuration...
    };
    
    // Create metrics
    let metrics = DefaultMetrics::new("controller-metrics".to_string());
    
    // Create session control
    let session_control = DefaultSessionControl::new(SessionControlConfig {
        prioritize_request_rate: false,
        ramp_mode: RampMode::Automatic,
        constant_think_time: false,
        max_sessions: Some(10000),
    });
    
    // Create transaction profiler
    let profiler = StandardTransactionProfiler::new(
        "profiler-1".to_string(),
        ProfilingConfig {
            mode: ProfilingMode::RuntimeOnly,
            sample_rate: 0.01,
        },
    );
    
    // Create rate controller with factory (static dispatch)
    let controller = RateControllerFactory::create(
        &config,
        metrics,
        session_control,
        Some(profiler),
    );
    
    // Create client session manager with the controller
    let session_manager = ClientSessionManagerImpl::new(
        "session-manager-1".to_string(),
        ClientSessionConfig {
            session_creation_rate: 10.0,
            avg_session_duration_secs: 300.0,
            distribution: client_distribution,
        },
        controller,
    );
    
    // Create and run the test
    // ...
}
```

### 10.3 Builder Pattern with Static Types

```rust
// Example 3: Using builder pattern with static dispatch
fn create_load_test() -> impl LoadTest {
    // Create load test builder
    let builder = LoadTestBuilder::new();
    
    // Configure test
    let builder = builder
        .with_urls(vec!["https://example.com/api".to_string()])
        .with_pid_controller(
            100.0,                      // Target 100ms latency 
            0.1, 0.01, 0.001,           // PID gains
            SignalType::LatencyP99,     // Signal type
        )
        .with_session_control(SessionControlConfig {
            prioritize_request_rate: false,
            ramp_mode: RampMode::Automatic,
            constant_think_time: false,
            max_sessions: Some(10000),
        })
        .with_client_session_config(ClientSessionConfig {
            session_creation_rate: 10.0,
            avg_session_duration_secs: 300.0,
            distribution: client_distribution,
            think_time_distribution: ThinkTimeDistribution::LogNormal {
                mean_ms: 2000.0,
                std_dev_ms: 500.0,
            },
        })
        .with_scheduler_config(SchedulerConfig {
            sampling_rate: 0.01,
            tick_resolution_us: 100,
            jitter_threshold_ns: 1000,
        })
        .with_transaction_profiling(ProfilingConfig {
            mode: ProfilingMode::Full,
            sample_rate: 0.01,
        })
        .with_network_condition_config(NetworkConditionConfig {
            slow_connection_probability: 0.2,
            consistent_per_session: true,
            // Other network settings...
        });
    
    // Build the test with concrete types
    builder.build::<
        PidController<DefaultMetrics, LatencyP99Signal, DefaultSessionControl>,
        StandardClientSessionManager<PidController<DefaultMetrics, LatencyP99Signal, DefaultSessionControl>>,
        PoissonScheduler,
        StandardGenerator,
        HeaderTransformer,
        ConnectionPoolExecutor,
        StandardTransactionProfiler,
        StandardResultsCollector,
        StandardNetworkSimulator
    >()
}
```

### 10.4 Saturation-Aware Controller with Static Dispatch

```rust
// Example 4: Create a saturation-aware controller with static dispatch
fn create_saturation_aware_controller() -> impl RateController<DefaultMetrics, SaturationSignal<StandardTransactionProfiler>, DefaultSessionControl> {
    // Create profiler
    let profiler = StandardTransactionProfiler::new(
        "profiler-1".to_string(),
        ProfilingConfig {
            mode: ProfilingMode::Full,
            sample_rate: 0.01,
        },
    );
    
    // Create saturation provider
    let saturation_provider = SaturationMetricsProvider::new(
        "saturation-provider-1".to_string(),
        profiler,
        Duration::from_millis(500),
    );
    
    // Start monitoring
    saturation_provider.start_monitoring();
    
    // Create signal
    let signal = saturation_provider.create_signal();
    
    // Create metrics
    let metrics = DefaultMetrics::new("saturation-controller".to_string());
    
    // Create session control
    let session_control = DefaultSessionControl::new(SessionControlConfig {
        prioritize_request_rate: false,
        ramp_mode: RampMode::Automatic,
        constant_think_time: false,
        max_sessions: Some(10000),
    });
    
    // Create PID controller
    PidController::new(
        "saturation-controller".to_string(),
        0.3,    // Target saturation level of 0.3 (moderate)
        0.2,    // Higher kp for saturation control
        0.02,   // ki
        0.002,  // kd
        signal,
        session_control,
        metrics,
        100,    // initial RPS
        10,     // min RPS
        1000,   // max RPS
        500,    // update interval ms
    )
}
```

### 10.5 Type-Safe Pipeline with Static Dispatch

```rust
// Example 5: Creating a complete pipeline with static dispatch
fn create_pipeline() {
    // Type aliases for clarity
    type MetricsType = DefaultMetrics;
    type SignalType = LatencyP99Signal;
    type SessionControlType = DefaultSessionControl;
    type ControllerType = PidController<MetricsType, SignalType, SessionControlType>;
    type SessionManagerType = StandardClientSessionManager<ControllerType>;
    type SchedulerType = PoissonScheduler;
    
    // Create controller
    let controller: ControllerType = create_test_controller();
    
    // Create session manager
    let session_manager = StandardClientSessionManager::new(
        "session-manager-1".to_string(),
        ClientSessionConfig {
            // Configuration...
        },
        controller,
    );
    
    // Create scheduler
    let scheduler = PoissonScheduler::new(
        "scheduler-1".to_string(),
        SchedulerConfig {
            // Configuration...
        },
    );
    
    // Other components...
    
    // Create the complete pipeline with static dispatch
    let pipeline = LoadTestImpl::<
        ControllerType,
        SessionManagerType,
        SchedulerType,
        // Other component types...
    >::new(
        controller,
        session_manager,
        scheduler,
        // Other components...
    );
    
    // Run the test
    pipeline.run();
}
```

## 11. Conclusion

This revised Rate Control system design provides a comprehensive solution for load testing scenarios, optimized for performance through static dispatch while maintaining flexibility and composability. Key advantages of this design include:

1. **Static Dispatch Performance**: By using generic types and avoiding dynamic dispatch in the critical path, the system achieves maximum performance with full compiler optimization.

2. **Signal-Based Control Unification**: A single generic PID controller can be specialized with different signal implementations (latency, error rate, saturation), creating a unified approach to control while allowing full customization.

3. **Comprehensive Session Control**: Rate controllers translate raw RPS targets into realistic client session behavior through the Session Control interface.

4. **Type-Safe Composition**: Components are designed for type-safe composition while preserving static dispatch, enabling precise compiler checking without runtime overhead.

5. **Realistic Load Patterns**: Smoothing and session-aware adjustments prevent abrupt, unrealistic traffic changes.

6. **Transaction Profiler Integration**: Saturation awareness is implemented as a signal type, allowing any controller to leverage profiling data through a consistent interface.

7. **Distributed Session Coordination**: Distributed nodes can coordinate not just on raw RPS but on session characteristics.

8. **Feedback-Driven Control**: Comprehensive metrics flow from results collection back to rate decisions.

This architecture demonstrates how to maintain high performance with static dispatch while achieving flexible composition through generic programming. It also shows how to integrate multiple control sources (latency, error rates, saturation) into a unified, extensible system that can accurately model real-world client behavior patterns.
