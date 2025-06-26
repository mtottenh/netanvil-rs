 Client Session Management: Session-Based Load Generation with Realistic Behavior

## 1. Introduction and Core Responsibilities

The Client Session Manager sits between the Rate Controller and Request Scheduler in the load testing pipeline, transforming abstract rate targets into realistic client behavior patterns. This component bridges the gap between system-level metrics (requests per second) and realistic client behavior modeling (sessions with think time, correlation, and browsing patterns).

### 1.1 Core Responsibilities

1. **Session Lifecycle Management**: Create, maintain, and terminate client sessions based on statistical models
2. **Request Ticket Generation**: Produce scheduled request tickets for the Request Scheduler based on session behavior
3. **Client Behavior Simulation**: Model realistic client patterns including think time, navigation flows, and session duration
4. **Client Diversity**: Simulate various client types (browser, mobile, API) with appropriate behavior parameters
5. **Traffic Pattern Implementation**: Translate abstract RPS targets into realistic session-driven request patterns
6. **State Correlation**: Maintain session state for stateful interactions (cookies, authentication, shopping carts)
7. **IP Address Management**: Assign and maintain consistent IP addresses for sessions

### 1.2 Architectural Positioning

```
┌────────────────┐        ┌───────────────────┐        ┌──────────────────┐        ┌────────────────┐
│ Rate           │        │ Client Session     │        │ Request          │        │ Request        │
│ Controller     │───────>│ Manager           │───────>│ Scheduler        │───────>│ Pipeline       │
└────────────────┘signals └───────────────────┘tickets └──────────────────┘dispatch└────────────────┘
       ^                           │
       │                           │
       └───────────────────────────┘
           session metrics feedback
```

The Client Session Manager translates the abstract load targets from the Rate Controller into concrete request tickets with realistic timing, correlations, and client context. It uses feedback loops to maintain the target RPS while preserving realistic client behavior.

## 2. Core Interfaces and Types

### 2.1 Client Session Manager Interface

```rust
/// Interface for managing client sessions with static dispatch
pub trait ClientSessionManager<M, C>: Send + Sync
where
    M: MetricsProvider,
    C: ClientType,
{
    /// Run the client session manager
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<impl RateController<_, _, _>>,
        ticket_tx: async_channel::Sender<SchedulerTicket>,
    );

    /// Update distribution of client types
    fn update_client_distribution(&self, distribution: ClientDistribution<C>);

    /// Get current client session statistics
    fn get_session_stats(&self) -> ClientSessionStats<C>;

    /// Signal the manager to terminate
    fn terminate(&self);

    /// Get metrics provider for monitoring
    fn metrics(&self) -> &M;
}

/// Client session statistics
#[derive(Debug, Clone)]
pub struct ClientSessionStats<C: ClientType> {
    /// Number of active sessions
    pub active_sessions: u64,

    /// Total sessions created
    pub total_sessions_created: u64,

    /// Sessions closed
    pub sessions_closed: u64,

    /// Distribution by client type
    pub distribution: HashMap<C, u64>,

    /// Average session duration in seconds
    pub avg_session_duration_secs: f64,

    /// Average requests per session
    pub avg_requests_per_session: f64,

    /// Current session creation rate
    pub current_creation_rate: f64,

    /// Current think time statistics (median, p95)
    pub think_time_stats: ThinkTimeStats,
}

/// Think time statistics
#[derive(Debug, Clone)]
pub struct ThinkTimeStats {
    /// Median think time in milliseconds
    pub median_ms: u64,

    /// 95th percentile think time in milliseconds
    pub p95_ms: u64,

    /// Minimum think time in milliseconds
    pub min_ms: u64,

    /// Maximum think time in milliseconds
    pub max_ms: u64,
}

/// Client distribution configuration
#[derive(Debug, Clone)]
pub struct ClientDistribution<C: ClientType> {
    /// Distribution by client type
    pub distribution: HashMap<C, f64>,

    /// Session creation rate per second
    pub session_creation_rate: f64,

    /// Average session duration in seconds
    pub avg_session_duration_secs: f64,

    /// Variance in session duration (coefficient of variation)
    pub session_duration_variance: f64,

    /// Think time distribution
    pub think_time_distribution: ThinkTimeDistribution,
}

/// Distribution models for think time
#[derive(Debug, Clone)]
pub enum ThinkTimeDistribution {
    /// Fixed think time
    Fixed(u64),

    /// Uniform distribution
    Uniform {
        /// Minimum think time (ms)
        min_ms: u64,

        /// Maximum think time (ms)
        max_ms: u64,
    },

    /// Normal distribution
    Normal {
        /// Mean think time (ms)
        mean_ms: f64,

        /// Standard deviation (ms)
        std_dev_ms: f64,
    },

    /// Log-normal distribution (most realistic for user behavior)
    LogNormal {
        /// Mean think time (ms)
        mean_ms: f64,

        /// Standard deviation (ms)
        std_dev_ms: f64,
    },

    /// Exponential distribution
    Exponential {
        /// Mean think time (ms)
        mean_ms: f64,
    },

    /// Custom distribution function
    Custom(String),
}
```

### 2.2 Client Types and Behavior Models

```rust
/// Client type trait for static dispatch
pub trait ClientType: Debug + Clone + PartialEq + Eq + Hash + Send + Sync + 'static {
    /// Get a string description of this client type
    fn description(&self) -> String;

    /// Get client characteristics
    fn characteristics(&self) -> ClientCharacteristics;

    /// Get connection parameters
    fn connection_params(&self) -> ConnectionParams;
}

/// Default client type implementation
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DefaultClientType {
    /// Browser client
    Browser {
        /// Browser type
        browser_type: BrowserType,

        /// Whether HTTP/2 is enabled
        http2_enabled: bool,

        /// Connection limit
        connection_limit: u32,
    },

    /// Mobile client
    Mobile {
        /// Mobile platform
        platform: MobilePlatform,

        /// Network type
        network_type: NetworkType,

        /// Whether HTTP/2 is enabled
        http2_enabled: bool,
    },

    /// API client
    ApiClient {
        /// Client library
        client_library: ApiClientLibrary,

        /// Whether to use persistent connections
        persistent_connections: bool,
    },

    /// Custom client type
    Custom(String),
}

/// Client characteristics for behavior modeling
#[derive(Debug, Clone)]
pub struct ClientCharacteristics {
    /// Base think time multiplier (1.0 = default)
    pub think_time_multiplier: f64,

    /// Request parallelism factor (1.0 = sequential, higher = more parallel)
    pub parallelism_factor: f64,

    /// Session length multiplier (1.0 = default)
    pub session_length_multiplier: f64,

    /// Typical navigation patterns
    pub navigation_patterns: Vec<NavigationPattern>,

    /// Probability of executing each navigation pattern
    pub pattern_probabilities: Vec<f64>,
}

/// Connection parameters for different client types
#[derive(Debug, Clone)]
pub struct ConnectionParams {
    /// Maximum connections per host
    pub max_connections_per_host: u32,

    /// Whether to use HTTP/2
    pub use_http2: bool,

    /// Whether to use persistent connections
    pub persistent_connections: bool,

    /// Whether to support cookies
    pub supports_cookies: bool,

    /// Maximum requests per connection before recycling
    pub max_requests_per_connection: Option<u32>,
}

/// Navigation pattern for realistic client behavior
#[derive(Debug, Clone)]
pub struct NavigationPattern {
    /// Name of this pattern
    pub name: String,

    /// Sequence of request types in this pattern
    pub sequence: Vec<RequestType>,

    /// Think time modifiers between steps (multipliers)
    pub think_time_modifiers: Vec<f64>,
}

/// Types of requests in navigation patterns
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestType {
    /// Page navigation
    PageNavigation,

    /// API call
    ApiCall,

    /// Resource load (image, CSS, JS)
    ResourceLoad,

    /// Form submission
    FormSubmission,

    /// Authentication
    Authentication,

    /// Custom request type
    Custom(String),
}
```

