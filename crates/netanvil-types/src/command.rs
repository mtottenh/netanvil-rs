/// Commands sent from coordinator to worker via channel.
#[derive(Debug, Clone)]
pub enum WorkerCommand {
    /// Update this worker's target request rate.
    UpdateRate(f64),
    /// Gracefully stop the worker.
    Stop,
}
