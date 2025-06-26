# Job Scheduling System Design

## 1. Introduction

The Job Scheduling System provides queuing, scheduling, and lifecycle management for load tests in the NetAnvil-RS framework. It ensures orderly execution of tests, prevents resource conflicts, and enables advanced scheduling strategies like recurring tests, dependencies, and priority-based execution.

### Key Requirements

- **Single Test Execution**: Ensure only one load test runs at a time across the cluster
- **Job Queue Management**: FIFO with priority override capabilities
- **Scheduling Flexibility**: Support immediate, scheduled, and recurring jobs
- **Job Dependencies**: Allow jobs to depend on completion of others
- **Resource Validation**: Ensure cluster has capacity before starting jobs
- **Persistence**: Survive system restarts without losing job queue
- **Observability**: Full visibility into job queue and execution history

## 2. Architecture Overview

### 2.1 System Components

```
┌─────────────────────────────────────────────────────────────────┐
│                        Job Submission                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│   CLI/API          Web UI           Automation Tools           │
│      │                │                    │                    │
│      └────────────────┴────────────────────┘                   │
│                           │                                     │
│                           ▼                                     │
│                  ┌─────────────────┐                          │
│                  │   Job Service   │                          │
│                  │      API        │                          │
│                  └────────┬────────┘                          │
│                           │                                     │
│         ┌─────────────────┼─────────────────┐                │
│         ▼                 ▼                 ▼                │
│  ┌──────────┐     ┌──────────┐     ┌──────────┐            │
│  │   Job    │     │   Job    │     │   Job    │            │
│  │Validator │     │  Queue   │     │Scheduler │            │
│  └──────────┘     └──────────┘     └──────────┘            │
│         │                 │                 │                │
│         └─────────────────┴─────────────────┘                │
│                           │                                   │
│                           ▼                                   │
│                  ┌─────────────────┐                        │
│                  │  Job Executor   │                        │
│                  └────────┬────────┘                        │
│                           │                                   │
│                           ▼                                   │
│                  ┌─────────────────┐                        │
│                  │ Load Test Core  │                        │
│                  │  (Distributed)  │                        │
│                  └─────────────────┘                        │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 State Management

The job scheduling system maintains its state using CRDTs for distributed consistency:

```rust
pub struct JobSchedulerState {
    /// Active job (at most one)
    active_job: LWWReg<Option<JobExecution>, ActorId>,
    
    /// Job queue (pending jobs)
    job_queue: CausalQueue<ScheduledJob, ActorId>,
    
    /// Job definitions
    job_definitions: Map<JobId, JobDefinition, ActorId>,
    
    /// Execution history
    execution_history: Map<ExecutionId, JobExecution, ActorId>,
    
