/// Commands sent from coordinator to worker via channel.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    /// Update this worker's target request rate.
    UpdateRate(f64),
    /// Replace the target URL list mid-test.
    UpdateTargets(Vec<String>),
    /// Replace the header list mid-test.
    UpdateHeaders(Vec<(String, String)>),
    /// Gracefully stop the worker.
    Stop,
}

/// Commands sent from coordinator to the timer thread.
///
/// The timer thread acts as a command multiplexer: rate commands are
/// processed locally (distributed to per-worker schedulers), while
/// target/header/stop commands are forwarded to I/O workers via fire channels.
#[derive(Debug, Clone)]
pub enum TimerCommand {
    /// Update the aggregate target rate. Timer thread distributes to its schedulers.
    UpdateRate(f64),
    /// Forward target URL updates to I/O workers.
    UpdateTargets(Vec<String>),
    /// Forward header updates to I/O workers.
    UpdateHeaders(Vec<(String, String)>),
    /// Gracefully stop all I/O workers and the timer thread.
    Stop,
}

/// Messages sent from the timer thread to I/O workers via bounded fire channels.
///
/// The `Fire` variant is the hot-path message (one per request). Other variants
/// are control messages forwarded from the coordinator through the timer thread.
#[derive(Debug, Clone)]
pub enum ScheduledRequest {
    /// Fire a request at the given intended send time.
    Fire(std::time::Instant),
    /// Replace the target URL list.
    UpdateTargets(Vec<String>),
    /// Replace the header list.
    UpdateHeaders(Vec<(String, String)>),
    /// Gracefully stop this worker.
    Stop,
}
