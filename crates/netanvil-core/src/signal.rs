use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Polls an external HTTP endpoint for a JSON metric value.
///
/// Used for server-side metrics like proxy load, queue depth, etc.
/// The endpoint must return a JSON object; we extract the named field.
pub struct HttpSignalPoller {
    url: String,
    field: String,
    host: String,
    addr: String,
    path: String,
    timeout: Duration,
}

impl HttpSignalPoller {
    pub fn new(url: &str, field: &str) -> Option<Self> {
        // Parse "http://host:port/path" minimally
        let url = url.trim();
        let without_scheme = url.strip_prefix("http://")?;
        let (host_port, path) = match without_scheme.find('/') {
            Some(i) => (&without_scheme[..i], &without_scheme[i..]),
            None => (without_scheme, "/"),
        };

        Some(Self {
            url: url.to_string(),
            field: field.to_string(),
            host: host_port.to_string(),
            addr: host_port.to_string(),
            path: path.to_string(),
            timeout: Duration::from_secs(2),
        })
    }

    /// Poll the endpoint and return the extracted signal value.
    pub fn poll(&self) -> Option<(String, f64)> {
        tracing::debug!(url = %self.url, field = %self.field, "polling external signal");
        let mut stream = TcpStream::connect(&self.addr).ok().or_else(|| {
            tracing::warn!(addr = %self.addr, "failed to connect to external metrics endpoint");
            None
        })?;
        stream.set_read_timeout(Some(self.timeout)).ok()?;
        stream.set_write_timeout(Some(self.timeout)).ok()?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.path, self.host,
        );
        stream.write_all(request.as_bytes()).ok()?;

        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;

        // Extract body after headers
        let body = response.split("\r\n\r\n").nth(1)?;

        // Parse JSON and extract field
        let json: serde_json::Value = serde_json::from_str(body).ok().or_else(|| {
            tracing::warn!(body = %body, "failed to parse external metrics JSON");
            None
        })?;
        let value = json.get(&self.field)?.as_f64()?;

        tracing::debug!(field = %self.field, value, "external signal polled");
        Some((self.field.clone(), value))
    }
}

/// Create a signal source closure from TestConfig fields.
pub fn make_signal_source(url: Option<&str>, field: Option<&str>) -> Option<crate::SignalSourceFn> {
    let url = url?;
    let field = field?;
    let poller = HttpSignalPoller::new(url, field)?;

    Some(Box::new(move || match poller.poll() {
        Some(signal) => vec![signal],
        None => {
            tracing::warn!("failed to poll external signal from {}", poller.url);
            Vec::new()
        }
    }))
}
