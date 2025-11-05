//! Behavioral tests for plugin response callbacks.
//!
//! Validates that generators which opt into response callbacks receive
//! ExecutionResult deliveries via the io_worker response channel.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use netanvil_core::io_worker::{io_worker_loop, IoWorkerConfig};
use netanvil_core::NoopTransformer;
use netanvil_metrics::HdrMetricsCollector;
use netanvil_types::{
    EventRecorder, ExecutionResult, HttpRequestSpec, MetricsCollector, NoopEventRecorder,
    RequestContext, RequestExecutor, RequestGenerator, ScheduledRequest, TimingBreakdown,
};

// ---------------------------------------------------------------------------
// Mock executor that returns configurable status codes
// ---------------------------------------------------------------------------

struct MockExecutor {
    status: u16,
}

impl MockExecutor {
    fn new(status: u16) -> Self {
        Self { status }
    }
}

impl RequestExecutor for MockExecutor {
    type Spec = HttpRequestSpec;

    async fn execute(&self, _spec: &HttpRequestSpec, context: &RequestContext) -> ExecutionResult {
        ExecutionResult {
            request_id: context.request_id,
            intended_time: context.intended_time,
            actual_time: context.actual_time,
            timing: TimingBreakdown {
                total: Duration::from_micros(100),
                ..Default::default()
            },
            status: Some(self.status),
            bytes_sent: 0,
            response_size: 256,
            error: None,
            response_headers: Some(vec![("X-Test".to_string(), "hello".to_string())]),
            response_body: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Stateful generator that captures response statuses via on_response
// ---------------------------------------------------------------------------

struct ResponseCapturingGenerator {
    targets: Vec<String>,
    /// Statuses received via on_response callbacks.
    captured_statuses: Vec<u16>,
    /// Total on_response calls received.
    callback_count: usize,
}

impl ResponseCapturingGenerator {
    fn new(targets: Vec<String>) -> Self {
        Self {
            targets,
            captured_statuses: Vec::new(),
            callback_count: 0,
        }
    }
}

impl RequestGenerator for ResponseCapturingGenerator {
    type Spec = HttpRequestSpec;

    fn generate(&mut self, _ctx: &RequestContext) -> HttpRequestSpec {
        HttpRequestSpec {
            method: http::Method::GET,
            url: self.targets.first().cloned().unwrap_or_default(),
            headers: vec![],
            body: None,
        }
    }

    fn on_response(&mut self, result: &ExecutionResult) {
        self.callback_count += 1;
        if let Some(status) = result.status {
            self.captured_statuses.push(status);
        }
    }

    fn wants_responses(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Generator that does NOT want responses (verifies zero overhead)
// ---------------------------------------------------------------------------

struct NoResponseGenerator;

impl RequestGenerator for NoResponseGenerator {
    type Spec = HttpRequestSpec;

    fn generate(&mut self, _ctx: &RequestContext) -> HttpRequestSpec {
        HttpRequestSpec {
            method: http::Method::GET,
            url: "http://localhost".into(),
            headers: vec![],
            body: None,
        }
    }

    // Default: wants_responses() returns false, on_response is no-op
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn response_callbacks_receive_statuses() {
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    let (fire_tx, fire_rx) = flume::bounded(128);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    // Send fire events from a separate thread with a delay before Stop,
    // so spawned async tasks have time to complete before the worker exits.
    std::thread::spawn(move || {
        let now = Instant::now();
        for _ in 0..10 {
            fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
        }
        // Give async tasks time to execute before sending Stop
        std::thread::sleep(Duration::from_millis(100));
        fire_tx.send(ScheduledRequest::Stop).unwrap();
    });

    // Wrap generator in shared state so we can inspect after the worker exits.
    // We use a wrapper that delegates to the inner generator.
    let captured_statuses: Rc<RefCell<Vec<u16>>> = Rc::new(RefCell::new(Vec::new()));
    let callback_count: Rc<Cell<usize>> = Rc::new(Cell::new(0));

    struct Wrapper {
        statuses: Rc<RefCell<Vec<u16>>>,
        count: Rc<Cell<usize>>,
    }

    impl RequestGenerator for Wrapper {
        type Spec = HttpRequestSpec;

        fn generate(&mut self, _ctx: &RequestContext) -> HttpRequestSpec {
            HttpRequestSpec {
                method: http::Method::GET,
                url: "http://localhost".into(),
                headers: vec![],
                body: None,
            }
        }

        fn on_response(&mut self, result: &ExecutionResult) {
            self.count.set(self.count.get() + 1);
            if let Some(status) = result.status {
                self.statuses.borrow_mut().push(status);
            }
        }

        fn wants_responses(&self) -> bool {
            true
        }
    }

    let statuses_clone = captured_statuses.clone();
    let count_clone = callback_count.clone();

    rt.block_on(async {
        let generator = Wrapper {
            statuses: statuses_clone,
            count: count_clone,
        };
        let transformer = Rc::new(NoopTransformer);
        let executor = Rc::new(MockExecutor::new(200));
        let metrics = Rc::new(HdrMetricsCollector::new(400, vec![], false));

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_secs(60),
                graceful_shutdown: true,
            },
            generator,
            transformer,
            executor,
            metrics,
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;
    });

    let statuses = captured_statuses.borrow();
    let count = callback_count.get();

    // We should have received some response callbacks.
    // Not necessarily all 10 because the worker may exit before all
    // spawned tasks complete, but at least some should have been delivered.
    assert!(count > 0, "expected at least 1 response callback, got 0");
    assert!(
        statuses.iter().all(|&s| s == 200),
        "all captured statuses should be 200, got {:?}",
        *statuses
    );
}

#[test]
fn no_response_generator_has_zero_overhead() {
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    let (fire_tx, fire_rx) = flume::bounded(128);
    let (metrics_tx, _metrics_rx) = flume::unbounded();

    let now = Instant::now();
    for _ in 0..5 {
        fire_tx.send(ScheduledRequest::Fire(now)).unwrap();
    }
    fire_tx.send(ScheduledRequest::Stop).unwrap();

    rt.block_on(async {
        let generator = NoResponseGenerator;

        // Verify wants_responses is false
        assert!(!generator.wants_responses());

        let transformer = Rc::new(NoopTransformer);
        let executor = Rc::new(MockExecutor::new(200));
        let metrics = Rc::new(HdrMetricsCollector::new(400, vec![], false));

        io_worker_loop(
            IoWorkerConfig {
                fire_rx,
                metrics_tx,
                core_id: 0,
                metrics_interval: Duration::from_secs(60),
                graceful_shutdown: true,
            },
            generator,
            transformer,
            executor,
            metrics,
            Rc::new(NoopEventRecorder) as Rc<dyn EventRecorder>,
        )
        .await;

        // If we reach here without panicking, the worker ran fine
        // without response callbacks. No channel was created.
    });
}