### 2.3 Session and Request Generation

```rust
/// Client session representation
#[derive(Debug)]
pub struct ClientSession<C: ClientType> {
    /// Unique session ID
    pub id: String,

    /// Client type
    pub client_type: C,

    /// Session start time
    pub start_time: Instant,

    /// Session expected duration
    pub expected_duration: Duration,

    /// Current state
    pub state: SessionState,

    /// Client IP address
    pub client_ip: IpAddr,

    /// Navigation context
    pub navigation_context: NavigationContext,

    /// Session-specific data (cookies, tokens, etc.)
    pub session_data: HashMap<String, String>,

    /// Statistics for this session
    pub stats: SessionStats,
}

/// Session state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Active and generating requests
    Active,

    /// Temporarily idle
    Idle,

    /// In process of closing
    Closing,

    /// Closed/completed
    Closed,
}

/// Navigation context for a session
#[derive(Debug, Clone)]
pub struct NavigationContext {
    /// Current path/endpoint
    pub current_path: String,

    /// Navigation history
    pub history: VecDeque<String>,

    /// Current navigation pattern being executed (if any)
    pub current_pattern: Option<NavigationPattern>,

    /// Step within the current pattern
    pub pattern_step: usize,

    /// Resource dependencies for current page
    pub resource_dependencies: Vec<String>,
}

/// Session statistics
#[derive(Debug, Default, Clone)]
pub struct SessionStats {
    /// Number of requests generated
    pub requests_generated: u64,

    /// Number of completed requests
    pub requests_completed: u64,

    /// Number of error responses
    pub error_responses: u64,

    /// Total think time in milliseconds
    pub total_think_time_ms: u64,

    /// Average think time
    pub avg_think_time_ms: f64,

    /// Last activity timestamp
    pub last_activity: Instant,
}
```

## 3. Implementation

### 3.1 Standard Client Session Manager Implementation

```rust
/// Standard implementation of client session manager with generic types
pub struct StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Component ID
    id: String,

    /// Configuration
    config: Arc<ClientSessionConfig<C>>,

    /// Rate controller reference
    rate_controller: Arc<R>,

    /// Active sessions
    active_sessions: Arc<DashMap<String, ClientSession<C>>>,

    /// RNG for random decisions
    rng: Arc<Mutex<SmallRng>>,

    /// Session creation timer
    creation_timer: Arc<PrecisionTimer>,

    /// Last control signal update
    last_control_update: Arc<AtomicU64>,

    /// Current session creation rate
    current_creation_rate: Arc<AtomicF64>,

    /// Whether the manager is running
    running: Arc<AtomicBool>,

    /// Think time distribution samplers
    think_time_samplers: Arc<DashMap<C, Box<dyn ThinkTimeSampler>>>,

    /// Session duration samplers
    duration_samplers: Arc<DashMap<C, Box<dyn DurationSampler>>>,

    /// Navigation pattern selectors
    pattern_selectors: Arc<DashMap<C, Box<dyn PatternSelector<C>>>>,

    /// Client IP assignment strategy
    ip_strategy: Arc<dyn IpAssignmentStrategy>,

    /// Metrics collection
    metrics: M,

    /// Registry handle if registered
    registry_handle: Mutex<Option<RegistrationHandle>>,
}

impl<M, C, R> StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Create a new standard client session manager
    pub fn new(
        id: String,
        config: ClientSessionConfig<C>,
        rate_controller: Arc<R>,
        metrics: M,
    ) -> Self {
        // Implementation details...
    }

    /// Create a new session
    fn create_session(&self, client_type: C) -> ClientSession<C> {
        // Implementation details...
    }

    /// Generate requests for a session
    async fn generate_session_requests(
        &self,
        session: &mut ClientSession<C>,
        ticket_tx: &async_channel::Sender<SchedulerTicket>,
    ) -> Result<(), Error> {
        // Implementation details...
    }

    /// Update session based on control signals
    fn update_session_control(&self) {
        // Implementation details...
    }

    /// Process a rate change event
    fn process_rate_change_event(&self, event: &RateChangeEvent) {
        // Implementation details...
    }
}

impl<M, C, R> ClientSessionManager<M, C> for StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    async fn run(
        &self,
        config: Arc<LoadTestConfig>,
        rate_controller: Arc<impl RateController<_, _, _>>,
        ticket_tx: async_channel::Sender<SchedulerTicket>,
    ) {
        // Set running flag
        self.running.store(true, Ordering::SeqCst);

        // Session creation task
        let session_creator = {
            let manager = self.clone();
            let running = self.running.clone();
            let active_sessions = self.active_sessions.clone();
            let ticket_tx = ticket_tx.clone();

            tokio::spawn(async move {
                // Create new sessions at configured rate
                let mut interval = tokio::time::interval(Duration::from_millis(100));

                while running.load(Ordering::SeqCst) {
                    interval.tick().await;

                    // Update from rate controller
                    manager.update_session_control();

                    // Calculate how many sessions to create in this interval
                    let creation_rate = manager.current_creation_rate.load(Ordering::Relaxed);
                    let sessions_to_create = (creation_rate / 10.0).round() as usize; // 10 intervals per second

                    // Create sessions
                    for _ in 0..sessions_to_create {
                        // Select client type based on distribution
                        let client_type = manager.select_client_type();

                        // Create session
                        let session = manager.create_session(client_type);

                        // Store session
                        active_sessions.insert(session.id.clone(), session);
                    }
                }
            })
        };

        // Session activity task
        let session_activity = {
            let manager = self.clone();
            let running = self.running.clone();
            let active_sessions = self.active_sessions.clone();

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(50));

                while running.load(Ordering::SeqCst) {
                    interval.tick().await;

                    // Process active sessions
                    for mut entry in active_sessions.iter_mut() {
                        let session_id = entry.key().clone();
                        let session = entry.value_mut();

                        // Skip sessions that aren't active
                        if session.state != SessionState::Active {
                            continue;
                        }

                        // Check if session has expired
                        if session.start_time.elapsed() > session.expected_duration {
                            // Mark session as closing
                            session.state = SessionState::Closing;
                            continue;
                        }

                        // Generate requests for this session
                        if let Err(e) = manager.generate_session_requests(session, &ticket_tx).await {
                            // Handle error (e.g., log, mark session as closing)
                            session.state = SessionState::Closing;
                        }
                    }

                    // Clean up closed sessions
                    let mut to_remove = Vec::new();
                    for entry in active_sessions.iter() {
                        if entry.value().state == SessionState::Closed {
                            to_remove.push(entry.key().clone());
                        }
                    }

                    for id in to_remove {
                        active_sessions.remove(&id);
                    }
                }
            })
        };

        // Wait for tasks to complete
        let _ = tokio::join!(session_creator, session_activity);
    }

    fn update_client_distribution(&self, distribution: ClientDistribution<C>) {
        // Update configuration
        if let Ok(mut config) = Arc::get_mut(&mut self.config.clone()) {
            config.distribution = distribution.distribution.clone();
            config.session_creation_rate = distribution.session_creation_rate;
            config.avg_session_duration_secs = distribution.avg_session_duration_secs;
            config.session_duration_variance = distribution.session_duration_variance;
            config.think_time_distribution = distribution.think_time_distribution.clone();
        }

        // Update current creation rate
        self.current_creation_rate.store(distribution.session_creation_rate, Ordering::Relaxed);

        // Update samplers
        for (client_type, _) in &distribution.distribution {
            // Create think time sampler
            let think_time_sampler = create_think_time_sampler(&distribution.think_time_distribution);
            self.think_time_samplers.insert(client_type.clone(), think_time_sampler);

            // Create duration sampler
            let duration_sampler = create_duration_sampler(
                distribution.avg_session_duration_secs,
                distribution.session_duration_variance,
            );
            self.duration_samplers.insert(client_type.clone(), duration_sampler);
        }
    }

    fn get_session_stats(&self) -> ClientSessionStats<C> {
        // Calculate session statistics
        let mut active_count = 0;
        let mut distribution = HashMap::new();
        let mut total_duration_secs = 0.0;
        let mut total_requests = 0;
        let mut think_times = Vec::new();

        for entry in self.active_sessions.iter() {
            let session = entry.value();

            // Count active sessions
            if session.state == SessionState::Active {
                active_count += 1;

                // Update type distribution
                *distribution.entry(session.client_type.clone()).or_insert(0) += 1;

                // Accumulate duration and requests
                total_duration_secs += session.start_time.elapsed().as_secs_f64();
                total_requests += session.stats.requests_generated;

                // Record think time if available
                if session.stats.requests_generated > 0 {
                    think_times.push(session.stats.avg_think_time_ms);
                }
            }
        }

        // Calculate think time statistics
        let think_time_stats = if !think_times.is_empty() {
            think_times.sort_unstable();

            let median_idx = think_times.len() / 2;
            let p95_idx = (think_times.len() as f64 * 0.95) as usize;

            ThinkTimeStats {
                median_ms: think_times.get(median_idx).copied().unwrap_or(0),
                p95_ms: think_times.get(p95_idx).copied().unwrap_or(0),
                min_ms: think_times.first().copied().unwrap_or(0),
                max_ms: think_times.last().copied().unwrap_or(0),
            }
        } else {
            ThinkTimeStats {
                median_ms: 0,
                p95_ms: 0,
                min_ms: 0,
                max_ms: 0,
            }
        };

        // Calculate averages
        let avg_duration = if active_count > 0 {
            total_duration_secs / active_count as f64
        } else {
            0.0
        };

        let avg_requests = if active_count > 0 {
            total_requests as f64 / active_count as f64
        } else {
            0.0
        };

        ClientSessionStats {
            active_sessions: active_count,
            total_sessions_created: self.metrics.get_total_sessions_created(),
            sessions_closed: self.metrics.get_sessions_closed(),
            distribution,
            avg_session_duration_secs: avg_duration,
            avg_requests_per_session: avg_requests,
            current_creation_rate: self.current_creation_rate.load(Ordering::Relaxed),
            think_time_stats,
        }
    }

    fn terminate(&self) {
        // Set running flag to false
        self.running.store(false, Ordering::SeqCst);
    }

    fn metrics(&self) -> &M {
        &self.metrics
    }
}
```

