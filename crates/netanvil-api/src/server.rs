use netanvil_types::WorkerCommand;

use crate::handlers;
use crate::types::SharedState;

/// Handle to a running control server. Drop to shut down.
pub struct ControlServerHandle {
    _thread: std::thread::JoinHandle<()>,
}

/// HTTP control server for a running load test.
///
/// Runs on a background `std::thread` using `tiny_http` (synchronous,
/// no async runtime). Communicates with the coordinator via:
/// - `flume::Sender<WorkerCommand>` for injecting commands
/// - `SharedState` (Arc<Mutex>) for reading live metrics
pub struct ControlServer {
    server: tiny_http::Server,
    shared_state: SharedState,
    command_tx: flume::Sender<WorkerCommand>,
}

impl ControlServer {
    /// Create a new control server listening on the given port.
    pub fn new(
        port: u16,
        shared_state: SharedState,
        command_tx: flume::Sender<WorkerCommand>,
    ) -> std::io::Result<Self> {
        let addr = format!("0.0.0.0:{port}");
        let server = tiny_http::Server::http(&addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e.to_string()))?;
        Ok(Self {
            server,
            shared_state,
            command_tx,
        })
    }

    /// Spawn the server on a background thread.
    pub fn spawn(self) -> ControlServerHandle {
        let thread = std::thread::Builder::new()
            .name("netanvil-api".into())
            .spawn(move || self.run())
            .expect("spawn API server thread");
        ControlServerHandle { _thread: thread }
    }

    fn run(self) {
        tracing::info!("Control API listening on {}", self.server.server_addr());
        loop {
            let request = match self.server.recv() {
                Ok(req) => req,
                Err(_) => break, // server shut down
            };
            self.handle_request(request);
        }
    }

    fn handle_request(&self, request: tiny_http::Request) {
        let method = request.method().as_str().to_uppercase();
        let path = request.url().to_string();

        tracing::debug!("{method} {path}");

        match (method.as_str(), path.as_str()) {
            ("GET", "/status") => handlers::handle_get_status(request, &self.shared_state),
            ("GET", "/metrics") => handlers::handle_get_metrics(request, &self.shared_state),
            ("GET", "/metrics/prometheus") => {
                handlers::handle_get_metrics_prometheus(request, &self.shared_state)
            }
            ("PUT", "/rate") => handlers::handle_put_rate(request, &self.command_tx),
            ("PUT", "/targets") => handlers::handle_put_targets(request, &self.command_tx),
            ("PUT", "/headers") => handlers::handle_put_headers(request, &self.command_tx),
            ("PUT", "/signal") => handlers::handle_put_signal(request, &self.shared_state),
            ("POST", "/stop") => handlers::handle_post_stop(request, &self.command_tx),
            _ => handlers::handle_not_found(request),
        }
    }
}
