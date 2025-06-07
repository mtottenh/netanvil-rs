use netanvil_types::{MetricsSnapshot, WorkerCommand};

/// Coordinator's view of a worker. Type-erased — the coordinator doesn't
/// know the worker's generic parameters `<S, G, T, E, M>`.
pub struct WorkerHandle {
    pub command_tx: flume::Sender<WorkerCommand>,
    pub metrics_rx: flume::Receiver<MetricsSnapshot>,
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub core_id: usize,
}