### 3.2 Think Time and Duration Sampling

```rust
/// Interface for think time sampling
pub trait ThinkTimeSampler: Send + Sync {
    /// Sample a think time value
    fn sample(&self, rng: &mut SmallRng) -> u64;
}

/// Log-normal distribution think time sampler
pub struct LogNormalThinkTimeSampler {
    /// Mean of the underlying normal distribution
    ln_mean: f64,

    /// Standard deviation of the underlying normal distribution
    ln_std: f64,
}

impl LogNormalThinkTimeSampler {
    /// Create a new log-normal think time sampler
    pub fn new(mean_ms: f64, std_dev_ms: f64) -> Self {
        // Convert mean and std dev to log-normal parameters
        let variance = std_dev_ms * std_dev_ms;
        let ln_mean = (mean_ms * mean_ms) / f64::sqrt(mean_ms * mean_ms + variance);
        let ln_mean = ln_mean.ln();
        let ln_std = f64::sqrt((mean_ms * mean_ms + variance) / (mean_ms * mean_ms)).ln();

        Self { ln_mean, ln_std }
    }
}

impl ThinkTimeSampler for LogNormalThinkTimeSampler {
    fn sample(&self, rng: &mut SmallRng) -> u64 {
        // Generate a normally distributed random value
        let normal = rand_distr::Normal::new(self.ln_mean, self.ln_std).unwrap();
        let ln_value = normal.sample(rng);

        // Convert to log-normal by taking e^value
        let value = ln_value.exp();

        // Convert to u64
        value.round() as u64
    }
}

/// Factory function to create think time sampler from distribution
fn create_think_time_sampler(distribution: &ThinkTimeDistribution) -> Box<dyn ThinkTimeSampler> {
    match distribution {
        ThinkTimeDistribution::Fixed(ms) => {
            Box::new(FixedThinkTimeSampler::new(*ms))
        },
        ThinkTimeDistribution::Uniform { min_ms, max_ms } => {
            Box::new(UniformThinkTimeSampler::new(*min_ms, *max_ms))
        },
        ThinkTimeDistribution::Normal { mean_ms, std_dev_ms } => {
            Box::new(NormalThinkTimeSampler::new(*mean_ms, *std_dev_ms))
        },
        ThinkTimeDistribution::LogNormal { mean_ms, std_dev_ms } => {
            Box::new(LogNormalThinkTimeSampler::new(*mean_ms, *std_dev_ms))
        },
        ThinkTimeDistribution::Exponential { mean_ms } => {
            Box::new(ExponentialThinkTimeSampler::new(*mean_ms))
        },
        ThinkTimeDistribution::Custom(_) => {
            // Default to log-normal if custom not implemented
            Box::new(LogNormalThinkTimeSampler::new(2000.0, 1000.0))
        },
    }
}

/// Interface for session duration sampling
pub trait DurationSampler: Send + Sync {
    /// Sample a session duration in seconds
    fn sample(&self, rng: &mut SmallRng) -> f64;
}

/// Factory function to create duration sampler
fn create_duration_sampler(
    mean_secs: f64,
    coefficient_of_variation: f64,
) -> Box<dyn DurationSampler> {
    // Standard deviation = mean * coefficient_of_variation
    let std_dev = mean_secs * coefficient_of_variation;

    // Use log-normal distribution for session duration
    Box::new(LogNormalDurationSampler::new(mean_secs, std_dev))
}
```

### 3.3 Navigation Pattern Selection

```rust
/// Interface for pattern selection
pub trait PatternSelector<C: ClientType>: Send + Sync {
    /// Select a navigation pattern for a client type
    fn select_pattern(&self, client_type: &C, rng: &mut SmallRng) -> Option<NavigationPattern>;
}

/// Default implementation of pattern selector
pub struct DefaultPatternSelector<C: ClientType> {
    /// Patterns by client type
    patterns: HashMap<C, Vec<NavigationPattern>>,

    /// Probabilities by client type
    probabilities: HashMap<C, Vec<f64>>,
}

impl<C: ClientType> PatternSelector<C> for DefaultPatternSelector<C> {
    fn select_pattern(&self, client_type: &C, rng: &mut SmallRng) -> Option<NavigationPattern> {
        // Get patterns for this client type
        let patterns = self.patterns.get(client_type)?;
        let probabilities = self.probabilities.get(client_type)?;

        if patterns.is_empty() || probabilities.is_empty() {
            return None;
        }

        // Generate random value
        let value = rng.gen::<f64>();

        // Find pattern based on cumulative probability
        let mut cumulative = 0.0;
        for (idx, &prob) in probabilities.iter().enumerate() {
            cumulative += prob;
            if value <= cumulative {
                return Some(patterns[idx].clone());
            }
        }

        // Default to first pattern
        patterns.first().cloned()
    }
}
```

