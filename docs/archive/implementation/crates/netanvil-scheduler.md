# netanvil-scheduler Implementation Guide

## Overview

The `netanvil-scheduler` crate implements job scheduling and lifecycle management for the NetAnvil-RS load testing system. It ensures orderly execution of tests, manages job queues, handles recurring schedules, and enforces resource constraints.

## Related Design Documents

- [Job Scheduling System Design](../../section-5-2-job-scheduling-design.md) - Complete scheduling architecture
- [CRDT Library Integration](../../section-4-4-crdt-library-integration.md) - Distributed queue management
- [Distributed Coordination](../../section-4-2-distributed-coordination-design.md) - Integration context

## Core Architecture

### Main Scheduler Structure

```rust
use netanvil_crdt::DistributedState;
use netanvil_core::LoadTest;
use netanvil_distributed::DistributedCoordinator;

pub struct JobScheduler {
    /// Distributed state with CRDT-based queue
    state: Arc<RwLock<JobSchedulerState>>,
    
    /// Job executor that interfaces with load test system
    executor: Arc<JobExecutor>,
    
    /// Resource manager for capacity validation
    resource_manager: Arc<ResourceManager>,
    
    /// Notification service
    notifier: Arc<NotificationService>,
    
    /// Recurring job scheduler
    recurring_scheduler: Arc<RecurringJobScheduler>,
    
    /// Scheduler configuration
    config: SchedulerConfig,
    
    /// Metrics
    metrics: SchedulerMetrics,
}

/// CRDT-based scheduler state
pub struct JobSchedulerState {
    /// Active job (at most one)
    active_job: LWWReg<Option<JobExecution>, ActorId>,
    
    /// Job queue using causal ordering
    job_queue: CausalQueue<QueuedJob, ActorId>,
    
    /// Job definitions
    job_definitions: Map<JobId, JobDefinition, ActorId>,
    
    /// Execution history
    execution_history: Map<ExecutionId, JobExecution, ActorId>,
    
    /// Recurring schedules
    recurring_schedules: Map<JobId, RecurringSchedule, ActorId>,
    
    /// Actor ID for this node
    actor: ActorId,
}
```

### CRDT-based Priority Queue

Implementing a distributed priority queue using CRDTs:

```rust
use crdts::{Map, CmRDT, VClock};

/// Causal queue with priority support
pub struct CausalQueue<T, A: Actor> {
    /// Items in the queue
    items: Map<QueueId, QueueItem<T>, A>,
    
    /// Causal ordering information
    ordering: Map<QueueId, VClock<A>, A>,
    
    /// Actor ID
    actor: A,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem<T> {
    pub id: QueueId,
    pub item: T,
    pub priority: i32,
    pub submitted_at: u64, // Unix timestamp
    pub submitted_by: ActorId,
    pub status: QueueItemStatus,
}

impl<T: Clone, A: Actor + Clone> CausalQueue<T, A> {
    pub fn enqueue(&mut self, item: T, priority: i32) -> QueueId {
        let id = QueueId::new();
        let queue_item = QueueItem {
            id: id.clone(),
            item,
            priority,
            submitted_at: current_timestamp(),
            submitted_by: self.actor.clone(),
            status: QueueItemStatus::Pending,
        };
        
        // Add to CRDT map
        let ctx = self.items.read_ctx().derive_add_ctx(self.actor.clone());
        self.items.apply(self.items.update(id.clone(), queue_item, ctx));
        
        // Track causal ordering
        let clock = VClock::new();
        let clock_ctx = self.ordering.read_ctx().derive_add_ctx(self.actor.clone());
        self.ordering.apply(self.ordering.update(id.clone(), clock, clock_ctx));
        
        id
    }
    
    pub fn dequeue(&mut self) -> Option<(QueueId, T)> {
        // Get all pending items
        let pending: Vec<_> = self.items
            .values()
            .filter(|item| matches!(item.status, QueueItemStatus::Pending))
            .collect();
        
        // Sort by priority (desc) then timestamp (asc)
        let next = pending.into_iter()
            .max_by(|a, b| {
                match b.priority.cmp(&a.priority) {
                    Ordering::Equal => a.submitted_at.cmp(&b.submitted_at),
                    other => other,
                }
            });
        
        if let Some(item) = next {
            // Mark as processing
            let mut updated = item.clone();
            updated.status = QueueItemStatus::Processing {
                processor: self.actor.clone(),
                started_at: current_timestamp(),
            };
            
            let ctx = self.items.read_ctx().derive_add_ctx(self.actor.clone());
            self.items.apply(self.items.update(item.id.clone(), updated, ctx));
            
            Some((item.id.clone(), item.item.clone()))
        } else {
            None
        }
    }
    
    /// Merge with another queue (CRDT operation)
    pub fn merge(&mut self, other: &Self) {
        self.items.merge(other.items.clone());
        self.ordering.merge(other.ordering.clone());
    }
}
```

