//! Sequential test queue for the leader daemon.
//!
//! Maintains a queue of test specs, executes them one at a time via
//! `DistributedCoordinator`, and stores results to the `ResultStore`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use netanvil_types::{ControllerView, HoldCommand, NodeDiscovery, NodeInfo, TestConfig, TlsConfig};

use crate::coordinator::{
    ControllerUpdateCommand, DistributedProgressUpdate, DistributedTestResult,
};
use crate::leader_server::LeaderMetricsState;
use crate::result_store::{ResultStore, StoredTestEntry, StoredTestResult};
use crate::test_spec::{format_timestamp, TestId, TestInfo, TestSpec, TestStatus};

/// Shared state for the test queue, accessible from the HTTP server
/// and the scheduler thread.
pub struct TestQueue {
    inner: Arc<Mutex<QueueInner>>,
}

struct QueueInner {
    /// Tests waiting to run.
    pending: VecDeque<QueueEntry>,
    /// Currently running test (at most one).
    running: Option<RunningTest>,
    /// All test infos (for listing). Includes completed tests from the store.
    all_tests: Vec<TestInfo>,
    /// Result store for persistence.
    store: ResultStore,
}

struct QueueEntry {
    id: TestId,
    config: TestConfig,
    spec: Option<TestSpec>,
    summary: String,
    created_at: SystemTime,
}

struct RunningTest {
    id: TestId,
    config_summary: String,
    spec: Option<TestSpec>,
    created_at: SystemTime,
    started_at: SystemTime,
    /// Live progress from the coordinator.
    progress: Arc<Mutex<Option<DistributedProgressUpdate>>>,
    /// Send to cancel the running test.
    cancel_tx: flume::Sender<()>,
    /// Command sender for rate overrides on the running test's agents.
    rate_override_tx: flume::Sender<f64>,
    /// Signal sender for pushing external signals during a running test.
    signal_tx: flume::Sender<(String, f64)>,
    /// Hold/release command sender.
    hold_tx: flume::Sender<HoldCommand>,
    /// Controller parameter update sender.
    controller_update_tx: flume::Sender<ControllerUpdateCommand>,
    /// Controller introspection request sender.
    controller_info_tx: flume::Sender<flume::Sender<ControllerView>>,
}

/// Configuration for the test queue scheduler.
pub struct QueueConfig {
    /// Agent addresses for the distributed coordinator.
    pub workers: Vec<String>,
    /// Optional mTLS configuration for agent communication.
    pub tls: Option<TlsConfig>,
    /// External metrics URL (optional).
    pub external_metrics_url: Option<String>,
    /// External metrics field name (optional).
    pub external_metrics_field: Option<String>,
    /// Shared agent list (updated after discovery, read by GET /agents).
    pub agents: Arc<Mutex<Vec<NodeInfo>>>,
    /// Leader Prometheus metrics state (updated by progress callback).
    pub leader_metrics: LeaderMetricsState,
}

impl Clone for TestQueue {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl TestQueue {
    /// Create a new test queue backed by the given result store.
    pub fn new(store: ResultStore) -> Self {
        let all_tests = store.load_index();
        Self {
            inner: Arc::new(Mutex::new(QueueInner {
                pending: VecDeque::new(),
                running: None,
                all_tests,
                store,
            })),
        }
    }

    /// Enqueue a test from a `TestSpec` (API path).
    /// Converts to `TestConfig` immediately so validation happens at enqueue time.
    pub fn enqueue(&self, spec: TestSpec) -> Result<TestId, String> {
        let config = spec.clone().into_config().map_err(|e| e.to_string())?;
        let summary = spec.summary();
        Ok(self.enqueue_inner(config, Some(spec), summary))
    }

    /// Enqueue a test from a pre-built `TestConfig` (one-shot CLI path).
    pub fn enqueue_config(&self, config: TestConfig, summary: String) -> TestId {
        self.enqueue_inner(config, None, summary)
    }