### 3.4 Lock-Free Metrics Collection

```rust
/// Lock-free metrics for client session manager
pub struct ClientSessionManagerMetrics {
    /// Component ID
    id: String,

    /// Total sessions created
    total_sessions_created: AtomicU64,

    /// Active sessions
    active_sessions: AtomicU64,

    /// Sessions closed
    sessions_closed: AtomicU64,

    /// Current session creation rate
    current_creation_rate: AtomicF64,

    /// Total requests generated
    total_requests_generated: AtomicU64,

    /// Current rate control signal
    current_rate_signal: AtomicU64,

    /// Average think time (ms)
    avg_think_time_ms: AtomicF64,

    /// Average session duration (sec)
    avg_session_duration_sec: AtomicF64,

    /// Client type distribution
    /// Protected by RwLock but updated infrequently
    client_distribution: Arc<RwLock<HashMap<String, u64>>>,

    /// Last metrics update timestamp
    last_update: AtomicU64,
}

impl ClientSessionManagerMetrics {
    /// Create new metrics collection
    pub fn new(id: String) -> Self {
        Self {
            id,
            total_sessions_created: AtomicU64::new(0),
            active_sessions: AtomicU64::new(0),
            sessions_closed: AtomicU64::new(0),
            current_creation_rate: AtomicF64::new(0.0),
            total_requests_generated: AtomicU64::new(0),
            current_rate_signal: AtomicU64::new(0),
            avg_think_time_ms: AtomicF64::new(0.0),
            avg_session_duration_sec: AtomicF64::new(0.0),
            client_distribution: Arc::new(RwLock::new(HashMap::new())),
            last_update: AtomicU64::new(current_timestamp()),
        }
    }

    /// Record session creation
    pub fn record_session_created(&self, client_type: &str) {
        self.total_sessions_created.fetch_add(1, Ordering::Relaxed);
        self.active_sessions.fetch_add(1, Ordering::Relaxed);

        // Update client type distribution
        if let Ok(mut dist) = self.client_distribution.write() {
            *dist.entry(client_type.to_string()).or_insert(0) += 1;
        }

        self.update_timestamp();
    }

    /// Record session closed
    pub fn record_session_closed(&self, client_type: &str, duration_sec: f64) {
        self.sessions_closed.fetch_add(1, Ordering::Relaxed);
        self.active_sessions.fetch_sub(1, Ordering::Relaxed);

        // Update client type distribution
        if let Ok(mut dist) = self.client_distribution.write() {
            if let Some(count) = dist.get_mut(client_type) {
                if *count > 0 {
                    *count -= 1;
                }
            }
        }

        // Update average session duration using moving average
        let current_avg = self.avg_session_duration_sec.load(Ordering::Relaxed);
        let closed_count = self.sessions_closed.load(Ordering::Relaxed);

        if closed_count > 0 {
            let new_avg = current_avg + (duration_sec - current_avg) / closed_count as f64;
            self.avg_session_duration_sec.store(new_avg, Ordering::Relaxed);
        }

        self.update_timestamp();
    }

    /// Record request generation
    pub fn record_request_generated(&self, think_time_ms: u64) {
        self.total_requests_generated.fetch_add(1, Ordering::Relaxed);

        // Update average think time using moving average
        let current_avg = self.avg_think_time_ms.load(Ordering::Relaxed);
        let total_requests = self.total_requests_generated.load(Ordering::Relaxed);

        if total_requests > 0 {
            let new_avg = current_avg + (think_time_ms as f64 - current_avg) / total_requests as f64;
            self.avg_think_time_ms.store(new_avg, Ordering::Relaxed);
        }

        self.update_timestamp();
    }

    /// Update creation rate
    pub fn update_creation_rate(&self, rate: f64) {
        self.current_creation_rate.store(rate, Ordering::Relaxed);
        self.update_timestamp();
    }

    /// Update rate control signal
    pub fn update_rate_signal(&self, signal: u64) {
        self.current_rate_signal.store(signal, Ordering::Relaxed);
        self.update_timestamp();
    }

    /// Get total sessions created
    pub fn get_total_sessions_created(&self) -> u64 {
        self.total_sessions_created.load(Ordering::Relaxed)
    }

    /// Get active sessions count
    pub fn get_active_sessions(&self) -> u64 {
        self.active_sessions.load(Ordering::Relaxed)
    }

    /// Get sessions closed count
    pub fn get_sessions_closed(&self) -> u64 {
        self.sessions_closed.load(Ordering::Relaxed)
    }

    /// Update the last timestamp
    fn update_timestamp(&self) {
        let now = current_timestamp();
        self.last_update.store(now, Ordering::Relaxed);
    }

    /// Get a snapshot of metrics for registry
    pub fn get_snapshot(&self) -> HashMap<String, MetricValue> {
        let mut metrics = HashMap::new();

        // Add atomic counter values
        metrics.insert("total_sessions_created".to_string(),
                      MetricValue::Counter(self.total_sessions_created.load(Ordering::Relaxed)));

        metrics.insert("active_sessions".to_string(),
                      MetricValue::Gauge(self.active_sessions.load(Ordering::Relaxed) as i64));

        metrics.insert("sessions_closed".to_string(),
                      MetricValue::Counter(self.sessions_closed.load(Ordering::Relaxed)));

        metrics.insert("current_creation_rate".to_string(),
                      MetricValue::Float(self.current_creation_rate.load(Ordering::Relaxed)));

        metrics.insert("total_requests_generated".to_string(),
                      MetricValue::Counter(self.total_requests_generated.load(Ordering::Relaxed)));

        metrics.insert("current_rate_signal".to_string(),
                      MetricValue::Gauge(self.current_rate_signal.load(Ordering::Relaxed) as i64));

        metrics.insert("avg_think_time_ms".to_string(),
                      MetricValue::Float(self.avg_think_time_ms.load(Ordering::Relaxed)));

        metrics.insert("avg_session_duration_sec".to_string(),
                      MetricValue::Float(self.avg_session_duration_sec.load(Ordering::Relaxed)));

        // Add client type distribution
        if let Ok(dist) = self.client_distribution.read() {
            let dist_str = dist.iter()
                .map(|(k, v)| format!("{}:{}", k, v))
                .collect::<Vec<_>>()
                .join(",");

            metrics.insert("client_distribution".to_string(),
                          MetricValue::Text(dist_str));
        }

        metrics
    }
}

impl MetricsProvider for ClientSessionManagerMetrics {
    fn get_metrics(&self) -> ComponentMetrics {
        ComponentMetrics {
            component_type: "ClientSessionManager".to_string(),
            component_id: self.id.clone(),
            timestamp: current_timestamp(),
            metrics: self.get_snapshot(),
            status: Some(format!(
                "Active: {}, Creation Rate: {:.2}/sec, Avg Think Time: {:.2}ms",
                self.active_sessions.load(Ordering::Relaxed),
                self.current_creation_rate.load(Ordering::Relaxed),
                self.avg_think_time_ms.load(Ordering::Relaxed)
            )),
        }
    }

    fn get_component_type(&self) -> &str {
        "ClientSessionManager"
    }

    fn get_component_id(&self) -> &str {
        &self.id
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

## 4. Integration with Other Components

### 4.1 Integration with Rate Controller

```rust
impl<M, C, R> StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Update session parameters based on rate controller signals
    fn update_from_rate_controller(&self) {
        // Get control signals from rate controller
        let signals = self.rate_controller.get_session_control_signals();

        // Update metrics
        self.metrics.update_rate_signal(signals.target_rps);

        // Update session creation rate if provided
        if let Some(creation_rate) = signals.session_creation_rate {
            self.current_creation_rate.store(creation_rate, Ordering::Relaxed);
            self.metrics.update_creation_rate(creation_rate);
        }

        // Process rate change event if present
        if let Some(event) = signals.rate_change_event {
            self.process_rate_change_event(&event);
        }

        // Update client type distribution if provided
        if let Some(adjustments) = signals.session_type_adjustments {
            // Apply adjustments to current distribution
            if let Ok(mut config) = Arc::get_mut(&mut self.config.clone()) {
                for (client_type, adjustment) in adjustments {
                    if let Some(current) = config.distribution.get_mut(&client_type) {
                        *current = adjustment;
                    }
                }
            }
        }

        // Record last update time
        self.last_control_update.store(current_timestamp(), Ordering::Relaxed);
    }

    /// Process a rate change event from the rate controller
    fn process_rate_change_event(&self, event: &RateChangeEvent) {
        match &event.session_impact {
            SessionImpact::AddSessions { count, priority_types } => {
                // Create additional sessions
                for _ in 0..*count {
                    // Select client type, prioritizing specified types
                    let client_type = if !priority_types.is_empty() {
                        self.select_priority_client_type(priority_types)
                    } else {
                        self.select_client_type()
                    };

                    // Create session
                    let session = self.create_session(client_type);

                    // Store session
                    self.active_sessions.insert(session.id.clone(), session);
                }
            },
            SessionImpact::RemoveSessions { count, strategy } => {
                // Find sessions to remove
                let sessions_to_remove = self.select_sessions_for_removal(*count, strategy);

                // Mark sessions as closing
                for session_id in sessions_to_remove {
                    if let Some(mut entry) = self.active_sessions.get_mut(&session_id) {
                        entry.state = SessionState::Closing;
                    }
                }
            },
            SessionImpact::ModifyActivityLevel { multiplier } => {
                // Adjust think time in all active sessions
                for mut entry in self.active_sessions.iter_mut() {
                    let session = entry.value_mut();

                    if session.state == SessionState::Active {
                        // Adjust think time samplers
                        if let Some(sampler) = self.think_time_samplers.get_mut(&session.client_type) {
                            // Scale the sampler (implementation-specific)
                            if let Some(adjustable) = sampler.value_mut()
                                .as_any_mut()
                                .downcast_mut::<AdjustableThinkTimeSampler>() {
                                adjustable.adjust_multiplier(*multiplier);
                            }
                        }
                    }
                }
            },
            SessionImpact::GradualChange { duration_ms, easing } => {
                // Schedule gradual adjustment over time
                // Implementation depends on easing function
                let start_time = Instant::now();
                let end_time = start_time + Duration::from_millis(*duration_ms);
                let start_rps = event.old_rps;
                let end_rps = event.new_rps;

                // Store transition parameters for use in request generation
                // Implementation-specific
            },
        }
    }

    /// Select sessions for removal based on strategy
    fn select_sessions_for_removal(
        &self,
        count: u64,
        strategy: &SessionRemovalStrategy,
    ) -> Vec<String> {
        // Get active sessions
        let active = self.active_sessions.iter()
            .filter(|e| e.value().state == SessionState::Active)
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect::<Vec<_>>();

        if active.is_empty() {
            return Vec::new();
        }

        // Sort or filter based on strategy
        let mut sessions = match strategy {
            SessionRemovalStrategy::OldestFirst => {
                // Sort by start time (oldest first)
                let mut sorted = active;
                sorted.sort_by(|a, b| a.1.start_time.cmp(&b.1.start_time));
                sorted
            },
            SessionRemovalStrategy::NewestFirst => {
                // Sort by start time (newest first)
                let mut sorted = active;
                sorted.sort_by(|a, b| b.1.start_time.cmp(&a.1.start_time));
                sorted
            },
            SessionRemovalStrategy::Random => {
                // Shuffle
                let mut shuffled = active;
                shuffled.shuffle(&mut rand::thread_rng());
                shuffled
            },
            SessionRemovalStrategy::LeastActiveFirst => {
                // Sort by request count (least first)
                let mut sorted = active;
                sorted.sort_by_key(|(_, s)| s.stats.requests_generated);
                sorted
            },
        };

        // Take the required number of sessions
        sessions.truncate(count as usize);

        // Return session IDs
        sessions.into_iter().map(|(id, _)| id).collect()
    }
}
```

### 4.2 Integration with Request Scheduler

```rust
impl<M, C, R> StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Generate a scheduler ticket for a request
    fn generate_ticket(
        &self,
        session: &ClientSession<C>,
        scheduled_time: Instant,
        is_warmup: bool,
    ) -> SchedulerTicket {
        let id = self.next_request_id();

        // Create protocol-specific information
        let protocol_info = match session.client_type.connection_params().use_http2 {
            true => Some(ProtocolInfo::Http2 {
                stream_id: None, // Will be assigned by executor
            }),
            false => Some(ProtocolInfo::Http1 {
                connection_id: None, // Will be assigned by executor
            }),
        };

        SchedulerTicket {
            id,
            scheduled_time,
            is_warmup,
            should_sample: false, // Sampling decision made by scheduler
            client_session_id: Some(session.id.clone()),
            protocol_info,
        }
    }

    /// Send requests to scheduler
    async fn send_session_requests(
        &self,
        session: &mut ClientSession<C>,
        ticket_tx: &async_channel::Sender<SchedulerTicket>,
    ) -> Result<(), Error> {
        // Check if it's time to generate a request
        let elapsed = session.stats.last_activity.elapsed();

        // Get think time sampler for this client type
        let think_time_sampler = self.think_time_samplers.get(&session.client_type)
            .ok_or_else(|| Error::MissingSampler)?;

        // Sample think time
        let mut rng = self.rng.lock().unwrap();
        let think_time_ms = think_time_sampler.sample(&mut rng);
        drop(rng); // Release lock

        // Check if think time has elapsed
        if elapsed < Duration::from_millis(think_time_ms) {
            // Not yet time for next request
            return Ok(());
        }

        // Update last activity time
        session.stats.last_activity = Instant::now();

        // Check if we're executing a navigation pattern
        if let Some(pattern) = &session.navigation_context.current_pattern {
            // Get current step in pattern
            let step = session.navigation_context.pattern_step;

            if step < pattern.sequence.len() {
                // Generate request based on pattern step
                let request_type = &pattern.sequence[step];

                // Schedule time for this request
                let scheduled_time = Instant::now();

                // Create ticket
                let ticket = self.generate_ticket(session, scheduled_time, false);

                // Send ticket to scheduler
                ticket_tx.send(ticket).await
                    .map_err(|_| Error::ChannelClosed)?;

                // Update statistics
                session.stats.requests_generated += 1;
                session.stats.total_think_time_ms += think_time_ms;

                // Update average think time
                if session.stats.requests_generated > 0 {
                    session.stats.avg_think_time_ms = session.stats.total_think_time_ms as f64
                        / session.stats.requests_generated as f64;
                }

                // Record metrics
                self.metrics.record_request_generated(think_time_ms);

                // Move to next step
                session.navigation_context.pattern_step += 1;

                // If pattern complete, reset
                if session.navigation_context.pattern_step >= pattern.sequence.len() {
                    session.navigation_context.current_pattern = None;
                    session.navigation_context.pattern_step = 0;
                }
            } else {
                // Reset pattern
                session.navigation_context.current_pattern = None;
                session.navigation_context.pattern_step = 0;
            }
        } else {
            // No active pattern, select a new one
            let mut rng = self.rng.lock().unwrap();

            // Try to select a pattern
            let pattern_selector = self.pattern_selectors.get(&session.client_type);
            let new_pattern = if let Some(selector) = pattern_selector {
                selector.select_pattern(&session.client_type, &mut rng)
            } else {
                None
            };

            // If pattern selected, set it
            if let Some(pattern) = new_pattern {
                session.navigation_context.current_pattern = Some(pattern);
                session.navigation_context.pattern_step = 0;
            } else {
                // No pattern, generate a simple request
                // Schedule time for this request
                let scheduled_time = Instant::now();

                // Create ticket
                let ticket = self.generate_ticket(session, scheduled_time, false);

                // Send ticket to scheduler
                ticket_tx.send(ticket).await
                    .map_err(|_| Error::ChannelClosed)?;

                // Update statistics
                session.stats.requests_generated += 1;
                session.stats.total_think_time_ms += think_time_ms;

                // Update average think time
                if session.stats.requests_generated > 0 {
                    session.stats.avg_think_time_ms = session.stats.total_think_time_ms as f64
                        / session.stats.requests_generated as f64;
                }

                // Record metrics
                self.metrics.record_request_generated(think_time_ms);
            }
        }

        Ok(())
    }
}
```

### 4.3 Request Context Generation

```rust
impl<M, C, R> StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Generate request context for a ticket
    pub fn generate_request_context(&self, ticket: &SchedulerTicket) -> Option<RequestContext> {
        // Get session ID from ticket
        let session_id = ticket.client_session_id.as_ref()?;

        // Look up session
        let session = self.active_sessions.get(session_id)?;

        // Create client session info
        let session_info = ClientSessionInfo {
            session_id: session.id.clone(),
            client_type: session.client_type.clone(),
            start_time: session.start_time,
            request_count: session.stats.requests_generated,
            client_ip: session.client_ip,
            network_condition: None, // Will be filled by network simulator
        };

        // Create parameters based on navigation context
        let mut parameters = HashMap::new();

        // Add current path
        parameters.insert("current_path".to_string(), session.navigation_context.current_path.clone());

        // Add session data
        for (key, value) in &session.session_data {
            parameters.insert(format!("session_{}", key), value.clone());
        }

        // Create request context
        Some(RequestContext {
            request_id: ticket.id,
            ticket: ticket.clone(),
            timestamp: Instant::now(),
            is_sampled: ticket.should_sample,
            session_info: Some(session_info),
            parameters,
        })
    }
}
```

## 5. Factory and Builder Pattern

```rust
/// Factory for creating client session managers
pub struct ClientSessionManagerFactory;

