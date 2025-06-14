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