    fn enqueue_inner(&self, config: TestConfig, spec: Option<TestSpec>, summary: String) -> TestId {
        let id = TestId::generate();
        let now = SystemTime::now();

        let info = TestInfo {
            id: id.clone(),
            status: TestStatus::Queued,
            created_at: format_timestamp(now),
            started_at: None,
            completed_at: None,
            config_summary: summary,
        };

        let mut inner = self.inner.lock().unwrap();
        inner.pending.push_back(QueueEntry {
            id: id.clone(),
            config,
            spec: spec.clone(),
            summary: info.config_summary.clone(),
            created_at: now,
        });
        inner.all_tests.push(info.clone());

        // Persist the queued entry (only if we have a spec for serialization).
        if let Some(spec) = spec {
            let entry = StoredTestEntry {
                info,
                spec,
                result: None,
            };
            let _ = inner.store.save(&entry);
        }

        id
    }

    /// List all tests (queued, running, completed).
    pub fn list(&self) -> Vec<TestInfo> {
        self.inner.lock().unwrap().all_tests.clone()
    }

    /// Get info for a specific test.
    pub fn get_info(&self, id: &TestId) -> Option<TestInfo> {
        self.inner
            .lock()
            .unwrap()
            .all_tests
            .iter()
            .find(|i| i.id == *id)
            .cloned()
    }

    /// Get the full stored entry (including result) for a test.
    pub fn get_entry(&self, id: &TestId) -> Option<StoredTestEntry> {
        self.inner.lock().unwrap().store.load_entry(id)
    }