impl ClientSessionManagerFactory {
    /// Create a client session manager based on configuration
    pub fn create<M, C, R>(
        config: &LoadTestConfig,
        session_config: ClientSessionConfig<C>,
        rate_controller: Arc<R>,
        metrics: M,
    ) -> impl ClientSessionManager<M, C>
    where
        M: MetricsProvider,
        C: ClientType,
        R: RateController<_, _, _>,
    {
        // Generate a unique ID for this manager
        let id = format!("session-manager-{}", Uuid::new_v4());

        // Create client session manager
        StandardClientSessionManager::new(
            id,
            session_config,
            rate_controller,
            metrics,
        )
    }

    /// Create a client session manager with default client types
    pub fn create_standard<M, R>(
        config: &LoadTestConfig,
        session_config: ClientSessionConfig<DefaultClientType>,
        rate_controller: Arc<R>,
        metrics: M,
    ) -> impl ClientSessionManager<M, DefaultClientType>
    where
        M: MetricsProvider,
        R: RateController<_, _, _>,
    {
        Self::create(
            config,
            session_config,
            rate_controller,
            metrics,
        )
    }
}

impl LoadTestBuilder {
    /// Configure client session manager
    pub fn with_client_session_config(
        mut self,
        config: ClientSessionConfig<DefaultClientType>,
    ) -> Self {
        self.client_session_config = Some(config);
        self
    }