### Main Scheduling Loop

```rust
impl JobScheduler {
    pub async fn run(&self) -> Result<()> {
        info!("Starting job scheduler");
        
        // Start background tasks
        let recurr_handle = tokio::spawn(
            self.recurring_scheduler.clone().run()
        );
        
        let cleanup_handle = tokio::spawn(
            self.clone().cleanup_loop()
        );
        
        // Main scheduling loop
        let mut interval = tokio::time::interval(self.config.schedule_interval);
        
        loop {
            interval.tick().await;
            
            // Check if we can run a job
            if !self.can_execute_job().await {
                continue;
            }
            
            // Get next job from queue
            if let Some((queue_id, job)) = self.dequeue_job().await? {
                // Validate job can still run
                match self.validate_job(&job).await {
                    Ok(()) => {
                        // Start execution
                        self.execute_job(queue_id, job).await?;
                    }
                    Err(e) => {
                        warn!("Job validation failed: {}", e);
                        self.mark_job_failed(queue_id, e.to_string()).await?;
                    }
                }
            }
            
            // Process any state updates from gossip
            self.process_state_updates().await?;
        }
    }
    
    async fn can_execute_job(&self) -> bool {
        let state = self.state.read().await;
        
        // Check if any job is currently active
        match state.active_job.read() {
            Some(job) => {
                // Check if job is still running
                matches!(job.state, ExecutionState::Running { .. })
            }
            None => true, // No active job
        }
    }
    
    async fn validate_job(&self, job: &QueuedJob) -> Result<()> {
        // Check dependencies
        if !self.check_dependencies(&job.definition).await? {
            return Err(SchedulerError::DependenciesNotMet);
        }
        
        // Check resources
        if !self.check_resources(&job.definition).await? {
            return Err(SchedulerError::InsufficientResources);
        }
        
        // Check schedule window
        if let Some(window) = &job.schedule_window {
            if !window.is_valid_now() {
                return Err(SchedulerError::OutsideScheduleWindow);
            }
        }
        
        Ok(())
    }
}
```

### Job Executor

Bridges the scheduler with the load test system:

```rust
pub struct JobExecutor {
    /// Reference to distributed coordinator
    coordinator: Arc<DistributedCoordinator>,
    
    /// Progress tracking
    progress_tracker: Arc<ProgressTracker>,
    
    /// Success criteria evaluator
    evaluator: Arc<SuccessEvaluator>,
}

impl JobExecutor {
    pub async fn execute_job(
        &self,
        queue_id: QueueId,
        job: QueuedJob,
    ) -> Result<()> {
        info!("Starting execution of job {}", job.definition.id);
        
        // Create execution record
        let mut execution = JobExecution {
            id: ExecutionId::new(),
            queue_id,
            job_id: job.definition.id.clone(),
            state: ExecutionState::Initializing,
            started_at: Utc::now(),
            completed_at: None,
            assigned_resources: self.allocate_resources(&job.definition).await?,
            test_id: None,
            result: None,
        };
        
        // Update state to mark job as active
        self.update_active_job(Some(execution.clone())).await?;
        
        // Create load test configuration
        let test_config = self.create_test_config(&job.definition)?;
        
        // Start load test via coordinator
        let test_id = self.coordinator
            .create_and_start_test(test_config)
            .await?;
        
        execution.test_id = Some(test_id.clone());
        execution.state = ExecutionState::Running {
            progress: 0.0,
            estimated_completion: Utc::now() + job.definition.scheduling.max_duration,
        };
        
        self.update_active_job(Some(execution.clone())).await?;
        
        // Monitor execution
        self.monitor_execution(execution, job.definition).await
    }
    
    async fn monitor_execution(
        &self,
        mut execution: JobExecution,
        definition: JobDefinition,
    ) -> Result<()> {
        let test_id = execution.test_id.as_ref().unwrap();
        let start_time = Instant::now();
        
        loop {
            // Check timeout
            if start_time.elapsed() > definition.scheduling.max_duration {
                self.handle_timeout(&mut execution, &definition).await?;
                return Ok(());
            }
            
            // Get test status
            let status = self.coordinator.get_test_status(test_id).await?;
            
            match status.state {
                TestState::Completed => {
                    let result = self.process_completion(&execution, &definition).await?;
                    execution.state = ExecutionState::Succeeded;
                    execution.result = Some(result);
                    execution.completed_at = Some(Utc::now());
                    
                    self.finalize_execution(execution).await?;
                    return Ok(());
                }
                
                TestState::Failed => {
                    execution.state = ExecutionState::Failed {
                        reason: status.error_message.unwrap_or_default(),
                    };
                    execution.completed_at = Some(Utc::now());
                    
                    self.finalize_execution(execution).await?;
                    return Ok(());
                }
                
                TestState::Running => {
                    // Update progress
                    let progress = self.calculate_progress(&status);
                    execution.state = ExecutionState::Running {
                        progress,
                        estimated_completion: self.estimate_completion(&status),
                    };
                    
                    self.update_active_job(Some(execution.clone())).await?;
                }
                
                _ => {} // Other states
            }
            
            // Wait before next check
            tokio::time::sleep(self.config.monitor_interval).await;
        }
    }
}
```

