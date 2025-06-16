use netanvil_types::MetricsSnapshot;

/// Coordinator's view of an I/O worker in the timer thread architecture.
///
/// Commands are routed through the timer thread which forwards them via
/// the fire channel. The coordinator only needs the metrics channel for
/// collecting snapshots.
pub struct IoWorkerHandle {
    pub metrics_rx: flume::Receiver<MetricsSnapshot>,
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub core_id: usize,
}