    /// Use a specific think time distribution
    pub fn with_think_time_distribution(
        mut self,
        distribution: ThinkTimeDistribution,
    ) -> Self {
        if let Some(ref mut config) = self.client_session_config {
            config.think_time_distribution = distribution;
        } else {
            let mut config = ClientSessionConfig::default();
            config.think_time_distribution = distribution;
            self.client_session_config = Some(config);
        }
        self
    }

    /// Use a specific client type distribution
    pub fn with_client_distribution(
        mut self,
        distribution: HashMap<DefaultClientType, f64>,
    ) -> Self {
        if let Some(ref mut config) = self.client_session_config {
            config.distribution = distribution;
        } else {
            let mut config = ClientSessionConfig::default();
            config.distribution = distribution;
            self.client_session_config = Some(config);
        }
        self
    }

    /// Build the client session manager with static dispatch
    fn build_session_manager<R: RateController<_, _, _>>(
        &self,
        rate_controller: Arc<R>,
    ) -> impl ClientSessionManager<DefaultMetricsProvider, DefaultClientType> {
        // Create metrics provider
        let metrics = DefaultMetricsProvider::new(
            format!("session-manager-{}", Uuid::new_v4())
        );

        // Get or create session config
        let session_config = self.client_session_config.clone()
            .unwrap_or_else(|| ClientSessionConfig::default());

        // Create session manager
        ClientSessionManagerFactory::create_standard(
            &self.config,
            session_config,
            rate_controller,
            metrics,
        )
    }
}
```

## 6. Statistical Models for Realistic Client Behavior

### 6.1 Think Time Distribution

```rust
/// Create think time sampler from distribution
fn create_think_time_sampler(distribution: &ThinkTimeDistribution) -> Box<dyn ThinkTimeSampler> {
    match distribution {
        ThinkTimeDistribution::Fixed(ms) => {
            Box::new(FixedThinkTimeSampler::new(*ms))
        },
        ThinkTimeDistribution::Uniform { min_ms, max_ms } => {
            Box::new(UniformThinkTimeSampler::new(*min_ms, *max_ms))
        },
        ThinkTimeDistribution::Normal { mean_ms, std_dev_ms } => {
            Box::new(NormalThinkTimeSampler::new(*mean_ms, *std_dev_ms))
        },
        ThinkTimeDistribution::LogNormal { mean_ms, std_dev_ms } => {
            Box::new(LogNormalThinkTimeSampler::new(*mean_ms, *std_dev_ms))
        },
        ThinkTimeDistribution::Exponential { mean_ms } => {
            Box::new(ExponentialThinkTimeSampler::new(*mean_ms))
        },
        ThinkTimeDistribution::Custom(_) => {
            // Default to log-normal if custom not implemented
            Box::new(LogNormalThinkTimeSampler::new(2000.0, 1000.0))
        },
    }
}
```

### 6.2 Session Duration Distribution

```rust
/// LogNormal distribution for session duration
pub struct LogNormalDurationSampler {
    /// Mean of the underlying normal distribution
    ln_mean: f64,

    /// Standard deviation of the underlying normal distribution
    ln_std: f64,
}

impl LogNormalDurationSampler {
    /// Create a new log-normal duration sampler
    pub fn new(mean_secs: f64, std_dev_secs: f64) -> Self {
        // Convert mean and std dev to log-normal parameters
        let variance = std_dev_secs * std_dev_secs;
        let ln_mean = (mean_secs * mean_secs) / f64::sqrt(mean_secs * mean_secs + variance);
        let ln_mean = ln_mean.ln();
        let ln_std = f64::sqrt((mean_secs * mean_secs + variance) / (mean_secs * mean_secs)).ln();

        Self { ln_mean, ln_std }
    }
}