### Recurring Job Scheduler

```rust
use cron::Schedule;
use chrono_tz::Tz;

pub struct RecurringJobScheduler {
    state: Arc<RwLock<JobSchedulerState>>,
    scheduler: Arc<JobScheduler>,
}

impl RecurringJobScheduler {
    pub async fn run(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        
        loop {
            interval.tick().await;
            
            if let Err(e) = self.check_schedules().await {
                error!("Error checking recurring schedules: {}", e);
            }
        }
    }
    
    async fn check_schedules(&self) -> Result<()> {
        let now = Utc::now();
        let state = self.state.read().await;
        
        for (job_id, schedule) in state.recurring_schedules.entries() {
            if self.should_trigger(schedule, now)? {
                match schedule.overlap_policy {
                    OverlapPolicy::Skip => {
                        if self.is_job_running(job_id).await {
                            continue; // Skip this occurrence
                        }
                    }
                    OverlapPolicy::Queue => {
                        // Always queue
                    }
                    OverlapPolicy::CancelPrevious => {
                        if let Some(exec_id) = self.get_running_execution(job_id).await {
                            self.cancel_execution(exec_id).await?;
                        }
                    }
                }
                
                // Queue the job
                self.queue_recurring_job(job_id, schedule).await?;
            }
        }
        
        Ok(())
    }
    
    fn should_trigger(&self, schedule: &RecurringSchedule, now: DateTime<Utc>) -> Result<bool> {
        // Parse cron expression
        let cron = Schedule::from_str(&schedule.cron)?;
        
        // Convert to local timezone
        let tz: Tz = schedule.timezone.parse()?;
        let local_now = now.with_timezone(&tz);
        
        // Check if we should run now (within 1 minute window)
        let next_run = cron.after(&local_now).next();
        
        Ok(next_run
            .map(|t| t <= local_now + chrono::Duration::minutes(1))
            .unwrap_or(false))
    }
}
```

### Resource Management

```rust
pub struct ResourceManager {
    /// Current cluster resources
    cluster_state: Arc<RwLock<ClusterResources>>,
    
    /// Resource allocations
    allocations: Arc<RwLock<HashMap<ExecutionId, ResourceAllocation>>>,
}

impl ResourceManager {
    pub async fn check_availability(
        &self,
        requirements: &ResourceRequirements,
    ) -> Result<bool> {
        let state = self.cluster_state.read().await;
        let allocations = self.allocations.read().await;
        
        // Calculate available resources
        let available = state.total_capacity - allocations.values()
            .map(|a| a.allocated_rps)
            .sum::<u64>();
        
        // Check RPS requirement
        if available < requirements.min_rps {
            return Ok(false);
        }
        
        // Check node requirement
        let available_nodes = state.active_nodes - allocations.len();
        if available_nodes < requirements.min_nodes as usize {
            return Ok(false);
        }
        
        // Check region requirements
        if let Some(regions) = &requirements.regions {
            let available_regions: HashSet<_> = state.nodes
                .iter()
                .filter(|n| !allocations.contains_key(&n.id))
                .map(|n| &n.region)
                .collect();
            
            if !regions.iter().all(|r| available_regions.contains(r)) {
                return Ok(false);
            }
        }
        
        Ok(true)
    }
    
    pub async fn allocate(
        &self,
        execution_id: ExecutionId,
        requirements: &ResourceRequirements,
    ) -> Result<ResourceAllocation> {
        let mut allocations = self.allocations.write().await;
        let state = self.cluster_state.read().await;
        
        // Select nodes based on requirements
        let selected_nodes = self.select_nodes(&state, requirements)?;
        
        let allocation = ResourceAllocation {
            execution_id: execution_id.clone(),
            nodes: selected_nodes,
            allocated_rps: requirements.preferred_rps,
            allocated_at: Utc::now(),
        };
        
        allocations.insert(execution_id, allocation.clone());
        
        Ok(allocation)
    }
}
```