    /// Get live progress for the currently running test.
    pub fn get_progress(&self, id: &TestId) -> Option<DistributedProgressUpdate> {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                return running.progress.lock().unwrap().clone();
            }
        }
        None
    }

    /// Send a rate override to the currently running test.
    /// Returns false if the test is not running or the channel is closed.
    pub fn send_rate_override(&self, id: &TestId, rps: f64) -> bool {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                return running.rate_override_tx.try_send(rps).is_ok();
            }
        }
        false
    }

    /// Send a hold command to the currently running test.
    pub fn send_hold(&self, id: &TestId, rps: f64) -> bool {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                return running.hold_tx.try_send(HoldCommand::Hold(rps)).is_ok();
            }
        }
        false
    }

    /// Send a release command to the currently running test.
    pub fn send_release(&self, id: &TestId) -> bool {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                return running.hold_tx.try_send(HoldCommand::Release).is_ok();
            }
        }
        false
    }

    /// Send a controller parameter update and wait for the response.
    /// Returns None if the test is not running.
    pub fn send_controller_update(
        &self,
        id: &TestId,
        action: String,
        params: serde_json::Value,
    ) -> Option<Result<serde_json::Value, String>> {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                let (response_tx, response_rx) = flume::bounded(1);
                let cmd = ControllerUpdateCommand {
                    action,
                    params,
                    response_tx,
                };
                if running.controller_update_tx.try_send(cmd).is_ok() {
                    // Drop the lock before blocking on the response.
                    drop(inner);
                    return match response_rx.recv_timeout(std::time::Duration::from_secs(10)) {
                        Ok(result) => Some(result),
                        Err(_) => Some(Err("timeout waiting for coordinator".into())),
                    };
                }
            }
        }
        None
    }

    /// Request controller introspection info.
    /// Returns None if the test is not running.
    pub fn get_controller_info(&self, id: &TestId) -> Option<ControllerView> {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                let (response_tx, response_rx) = flume::bounded(1);
                if running.controller_info_tx.try_send(response_tx).is_ok() {
                    drop(inner);
                    return response_rx
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .ok();
                }
            }
        }
        None
    }

    /// Push an external signal to the currently running test.
    /// Returns false if the test is not running or the channel is closed.
    pub fn send_signal(&self, id: &TestId, name: String, value: f64) -> bool {
        let inner = self.inner.lock().unwrap();
        if let Some(ref running) = inner.running {
            if running.id == *id {
                return running.signal_tx.try_send((name, value)).is_ok();
            }
        }
        false
    }

    /// Cancel a queued or running test.
    pub fn cancel(&self, id: &TestId) -> CancelResult {
        let mut inner = self.inner.lock().unwrap();

        // Check pending queue.
        if let Some(pos) = inner.pending.iter().position(|e| e.id == *id) {
            let entry = inner.pending.remove(pos).unwrap();
            let now_str = format_timestamp(SystemTime::now());
            if let Some(info) = inner.all_tests.iter_mut().find(|i| i.id == *id) {
                info.status = TestStatus::Cancelled;
                info.completed_at = Some(now_str.clone());
            }
            // Persist the cancellation (only if we have a spec).
            if let Some(spec) = entry.spec {
                let info = TestInfo {
                    id: id.clone(),
                    status: TestStatus::Cancelled,
                    created_at: format_timestamp(entry.created_at),
                    started_at: None,
                    completed_at: Some(now_str),
                    config_summary: entry.summary,
                };
                let stored = StoredTestEntry {
                    info,
                    spec,
                    result: None,
                };
                let _ = inner.store.save(&stored);
            }
            return CancelResult::Cancelled;
        }

        // Check running test.
        if let Some(ref running) = inner.running {
            if running.id == *id {
                let _ = running.cancel_tx.send(());
                return CancelResult::Stopping;
            }
        }

        // Check if already completed.
        if let Some(info) = inner.all_tests.iter().find(|i| i.id == *id) {
            if matches!(
                info.status,
                TestStatus::Completed | TestStatus::Failed | TestStatus::Cancelled
            ) {
                return CancelResult::AlreadyDone;
            }
        }

        CancelResult::NotFound
    }

    /// Try to start the next queued test. Called by the scheduler thread.
    /// Returns None if the queue is empty or a test is already running.
    #[allow(clippy::type_complexity)]
    fn try_dequeue(
        &self,
    ) -> Option<(
        TestId,
        TestConfig,
        String, // config_summary
        Arc<Mutex<Option<DistributedProgressUpdate>>>,
        flume::Receiver<()>,                            // cancel
        flume::Receiver<f64>,                           // rate override (deprecated)
        flume::Receiver<(String, f64)>,                 // signal push
        flume::Receiver<HoldCommand>,                   // hold/release
        flume::Receiver<ControllerUpdateCommand>,       // controller update
        flume::Receiver<flume::Sender<ControllerView>>, // controller info
    )> {
        let mut inner = self.inner.lock().unwrap();

        if inner.running.is_some() {
            return None;
        }

        let entry = inner.pending.pop_front()?;
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        let (rate_tx, rate_rx) = flume::bounded(4);
        let (signal_tx, signal_rx) = flume::bounded(16);
        let (hold_tx, hold_rx) = flume::bounded(4);
        let (ctrl_update_tx, ctrl_update_rx) = flume::bounded(4);
        let (ctrl_info_tx, ctrl_info_rx) = flume::bounded(4);
        let progress = Arc::new(Mutex::new(None));

        let now = SystemTime::now();
        let running = RunningTest {
            id: entry.id.clone(),
            config_summary: entry.summary.clone(),
            spec: entry.spec,
            created_at: entry.created_at,
            started_at: now,
            progress: progress.clone(),
            cancel_tx,
            rate_override_tx: rate_tx,
            signal_tx,
            hold_tx,
            controller_update_tx: ctrl_update_tx,
            controller_info_tx: ctrl_info_tx,
        };
        inner.running = Some(running);

        // Update status in all_tests.
        if let Some(info) = inner.all_tests.iter_mut().find(|i| i.id == entry.id) {
            info.status = TestStatus::Running;
            info.started_at = Some(format_timestamp(now));
        }

        Some((
            entry.id,
            entry.config,
            entry.summary,
            progress,
            cancel_rx,
            rate_rx,
            signal_rx,
            hold_rx,
            ctrl_update_rx,
            ctrl_info_rx,
        ))
    }

    /// Mark a test as completed with results. Called by the scheduler thread.
    fn mark_completed(&self, id: &TestId, result: &DistributedTestResult, status: TestStatus) {
        let mut inner = self.inner.lock().unwrap();
        let now = SystemTime::now();

        // Clear running state.
        if inner.running.as_ref().map(|r| &r.id) == Some(id) {
            let running = inner.running.take().unwrap();

            // Update all_tests.
            if let Some(info) = inner.all_tests.iter_mut().find(|i| i.id == *id) {
                info.status = status;
                info.completed_at = Some(format_timestamp(now));
            }

            // Persist result (only if we have a spec for serialization).
            if let Some(spec) = running.spec {
                let stored_result = StoredTestResult::from_distributed(result);
                let info = TestInfo {
                    id: id.clone(),
                    status,
                    created_at: format_timestamp(running.created_at),
                    started_at: Some(format_timestamp(running.started_at)),
                    completed_at: Some(format_timestamp(now)),
                    config_summary: running.config_summary,
                };
                let entry = StoredTestEntry {
                    info,
                    spec,
                    result: Some(stored_result),
                };
                let _ = inner.store.save(&entry);
            }
        }
    }

    /// Run the scheduler loop. Async — runs on the shared tokio runtime.
    /// Processes queued tests one at a time, yielding between polls
    /// so the API server can handle requests on the same runtime.
    pub async fn run_scheduler(&self, config: QueueConfig) {
        loop {
            let dequeued = self.try_dequeue();

            match dequeued {
                None => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Some((
                    id,
                    test_config,
                    _summary,
                    progress,
                    cancel_rx,
                    rate_rx,
                    signal_rx,
                    hold_rx,
                    ctrl_update_rx,
                    ctrl_info_rx,
                )) => {
                    tracing::info!(test_id = %id, "starting queued test");
                    tracing::debug!(test_id = %id, config = ?test_config, "applying test config");

                    let result = self
                        .run_test(
                            &test_config,
                            &config,
                            progress,
                            cancel_rx,
                            rate_rx,
                            signal_rx,
                            hold_rx,
                            ctrl_update_rx,
                            ctrl_info_rx,
                        )
                        .await;

                    match result {
                        Ok(r) => {
                            tracing::info!(
                                test_id = %id,
                                total_requests = r.total_requests,
                                total_errors = r.total_errors,
                                "test completed"
                            );
                            self.mark_completed(&id, &r, TestStatus::Completed);
                        }
                        Err(e) => {
                            tracing::error!(test_id = %id, error = %e, "test failed");
                            let empty = DistributedTestResult {
                                total_requests: 0,
                                total_errors: 0,
                                duration: std::time::Duration::ZERO,
                                nodes: vec![],
                            };
                            self.mark_completed(&id, &empty, TestStatus::Failed);
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_test(
        &self,
        test_config: &TestConfig,
        config: &QueueConfig,
        progress: Arc<Mutex<Option<DistributedProgressUpdate>>>,
        cancel_rx: flume::Receiver<()>,
        rate_rx: flume::Receiver<f64>,
        signal_rx: flume::Receiver<(String, f64)>,
        hold_rx: flume::Receiver<HoldCommand>,
        ctrl_update_rx: flume::Receiver<ControllerUpdateCommand>,
        ctrl_info_rx: flume::Receiver<flume::Sender<ControllerView>>,
    ) -> Result<DistributedTestResult, String> {
        let rate_controller = netanvil_core::build_rate_controller(
            &test_config.rate,
            test_config.control_interval,
            std::time::Instant::now(),
            test_config.duration,
            netanvil_core::clock::system_clock(),
        );

        let result = if let Some(ref tls) = config.tls {
            let discovery = crate::MtlsStaticDiscovery::new(config.workers.clone(), tls)
                .await
                .map_err(|e| format!("mTLS discovery: {e}"))?;

            // Populate agent list from discovery for GET /agents.
            let nodes = discovery.discover().await;
            *config.agents.lock().unwrap() = nodes;

            let fetcher =
                crate::MtlsMetricsFetcher::new(tls).map_err(|e| format!("mTLS fetcher: {e}"))?;
            let commander =
                crate::MtlsNodeCommander::new(tls).map_err(|e| format!("mTLS commander: {e}"))?;
            let mut coordinator = crate::DistributedCoordinator::new(
                discovery,
                fetcher,
                commander,
                test_config.clone(),
                rate_controller,
            );
            Self::configure_and_run(
                &mut coordinator,
                config,
                progress,
                cancel_rx,
                rate_rx,
                signal_rx,
                hold_rx,
                ctrl_update_rx,
                ctrl_info_rx,
            )
            .await
        } else {
            let discovery = crate::StaticDiscovery::new(config.workers.clone()).await;

            // Populate agent list from discovery for GET /agents.
            let nodes = discovery.discover().await;
            *config.agents.lock().unwrap() = nodes;

            let fetcher = crate::HttpMetricsFetcher::new(std::time::Duration::from_secs(5));
            let commander = crate::HttpNodeCommander::new(std::time::Duration::from_secs(10));
            let mut coordinator = crate::DistributedCoordinator::new(
                discovery,
                fetcher,
                commander,
                test_config.clone(),
                rate_controller,
            );
            Self::configure_and_run(
                &mut coordinator,
                config,
                progress,
                cancel_rx,
                rate_rx,
                signal_rx,
                hold_rx,
                ctrl_update_rx,
                ctrl_info_rx,
            )
            .await
        };

        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    async fn configure_and_run<D, M, C>(
        coordinator: &mut crate::DistributedCoordinator<D, M, C>,
        config: &QueueConfig,
        progress: Arc<Mutex<Option<DistributedProgressUpdate>>>,
        cancel_rx: flume::Receiver<()>,
        rate_rx: flume::Receiver<f64>,
        signal_rx: flume::Receiver<(String, f64)>,
        hold_rx: flume::Receiver<HoldCommand>,
        ctrl_update_rx: flume::Receiver<ControllerUpdateCommand>,
        ctrl_info_rx: flume::Receiver<flume::Sender<ControllerView>>,
    ) -> DistributedTestResult
    where
        D: netanvil_types::NodeDiscovery,
        M: netanvil_types::MetricsFetcher,
        C: netanvil_types::NodeCommander,
    {
        if let Some(poller) = crate::HttpSignalPoller::from_config(
            config.external_metrics_url.as_deref(),
            config.external_metrics_field.as_deref(),
        ) {
            coordinator.set_signal_source(poller.into_source());
        }

        coordinator.set_cancel_rx(cancel_rx);

        // Wire up rate override channel (deprecated — translates to hold).
        coordinator.set_rate_override_rx(rate_rx);
        coordinator.set_signal_push_rx(signal_rx);

        // Wire up new hold/controller channels.
        coordinator.set_hold_command_rx(hold_rx);
        coordinator.set_controller_update_rx(ctrl_update_rx);
        coordinator.set_controller_info_rx(ctrl_info_rx);

        // Progress callback updates both the per-test progress Arc
        // and the leader's Prometheus metrics state.
        let leader_metrics = config.leader_metrics.clone();
        coordinator.on_progress(move |update| {
            *progress.lock().unwrap() = Some(update.clone());
            leader_metrics.update(update);
        });

        coordinator.run().await
    }
}

/// Result of attempting to cancel a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    /// Test was queued and has been removed.
    Cancelled,
    /// Test is running; stop signal sent (will complete shortly).
    Stopping,
    /// Test has already completed/failed/cancelled.
    AlreadyDone,
    /// No test with this ID.
    NotFound,
}