impl DurationSampler for LogNormalDurationSampler {
    fn sample(&self, rng: &mut SmallRng) -> f64 {
        // Generate a normally distributed random value
        let normal = rand_distr::Normal::new(self.ln_mean, self.ln_std).unwrap();
        let ln_value = normal.sample(rng);

        // Convert to log-normal by taking e^value
        ln_value.exp()
    }
}
```

### 6.3 Session Creation Rate

```rust
impl<M, C, R> StandardClientSessionManager<M, C, R>
where
    M: MetricsProvider,
    C: ClientType,
    R: RateController<_, _, _>,
{
    /// Calculate session creation rate from RPS target
    fn calculate_session_creation_rate(&self, target_rps: u64) -> f64 {
        // Get average requests per session from metrics
        let avg_requests_per_session = self.get_avg_requests_per_session();

        if avg_requests_per_session > 0.0 {
            // Target sessions = Target RPS / Requests per session
            // But we need to account for session lifecycle

            // Get average session duration in seconds
            let avg_duration_sec = self.get_avg_session_duration_sec();

            if avg_duration_sec > 0.0 {
                // Need to create enough sessions to maintain target RPS
                // Creation rate = (Target RPS / Requests per session) / Avg Duration
                (target_rps as f64 / avg_requests_per_session) / avg_duration_sec
            } else {
                // Default to 1% of target RPS if no duration data
                target_rps as f64 * 0.01
            }
        } else {
            // Default to 1% of target RPS if no request data
            target_rps as f64 * 0.01
        }
    }

    /// Get average session duration
    fn get_avg_session_duration_sec(&self) -> f64 {
        // Get from metrics if available
        self.metrics.get_avg_session_duration_sec()
    }

    /// Get average requests per session
    fn get_avg_requests_per_session(&self) -> f64 {
        // Calculate from current sessions
        let mut total_sessions = 0;
        let mut total_requests = 0;

        for entry in self.active_sessions.iter() {
            let session = entry.value();

            if session.state == SessionState::Active {
                total_sessions += 1;
                total_requests += session.stats.requests_generated as usize;
            }
        }

        if total_sessions > 0 {
            total_requests as f64 / total_sessions as f64
        } else {
            // Default to configuration if no active sessions
            self.config.avg_requests_per_session
        }
    }
}
```

## 7. Session Correlations and State Management

```rust
/// Correlations for requests within a session
pub struct SessionCorrelation {
    /// Session ID
    pub session_id: String,

    /// Cookies by domain
    pub cookies: HashMap<String, Vec<Cookie>>,

    /// Authentication tokens
    pub auth_tokens: HashMap<String, String>,

    /// Extracted form values
    pub form_values: HashMap<String, String>,

    /// Navigation history
    pub navigation_history: VecDeque<String>,

    /// Resource cache status
    pub cached_resources: HashSet<String>,
}

impl SessionCorrelation {
    /// Create a new session correlation
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            cookies: HashMap::new(),
            auth_tokens: HashMap::new(),
            form_values: HashMap::new(),
            navigation_history: VecDeque::with_capacity(10),
            cached_resources: HashSet::new(),
        }
    }

    /// Add a cookie from response
    pub fn add_cookie(&mut self, domain: &str, cookie: Cookie) {
        let cookies = self.cookies.entry(domain.to_string()).or_default();

        // Check if cookie already exists
        if let Some(idx) = cookies.iter().position(|c| c.name == cookie.name) {
            // Replace existing cookie
            cookies[idx] = cookie;
        } else {
            // Add new cookie
            cookies.push(cookie);
        }
    }

    /// Get cookies for a domain
    pub fn get_cookies(&self, domain: &str) -> Vec<Cookie> {
        self.cookies.get(domain)
            .map(|cookies| cookies.clone())
            .unwrap_or_default()
    }

    /// Update from response
    pub fn update_from_response(
        &mut self,
        request_url: &str,
        response: &CompletedRequest,
    ) {
        // Extract domain from URL
        let domain = extract_domain(request_url).unwrap_or_default();

        // Process cookies from response
        if let Some(cookie_header) = response.headers.get("set-cookie") {
            // Parse cookies
            for cookie_str in cookie_header.split(';') {
                if let Some(cookie) = parse_cookie(cookie_str) {
                    self.add_cookie(&domain, cookie);
                }
            }
        }

        // Process auth tokens
        if let Some(auth_header) = response.headers.get("authorization") {
            if auth_header.starts_with("Bearer ") {
                self.auth_tokens.insert("Bearer".to_string(), auth_header[7..].to_string());
            } else if auth_header.starts_with("Basic ") {
                self.auth_tokens.insert("Basic".to_string(), auth_header[6..].to_string());
            }
        }

        // Update navigation history
        self.navigation_history.push_back(request_url.to_string());

        // Keep limited history
        while self.navigation_history.len() > 10 {
            self.navigation_history.pop_front();
        }

        // Extract values from response body (if sampled)
        if let Some(body) = &response.body {
            self.extract_values_from_body(body);
        }
    }
}
```

## 8. Configuration and Integration Examples

### 8.1 Complete Configuration Example

```rust
// Create a client session manager with realistic behavior
fn create_realistic_session_manager<R>(
    rate_controller: Arc<R>,
) -> impl ClientSessionManager<DefaultMetricsProvider, DefaultClientType>
where
    R: RateController<_, _, _>,
{
    // Define client distribution
    let mut distribution = HashMap::new();

    // Add browser client types (Chrome, Firefox, Edge)
    distribution.insert(
        DefaultClientType::Browser {
            browser_type: BrowserType::Chrome,
            http2_enabled: true,
            connection_limit: 6,
        },
        0.6, // 60% Chrome browsers
    );

    distribution.insert(
        DefaultClientType::Browser {
            browser_type: BrowserType::Firefox,
            http2_enabled: true,
            connection_limit: 8,
        },
        0.15, // 15% Firefox browsers
    );

    distribution.insert(
        DefaultClientType::Browser {
            browser_type: BrowserType::Edge,
            http2_enabled: true,
            connection_limit: 8,
        },
        0.05, // 5% Edge browsers
    );

    // Add mobile client types
    distribution.insert(
        DefaultClientType::Mobile {
            platform: MobilePlatform::iOS,
            network_type: NetworkType::WiFi,
            http2_enabled: true,
        },
        0.1, // 10% iOS mobile devices
    );

    distribution.insert(
        DefaultClientType::Mobile {
            platform: MobilePlatform::Android,
            network_type: NetworkType::Mobile4G,
            http2_enabled: true,
        },
        0.1, // 10% Android mobile devices
    );

    // Create session configuration
    let config = ClientSessionConfig {
        distribution,
        session_creation_rate: 5.0, // 5 new sessions per second
        avg_session_duration_secs: 300.0, // 5 minutes average
        session_duration_variance: 0.5, // 50% coefficient of variation
        avg_requests_per_session: 25.0, // 25 requests per session on average
        think_time_distribution: ThinkTimeDistribution::LogNormal {
            mean_ms: 3000.0, // 3 seconds average think time
            std_dev_ms: 2000.0, // 2 seconds standard deviation
        },
        nav_patterns: create_realistic_navigation_patterns(),
    };

    // Create metrics
    let metrics = DefaultMetricsProvider::new("realistic-session-manager");

    // Create manager
    StandardClientSessionManager::new(
        "realistic-session-manager".to_string(),
        config,
        rate_controller,
        metrics,
    )
}