### Dependency Resolution

```rust
pub struct DependencyResolver {
    state: Arc<RwLock<JobSchedulerState>>,
}

impl DependencyResolver {
    pub async fn check_dependencies(
        &self,
        job: &JobDefinition,
    ) -> Result<bool> {
        for dep in &job.scheduling.dependencies {
            if !self.is_dependency_satisfied(dep).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    
    async fn is_dependency_satisfied(
        &self,
        dep: &JobDependency,
    ) -> Result<bool> {
        let state = self.state.read().await;
        
        // Find most recent execution
        let execution = state.execution_history
            .values()
            .filter(|e| e.job_id == dep.job_id)
            .max_by_key(|e| e.started_at);
        
        let Some(exec) = execution else {
            return Ok(false); // Never ran
        };
        
        // Check timeout
        if exec.started_at + dep.timeout < Utc::now() {
            return Ok(false); // Dependency too old
        }
        
        // Check required outcome
        match &dep.required_outcome {
            DependencyOutcome::Success => {
                Ok(matches!(exec.state, ExecutionState::Succeeded))
            }
            DependencyOutcome::Completion => {
                Ok(matches!(
                    exec.state,
                    ExecutionState::Succeeded | ExecutionState::Failed { .. }
                ))
            }
            DependencyOutcome::MetricThreshold { metric, operator, value } => {
                if let Some(result) = &exec.result {
                    let metric_value = self.extract_metric(result, metric)?;
                    Ok(operator.compare(metric_value, *value))
                } else {
                    Ok(false)
                }
            }
        }
    }
}
```

## Integration with Distributed State

```rust
impl JobScheduler {
    /// Process state updates from gossip
    async fn process_state_updates(&self) -> Result<()> {
        // This is called periodically to handle remote state changes
        let mut state = self.state.write().await;
        
        // Check if another node has claimed active job
        if let Some(active) = state.active_job.read() {
            if active.assigned_to != self.node_id {
                // Another node is running a job
                self.metrics.remote_job_active.inc();
            }
        }
        
        // The CRDT queue automatically handles merges
        // Just need to react to state changes
        Ok(())
    }
    
    /// Publish our state changes
    async fn publish_state_change(&self) -> Result<()> {
        // State is automatically synchronized via gossip
        // Just increment version for differential sync
        self.state.write().await.increment_version();
        Ok(())
    }
}
```

## Testing

### Queue Convergence Test

```rust
#[tokio::test]
async fn test_distributed_queue_convergence() {
    // Create two scheduler instances
    let scheduler1 = create_test_scheduler("node1").await;
    let scheduler2 = create_test_scheduler("node2").await;
    
    // Add jobs to different nodes
    let job1 = create_test_job("job1", 10); // priority 10
    let job2 = create_test_job("job2", 5);  // priority 5
    
    scheduler1.queue_job(job1).await.unwrap();
    scheduler2.queue_job(job2).await.unwrap();
    
    // Simulate gossip merge
    let state1 = scheduler1.state.read().await;
    let state2 = scheduler2.state.read().await;
    
    let mut merged1 = state1.clone();
    merged1.merge(&state2);
    
    let mut merged2 = state2.clone();
    merged2.merge(&state1);
    
    // Both should have same queue contents
    assert_eq!(merged1.job_queue.len(), 2);
    assert_eq!(merged2.job_queue.len(), 2);
    
    // Both should dequeue in same order (high priority first)
    assert_eq!(merged1.job_queue.dequeue().unwrap().1.id, "job1");
    assert_eq!(merged2.job_queue.dequeue().unwrap().1.id, "job1");
}
```

This implementation provides a robust, distributed job scheduling system with CRDT-based queue management and comprehensive lifecycle control.