    /// Recurring job schedules
    recurring_schedules: Map<JobId, RecurringSchedule, ActorId>,
}
```

## 3. Core Data Types

### 3.1 Job Definition

```rust
/// A job definition that can be executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDefinition {
    /// Unique job identifier
    pub id: JobId,
    
    /// Job metadata
    pub metadata: JobMetadata,
    
    /// Load test configuration
    pub test_config: LoadTestConfig,
    
    /// Scheduling constraints
    pub scheduling: SchedulingConfig,
    
    /// Resource requirements
    pub resources: ResourceRequirements,
    
    /// Success criteria
    pub success_criteria: Option<SuccessCriteria>,
    
    /// Notification settings
    pub notifications: NotificationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobMetadata {
    /// Human-readable name
    pub name: String,
    
    /// Description
    pub description: Option<String>,
    
    /// Tags for categorization
    pub tags: Vec<String>,
    
    /// Owner/creator
    pub owner: String,
    
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    
    /// Last modified
    pub updated_at: DateTime<Utc>,
    
    /// Version for optimistic locking
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Scheduling type
    pub schedule_type: ScheduleType,
    
    /// Priority (higher = more important)
    pub priority: i32,
    
    /// Maximum runtime
    pub max_duration: Duration,
    
    /// Dependencies on other jobs
    pub dependencies: Vec<JobDependency>,
    
    /// Retry configuration
    pub retry_policy: Option<RetryPolicy>,
    
    /// Timeout behavior
    pub timeout_action: TimeoutAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScheduleType {
    /// Run immediately when resources available
    Immediate,
    
    /// Run at specific time
    Scheduled { start_time: DateTime<Utc> },
    
    /// Recurring schedule
    Recurring { schedule: RecurringSchedule },
    
    /// Manual trigger only
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringSchedule {
    /// Cron expression
    pub cron: String,
    
    /// Timezone for schedule
    pub timezone: String,
    
    /// When to stop recurring
    pub end_condition: EndCondition,
    
    /// What to do if previous run still active
    pub overlap_policy: OverlapPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EndCondition {
    /// Never end
    Never,
    
    /// End after N occurrences
    AfterOccurrences(u32),
    
    /// End at specific time
    UntilTime(DateTime<Utc>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OverlapPolicy {
    /// Skip this occurrence
    Skip,
    
    /// Queue for later execution
    Queue,
    
    /// Cancel previous and start new
    CancelPrevious,
    
    /// Allow concurrent execution (dangerous!)
    AllowConcurrent,
}
```

### 3.2 Job Execution

```rust
/// A job in execution or completed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobExecution {
    /// Unique execution ID
    pub id: ExecutionId,
    
    /// Job definition ID
    pub job_id: JobId,
    
    /// Current state
    pub state: ExecutionState,
    
    /// When execution started
    pub started_at: DateTime<Utc>,
    
    /// When execution completed
    pub completed_at: Option<DateTime<Utc>>,
    
    /// Assigned cluster resources
    pub assigned_resources: AssignedResources,
    
    /// Test ID in the load test system
    pub test_id: Option<String>,
    
    /// Execution result
    pub result: Option<ExecutionResult>,
    
    /// Metrics summary
    pub metrics_summary: Option<MetricsSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionState {
    /// Waiting for resources
    Pending,
    
    /// Initializing test
    Initializing,
    
    /// Test running
    Running {
        progress: f32,
        estimated_completion: DateTime<Utc>,
    },
    
    /// Test completing
    Completing,
    
    /// Successfully completed
    Succeeded,
    
    /// Failed
    Failed { reason: String },
    
    /// Cancelled by user
    Cancelled,
    
    /// Timed out
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether success criteria were met
    pub success: bool,
    
    /// Detailed metrics
    pub metrics: TestResults,
    
    /// Any errors encountered
    pub errors: Vec<String>,
    
    /// Warnings
    pub warnings: Vec<String>,
    
    /// Link to full results
    pub results_url: Option<String>,
}
```

### 3.3 Resource Management

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequirements {
    /// Minimum RPS needed
    pub min_rps: u64,
    
    /// Preferred RPS
    pub preferred_rps: u64,
    
    /// Minimum nodes
    pub min_nodes: u32,
    
    /// Geographic requirements
    pub regions: Option<Vec<String>>,
    
    /// Specific node capabilities
    pub node_capabilities: Option<NodeCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignedResources {
    /// Nodes assigned to this job
    pub nodes: Vec<NodeId>,
    
    /// Total RPS capacity
    pub total_rps: u64,
    
    /// Resource allocation timestamp
    pub allocated_at: DateTime<Utc>,
}
```

## 4. Job Queue Implementation

### 4.1 Priority Queue with CRDT

```rust
/// Causal queue implementation using CRDTs
pub struct CausalQueue<T, A: Actor> {
    /// Items with their priorities and timestamps
    items: Map<QueueId, QueueItem<T>, A>,
    
    /// Ordering information
    ordering: CausalOrdering<A>,
    
    /// Actor ID
    actor: A,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueueItem<T> {
    /// The actual item
    pub item: T,
    
    /// Priority (higher = more important)
    pub priority: i32,
    
    /// Submission time for FIFO within priority
    pub submitted_at: DateTime<Utc>,
    
    /// Unique ID for deduplication
    pub id: QueueId,
    
    /// Status in queue
    pub status: QueueItemStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum QueueItemStatus {
    /// Waiting in queue
    Pending,
    
    /// Currently being processed
    Processing,
    
    /// Completed
    Completed,
    
    /// Cancelled
    Cancelled,
}

impl<T: Clone, A: Actor + Clone> CausalQueue<T, A> {
    /// Add item to queue
    pub fn enqueue(&mut self, item: T, priority: i32) -> QueueId {
        let id = QueueId::new();
        let queue_item = QueueItem {
            item,
            priority,
            submitted_at: Utc::now(),
            id: id.clone(),
            status: QueueItemStatus::Pending,
        };
        
        let ctx = self.items.read_ctx().derive_add_ctx(self.actor.clone());
        self.items.apply(self.items.update(id.clone(), queue_item, ctx));
        
        id
    }
    
    /// Get next item by priority and FIFO
    pub fn dequeue(&mut self) -> Option<T> {
        let pending_items: Vec<_> = self.items
            .values()
            .filter(|item| matches!(item.status, QueueItemStatus::Pending))
            .collect();
        
        // Sort by priority (desc) then by time (asc)
        let next = pending_items
            .into_iter()
            .max_by(|a, b| {
                match b.priority.cmp(&a.priority) {
                    Ordering::Equal => a.submitted_at.cmp(&b.submitted_at),
                    other => other,
                }
            });
        
        if let Some(item) = next {
            // Mark as processing
            let mut updated = item.clone();
            updated.status = QueueItemStatus::Processing;
            
            let ctx = self.items.read_ctx().derive_add_ctx(self.actor.clone());
            self.items.apply(self.items.update(item.id.clone(), updated, ctx));
            
            Some(item.item.clone())
        } else {
            None
        }
    }
}
```

### 4.2 Job Scheduler Service

```rust
pub struct JobScheduler {
    /// Distributed state
    state: Arc<RwLock<JobSchedulerState>>,
    
    /// Job executor
    executor: Arc<JobExecutor>,
    
    /// Resource manager
    resource_manager: Arc<ResourceManager>,
    
    /// Notification service
    notifier: Arc<NotificationService>,
    
    /// Scheduler configuration
    config: SchedulerConfig,
}

impl JobScheduler {
    /// Main scheduling loop
    pub async fn run(&self) -> Result<()> {
        let mut interval = tokio::time::interval(self.config.schedule_interval);
        
        loop {
            interval.tick().await;
            
            // Check if any job is currently running
            if self.is_job_active().await {
                continue;
            }
            
            // Get next job from queue
            if let Some(job) = self.get_next_job().await? {
                // Validate resources
                if self.can_execute(&job).await? {
                    // Start execution
                    self.execute_job(job).await?;
                } else {
                    // Re-queue if resources not available
                    self.requeue_job(job).await?;
                }
            }
            
            // Check recurring schedules
            self.check_recurring_schedules().await?;
            
            // Clean up old executions
            self.cleanup_history().await?;
        }
    }
    
    /// Check if resources available for job
    async fn can_execute(&self, job: &ScheduledJob) -> Result<bool> {
        let requirements = &job.definition.resources;
        let available = self.resource_manager.get_available_resources().await?;
        
        Ok(available.total_rps >= requirements.min_rps &&
           available.node_count >= requirements.min_nodes)
    }
    
    /// Execute a job
    async fn execute_job(&self, job: ScheduledJob) -> Result<()> {
        // Create execution record
        let execution = JobExecution {
            id: ExecutionId::new(),
            job_id: job.definition.id.clone(),
            state: ExecutionState::Initializing,
            started_at: Utc::now(),
            completed_at: None,
            assigned_resources: self.resource_manager
                .allocate_resources(&job.definition.resources)
                .await?,
            test_id: None,
            result: None,
            metrics_summary: None,
        };
        
        // Update state
        self.state.write().await.active_job.set(Some(execution.clone()));
        
        // Start execution
        let executor = self.executor.clone();
        let notifier = self.notifier.clone();
        
        tokio::spawn(async move {
            // Run the job
            match executor.run_job(execution).await {
                Ok(result) => {
                    notifier.notify_completion(&job, &result).await;
                }
                Err(e) => {
                    notifier.notify_failure(&job, &e).await;
                }
            }
        });
        
        Ok(())
    }
}
```

### 4.3 Job Executor

```rust
pub struct JobExecutor {
    /// Reference to load test coordinator
    coordinator: Arc<DistributedCoordinator>,
    
    /// Metrics collector
    metrics_collector: Arc<MetricsCollector>,
    
    /// Progress tracker
    progress_tracker: Arc<ProgressTracker>,
}

impl JobExecutor {
    pub async fn run_job(&self, mut execution: JobExecution) -> Result<ExecutionResult> {
        // Get job definition
        let job = self.get_job_definition(&execution.job_id).await?;
        
        // Initialize load test
        let test_id = self.coordinator
            .create_test(&job.test_config)
            .await?;
        
        execution.test_id = Some(test_id.clone());
        execution.state = ExecutionState::Running {
            progress: 0.0,
            estimated_completion: Utc::now() + job.scheduling.max_duration,
        };
        
        // Update execution state
        self.update_execution(execution.clone()).await?;
        
        // Monitor test progress
        let start_time = Instant::now();
        let max_duration = job.scheduling.max_duration;
        
        loop {
            // Check timeout
            if start_time.elapsed() > max_duration {
                self.coordinator.stop_test(&test_id).await?;
                return Err(JobError::Timeout);
            }
            
            // Get test status
            let status = self.coordinator.get_test_status(&test_id).await?;
            
            match status.state {
                TestState::Completed => {
                    // Collect results
                    let results = self.collect_results(&test_id).await?;
                    
                    // Check success criteria
                    let success = self.evaluate_success_criteria(
                        &job.success_criteria,
                        &results
                    )?;
                    
                    return Ok(ExecutionResult {
                        success,
                        metrics: results,
                        errors: vec![],
                        warnings: vec![],
                        results_url: Some(format!("/results/{}", test_id)),
                    });
                }
                TestState::Failed => {
                    return Err(JobError::TestFailed(status.error_message));
                }
                _ => {
                    // Update progress
                    let progress = status.progress_percentage;
                    execution.state = ExecutionState::Running {
                        progress,
                        estimated_completion: self.estimate_completion(&status),
                    };
                    
                    self.update_execution(execution.clone()).await?;
                    
                    // Wait before next check
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
    
    /// Evaluate success criteria
    fn evaluate_success_criteria(
        &self,
        criteria: &Option<SuccessCriteria>,
        results: &TestResults,
    ) -> Result<bool> {
        let Some(criteria) = criteria else {
            return Ok(true); // No criteria = success
        };
        
        // Check latency criteria
        if let Some(max_p99) = criteria.max_p99_latency_ms {
            let p99 = results.latency_histogram.value_at_percentile(99.0);
            if p99 > max_p99 * 1000 {
                return Ok(false);
            }
        }
        
        // Check error rate
        if let Some(max_error_rate) = criteria.max_error_rate {
            let error_rate = results.error_count as f64 / results.total_requests as f64;
            if error_rate > max_error_rate {
                return Ok(false);
            }
        }
        
        // Check minimum throughput
        if let Some(min_rps) = criteria.min_throughput_rps {
            let actual_rps = results.total_requests as f64 / results.duration.as_secs_f64();
            if actual_rps < min_rps {
                return Ok(false);
            }
        }
        
        Ok(true)
    }
}
```

## 5. Recurring Jobs

### 5.1 Schedule Evaluator

```rust
use cron::Schedule;

pub struct RecurringJobScheduler {
    /// CRDT state
    state: Arc<RwLock<JobSchedulerState>>,
    
    /// Timezone database
    tz_db: TzDatabase,
}

impl RecurringJobScheduler {
    /// Check all recurring schedules
    pub async fn check_schedules(&self) -> Result<Vec<JobId>> {
        let mut jobs_to_queue = Vec::new();
        let now = Utc::now();
        
        let state = self.state.read().await;
        
        for (job_id, schedule) in state.recurring_schedules.iter() {
            if self.should_trigger(schedule, now)? {
                // Check overlap policy
                if self.can_queue_job(job_id, &schedule.overlap_policy).await? {
                    jobs_to_queue.push(job_id.clone());
                }
            }
        }
        
        Ok(jobs_to_queue)
    }
    
    /// Check if schedule should trigger
    fn should_trigger(&self, schedule: &RecurringSchedule, now: DateTime<Utc>) -> Result<bool> {
        // Parse cron expression
        let cron = Schedule::from_str(&schedule.cron)?;
        
        // Get timezone
        let tz = self.tz_db.get(&schedule.timezone)?;
        
        // Convert to local time
        let local_now = now.with_timezone(&tz);
        
        // Check if we should run
        let next_run = cron.after(&local_now).next();
        
        Ok(next_run.map(|t| t <= local_now + Duration::minutes(1))
            .unwrap_or(false))
    }
}
```

## 6. Job Dependencies

### 6.1 Dependency Resolution

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobDependency {
    /// Job that must complete first
    pub job_id: JobId,
    
    /// Required outcome
    pub required_outcome: DependencyOutcome,
    
    /// How long to wait for dependency
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DependencyOutcome {
    /// Must succeed
    Success,
    
    /// Must complete (any outcome)
    Completion,
    
    /// Must complete with specific metric threshold
    MetricThreshold {
        metric: String,
        operator: ComparisonOperator,
        value: f64,
    },
}

pub struct DependencyResolver {
    state: Arc<RwLock<JobSchedulerState>>,
}

impl DependencyResolver {
    /// Check if all dependencies satisfied
    pub async fn dependencies_satisfied(&self, job: &JobDefinition) -> Result<bool> {
        for dep in &job.scheduling.dependencies {
            if !self.is_dependency_satisfied(dep).await? {
                return Ok(false);
            }
        }
        
        Ok(true)
    }
    
    async fn is_dependency_satisfied(&self, dep: &JobDependency) -> Result<bool> {
        let state = self.state.read().await;
        
        // Find most recent execution of dependency
        let execution = state.execution_history
            .values()
            .filter(|e| e.job_id == dep.job_id)
            .max_by_key(|e| e.started_at);
        
        let Some(execution) = execution else {
            return Ok(false); // Never ran
        };
        
        match dep.required_outcome {
            DependencyOutcome::Success => {
                Ok(matches!(execution.state, ExecutionState::Succeeded))
            }
            DependencyOutcome::Completion => {
                Ok(matches!(
                    execution.state,
                    ExecutionState::Succeeded | ExecutionState::Failed { .. }
                ))
            }
            DependencyOutcome::MetricThreshold { ref metric, ref operator, value } => {
                // Check execution metrics
                if let Some(summary) = &execution.metrics_summary {
                    let metric_value = self.get_metric_value(summary, metric)?;
                    Ok(operator.compare(metric_value, value))
                } else {
                    Ok(false)
                }
            }
        }
    }
}
```

## 7. API Integration

### 7.1 Job Management API

```protobuf
service JobManagementService {
  // Job CRUD operations
  rpc CreateJob(CreateJobRequest) returns (Job);
  rpc GetJob(GetJobRequest) returns (Job);
  rpc UpdateJob(UpdateJobRequest) returns (Job);
  rpc DeleteJob(DeleteJobRequest) returns (google.protobuf.Empty);
  rpc ListJobs(ListJobsRequest) returns (ListJobsResponse);
  
  // Queue management
  rpc QueueJob(QueueJobRequest) returns (QueueJobResponse);
  rpc GetQueueStatus(GetQueueStatusRequest) returns (QueueStatus);
  rpc CancelQueuedJob(CancelQueuedJobRequest) returns (google.protobuf.Empty);
  rpc ReorderQueue(ReorderQueueRequest) returns (google.protobuf.Empty);
  
  // Execution management
  rpc GetExecutionStatus(GetExecutionStatusRequest) returns (ExecutionStatus);
  rpc ListExecutions(ListExecutionsRequest) returns (ListExecutionsResponse);
  rpc CancelExecution(CancelExecutionRequest) returns (google.protobuf.Empty);
  
  // Recurring jobs
  rpc CreateRecurringJob(CreateRecurringJobRequest) returns (RecurringJob);
  rpc PauseRecurringJob(PauseRecurringJobRequest) returns (google.protobuf.Empty);
  rpc ResumeRecurringJob(ResumeRecurringJobRequest) returns (google.protobuf.Empty);
}
```

### 7.2 REST API

```yaml
paths:
  /api/v1/jobs:
    post:
      summary: Create a new job
      requestBody:
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/JobDefinition'
      responses:
        201:
          description: Job created
          
  /api/v1/jobs/{jobId}/queue:
    post:
      summary: Queue a job for execution
      parameters:
        - name: jobId
          in: path
          required: true
          schema:
            type: string
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                priority:
                  type: integer
                  description: Override default priority
                scheduledTime:
                  type: string
                  format: date-time
                  description: Schedule for later execution
                
  /api/v1/queue:
    get:
      summary: Get queue status
      responses:
        200:
          description: Queue status
          content:
            application/json:
              schema:
                type: object
                properties:
                  pendingJobs:
                    type: integer
                  activeJob:
                    $ref: '#/components/schemas/JobExecution'
                  estimatedWaitTime:
                    type: string
                    format: duration
```

## 8. Notifications

### 8.1 Notification Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    /// Email notifications
    pub email: Option<EmailNotification>,
    
    /// Webhook notifications
    pub webhooks: Vec<WebhookNotification>,
    
    /// Slack notifications
    pub slack: Option<SlackNotification>,
    
    /// When to notify
    pub triggers: NotificationTriggers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationTriggers {
    pub on_start: bool,
    pub on_completion: bool,
    pub on_failure: bool,
    pub on_timeout: bool,
    pub on_criteria_fail: bool,
}

pub struct NotificationService {
    email_sender: Arc<EmailSender>,
    webhook_client: Arc<WebhookClient>,
    slack_client: Arc<SlackClient>,
}

impl NotificationService {
    pub async fn notify_job_event(
        &self,
        job: &JobDefinition,
        event: JobEvent,
    ) -> Result<()> {
        let config = &job.notifications;
        
        // Check if we should notify for this event
        if !self.should_notify(&config.triggers, &event) {
            return Ok(());
        }
        
        // Send notifications in parallel
        let mut tasks = Vec::new();
        
        if let Some(email) = &config.email {
            tasks.push(self.send_email_notification(email, job, &event));
        }
        
        for webhook in &config.webhooks {
            tasks.push(self.send_webhook_notification(webhook, job, &event));
        }
        
        if let Some(slack) = &config.slack {
            tasks.push(self.send_slack_notification(slack, job, &event));
        }
        
        // Wait for all notifications
        futures::future::join_all(tasks).await;
        
        Ok(())
    }
}
```

## 9. Persistence and Recovery

### 9.1 State Persistence

```rust
pub struct PersistentJobScheduler {
    /// In-memory state
    state: Arc<RwLock<JobSchedulerState>>,
    
    /// Persistent storage
    storage: Arc<dyn JobStorage>,
    
    /// Sync interval
    sync_interval: Duration,
}

#[async_trait]
pub trait JobStorage: Send + Sync {
    /// Save job definition
    async fn save_job(&self, job: &JobDefinition) -> Result<()>;
    
    /// Load all jobs
    async fn load_jobs(&self) -> Result<Vec<JobDefinition>>;
    
    /// Save execution
    async fn save_execution(&self, execution: &JobExecution) -> Result<()>;
    
    /// Load execution history
    async fn load_executions(&self, limit: usize) -> Result<Vec<JobExecution>>;
    
    /// Save queue state
    async fn save_queue(&self, queue: &[ScheduledJob]) -> Result<()>;
    
    /// Load queue state
    async fn load_queue(&self) -> Result<Vec<ScheduledJob>>;
}

impl PersistentJobScheduler {
    /// Recover state from storage
    pub async fn recover(&self) -> Result<()> {
        // Load jobs
        let jobs = self.storage.load_jobs().await?;
        for job in jobs {
            self.state.write().await.job_definitions
                .insert(job.id.clone(), job);
        }
        
        // Load queue
        let queue = self.storage.load_queue().await?;
        for item in queue {
            self.state.write().await.job_queue.enqueue(item, 0);
        }
        
        // Load recent executions
        let executions = self.storage.load_executions(1000).await?;
        for exec in executions {
            self.state.write().await.execution_history
                .insert(exec.id.clone(), exec);
        }
        
        Ok(())
    }
}
```

## 10. Monitoring and Observability

### 10.1 Metrics

```rust
pub struct JobSchedulerMetrics {
    /// Jobs in queue
    pub queued_jobs: Gauge,
    
    /// Active jobs
    pub active_jobs: Gauge,
    
    /// Completed jobs counter
    pub completed_jobs: Counter,
    
    /// Failed jobs counter  
    pub failed_jobs: Counter,
    
    /// Job duration histogram
    pub job_duration: Histogram,
    
    /// Queue wait time
    pub queue_wait_time: Histogram,
    
    /// Resource utilization
    pub resource_utilization: Gauge,
}
```

### 10.2 Job History and Analytics

```rust
pub struct JobAnalytics {
    storage: Arc<dyn JobStorage>,
}

impl JobAnalytics {
    /// Get job success rate
    pub async fn get_success_rate(
        &self,
        job_id: &JobId,
        time_range: TimeRange,
    ) -> Result<f64> {
        let executions = self.storage
            .load_executions_for_job(job_id, time_range)
            .await?;
        
        let total = executions.len() as f64;
        let succeeded = executions.iter()
            .filter(|e| matches!(e.state, ExecutionState::Succeeded))
            .count() as f64;
        
        Ok(succeeded / total)
    }
    
    /// Get average duration
    pub async fn get_average_duration(
        &self,
        job_id: &JobId,
        time_range: TimeRange,
    ) -> Result<Duration> {
        let executions = self.storage
            .load_executions_for_job(job_id, time_range)
            .await?;
        
        let total_duration: Duration = executions.iter()
            .filter_map(|e| {
                e.completed_at.map(|end| end - e.started_at)
            })
            .sum();
        
        Ok(total_duration / executions.len() as u32)
    }
}
```

## 11. Security and Access Control

### 11.1 Job Permissions

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPermissions {
    /// Who can view this job
    pub viewers: Vec<Principal>,
    
    /// Who can modify this job
    pub editors: Vec<Principal>,
    
    /// Who can execute this job
    pub executors: Vec<Principal>,
    
    /// Who can delete this job
    pub owners: Vec<Principal>,
}

pub struct JobAuthorizationService {
    /// Permission checker
    permission_service: Arc<PermissionService>,
}

impl JobAuthorizationService {
    pub async fn can_execute(
        &self,
        principal: &Principal,
        job: &JobDefinition,
    ) -> Result<bool> {
        // Check if principal is in executors list
        if job.permissions.executors.contains(principal) {
            return Ok(true);
        }
        
        // Check if principal has admin role
        if self.permission_service.has_role(principal, "admin").await? {
            return Ok(true);
        }
        
        Ok(false)
    }
}
```

## 12. Integration Example

```rust
// Create a recurring load test job
let job = JobDefinition {
    id: JobId::new(),
    metadata: JobMetadata {
        name: "Daily API Load Test".to_string(),
        description: Some("Test API performance every day at 2 AM".to_string()),
        tags: vec!["api".to_string(), "daily".to_string()],
        owner: "ops-team".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 1,
    },
    test_config: LoadTestConfig {
        urls: vec!["https://api.example.com/v1/health".to_string()],
        initial_rps: 1000,
        max_rps: 5000,
        test_duration_secs: 600,
        // ... other config
    },
    scheduling: SchedulingConfig {
        schedule_type: ScheduleType::Recurring {
            schedule: RecurringSchedule {
                cron: "0 2 * * *".to_string(), // 2 AM daily
                timezone: "America/New_York".to_string(),
                end_condition: EndCondition::Never,
                overlap_policy: OverlapPolicy::Skip,
            },
        },
        priority: 5,
        max_duration: Duration::from_secs(1200),
        dependencies: vec![],
        retry_policy: Some(RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::Exponential,
        }),
        timeout_action: TimeoutAction::Fail,
    },
    resources: ResourceRequirements {
        min_rps: 5000,
        preferred_rps: 10000,
        min_nodes: 3,
        regions: Some(vec!["us-east".to_string()]),
        node_capabilities: None,
    },
    success_criteria: Some(SuccessCriteria {
        max_p99_latency_ms: Some(200),
        max_error_rate: Some(0.001),
        min_throughput_rps: Some(900),
    }),
    notifications: NotificationConfig {
        email: Some(EmailNotification {
            recipients: vec!["ops-team@example.com".to_string()],
            template: "load-test-results".to_string(),
        }),
        webhooks: vec![],
        slack: Some(SlackNotification {
            channel: "#ops-alerts".to_string(),
            webhook_url: "https://hooks.slack.com/...".to_string(),
        }),
        triggers: NotificationTriggers {
            on_start: false,
            on_completion: true,
            on_failure: true,
            on_timeout: true,
            on_criteria_fail: true,
        },
    },
};

// Submit the job
let client = JobManagementClient::connect("http://scheduler:50051").await?;
let response = client.create_job(job).await?;

println!("Created job: {}", response.id);
```

This job scheduling system provides enterprise-grade job management for the NetAnvil-RS load testing framework, enabling automated, recurring, and dependent test execution with full observability and control.