// Create realistic navigation patterns
fn create_realistic_navigation_patterns() -> Vec<NavigationPattern> {
    vec![
        // Home page browsing pattern
        NavigationPattern {
            name: "Home Page Browsing".to_string(),
            sequence: vec![
                RequestType::PageNavigation, // Home page
                RequestType::ResourceLoad,   // Load resources
                RequestType::ApiCall,        // API call for dynamic content
                RequestType::PageNavigation, // Navigate to product page
            ],
            think_time_modifiers: vec![1.0, 0.1, 0.2, 1.5],
        },

        // Search and browse pattern
        NavigationPattern {
            name: "Search Pattern".to_string(),
            sequence: vec![
                RequestType::FormSubmission, // Search form
                RequestType::ApiCall,        // Search results API
                RequestType::PageNavigation, // Click on result
                RequestType::ResourceLoad,   // Load resources
                RequestType::PageNavigation, // Navigate to another result
            ],
            think_time_modifiers: vec![1.0, 0.2, 1.2, 0.1, 1.5],
        },

        // Authentication pattern
        NavigationPattern {
            name: "Authentication".to_string(),
            sequence: vec![
                RequestType::PageNavigation, // Login page
                RequestType::Authentication, // Submit credentials
                RequestType::ApiCall,        // Get user data
                RequestType::PageNavigation, // Redirect to dashboard
            ],
            think_time_modifiers: vec![1.0, 2.0, 0.2, 1.0],
        },

        // Checkout pattern
        NavigationPattern {
            name: "Checkout".to_string(),
            sequence: vec![
                RequestType::PageNavigation, // Cart page
                RequestType::FormSubmission, // Update quantities
                RequestType::PageNavigation, // Checkout page
                RequestType::FormSubmission, // Shipping info
                RequestType::ApiCall,        // Shipping rates
                RequestType::FormSubmission, // Payment info
                RequestType::ApiCall,        // Process payment
                RequestType::PageNavigation, // Confirmation page
            ],
            think_time_modifiers: vec![1.0, 1.2, 1.5, 2.0, 0.5, 3.0, 0.5, 1.0],
        },
    ]
}
```

### 8.2 Integration with Builder Pattern

```rust
impl LoadTestBuilder {
    /// Configure with realistic browser-based traffic
    pub fn with_realistic_browser_traffic(self) -> Self {
        let mut builder = self.with_client_session_config(ClientSessionConfig {
            session_creation_rate: 5.0,
            avg_session_duration_secs: 300.0,
            session_duration_variance: 0.5,
            avg_requests_per_session: 25.0,
            think_time_distribution: ThinkTimeDistribution::LogNormal {
                mean_ms: 3000.0,
                std_dev_ms: 2000.0,
            },
            distribution: {
                let mut dist = HashMap::new();
                dist.insert(
                    DefaultClientType::Browser {
                        browser_type: BrowserType::Chrome,
                        http2_enabled: true,
                        connection_limit: 6,
                    },
                    0.6,
                );
                dist.insert(
                    DefaultClientType::Browser {
                        browser_type: BrowserType::Firefox,
                        http2_enabled: true,
                        connection_limit: 8,
                    },
                    0.15,
                );
                dist.insert(
                    DefaultClientType::Browser {
                        browser_type: BrowserType::Edge,
                        http2_enabled: true,
                        connection_limit: 8,
                    },
                    0.05,
                );
                dist.insert(
                    DefaultClientType::Mobile {
                        platform: MobilePlatform::iOS,
                        network_type: NetworkType::WiFi,
                        http2_enabled: true,
                    },
                    0.1,
                );
                dist.insert(
                    DefaultClientType::Mobile {
                        platform: MobilePlatform::Android,
                        network_type: NetworkType::Mobile4G,
                        http2_enabled: true,
                    },
                    0.1,
                );
                dist
            },
            nav_patterns: create_realistic_navigation_patterns(),
        });

        // Enable network simulation for realistic conditions
        builder = builder.with_network_simulation(NetworkConditionConfig {
            slow_connection_probability: 0.2,
            consistent_per_session: true,
            bandwidth_config: Some(BandwidthLimitConfig {
                upload_limit: BandwidthLimit::Range(100 * 1024, 5 * 1024 * 1024),
                download_limit: BandwidthLimit::Range(500 * 1024, 10 * 1024 * 1024),
                application_strategy: LimitApplicationStrategy::All,
            }),
            latency_config: Some(LatencySimulationConfig {
                base_latency_ms: LatencySpec::Range(20, 200),
                jitter_ms: LatencySpec::Range(5, 50),
                application_point: LatencyApplicationPoint::Both,
                application_strategy: LimitApplicationStrategy::All,
            }),
            processing_delay_config: Some(ProcessingDelayConfig {
                base_processing_ms: ProcessingDelaySpec::Range(50, 500),
                scale_with_response_size: true,
                scaling_factor_ms_per_kb: 2.0,
            }),
        });

        builder
    }

    /// Configure with API client traffic pattern
    pub fn with_api_client_traffic(self) -> Self {
        self.with_client_session_config(ClientSessionConfig {
            session_creation_rate: 10.0,
            avg_session_duration_secs: 600.0,
            session_duration_variance: 0.3,
            avg_requests_per_session: 100.0,
            think_time_distribution: ThinkTimeDistribution::Exponential {
                mean_ms: 500.0,
            },
            distribution: {
                let mut dist = HashMap::new();
                dist.insert(
                    DefaultClientType::ApiClient {
                        client_library: ApiClientLibrary::Python,
                        persistent_connections: true,
                    },
                    0.3,
                );
                dist.insert(
                    DefaultClientType::ApiClient {
                        client_library: ApiClientLibrary::Node,
                        persistent_connections: true,
                    },
                    0.3,
                );
                dist.insert(
                    DefaultClientType::ApiClient {
                        client_library: ApiClientLibrary::Java,
                        persistent_connections: true,
                    },
                    0.2,
                );
                dist.insert(
                    DefaultClientType::ApiClient {
                        client_library: ApiClientLibrary::CSharp,
                        persistent_connections: true,
                    },
                    0.1,
                );
                dist.insert(
                    DefaultClientType::ApiClient {
                        client_library: ApiClientLibrary::Curl,
                        persistent_connections: false,
                    },
                    0.1,
                );
                dist
            },
            nav_patterns: create_api_navigation_patterns(),
        })
    }
}
```

## 9. Conclusion

The Client Session Manager is a critical component in the load testing framework, bridging the gap between abstract rate targets and realistic client behavior patterns. By modeling user sessions with statistical distributions and behavioral patterns, it generates request tickets that reflect how real users interact with applications.

Key advantages of this design:

1. **Realistic Traffic Patterns**: Sessions model real-world user behavior with appropriate think times, navigation flows, and session durations.

2. **Stateful Interactions**: Sessions maintain state and correlations, reflecting realistic user behavior across multiple requests.

3. **Client Diversity**: Various client types (browsers, mobile devices, API clients) are simulated with appropriate characteristics and behaviors.

4. **Rate Controller Integration**: Translates abstract RPS targets into realistic session creation/management without artificial request patterns.

5. **Static Dispatch Performance**: High-performance implementation with minimal overhead in the request generation path.

6. **Statistical Realism**: Uses appropriate statistical distributions for think time, session duration, and request patterns based on realistic user behavior models.

7. **Composable Design**: Maintains clean interfaces with Rate Controller and Request Scheduler components.

This design enables load tests that not only generate target request volumes but do so in a way that realistically models user behavior, providing more accurate insights into system performance under real-world conditions.
