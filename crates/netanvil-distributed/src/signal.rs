//! External signal polling for the distributed leader.
//!
//! Polls an HTTP endpoint on the target server to extract a named metric
//! value (e.g., "load" from `{"load": 82.5}`). Used by the leader to
//! feed server-reported metrics into the PID rate controller.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Polls an HTTP endpoint for an external signal value.
pub struct HttpSignalPoller {
    url: String,
    field: String,
    timeout: Duration,
}

impl HttpSignalPoller {
    pub fn new(url: String, field: String, timeout: Duration) -> Self {
        Self {
            url,
            field,
            timeout,
        }
    }

    /// Create from optional config fields. Returns None if not configured.
    pub fn from_config(url: Option<&str>, field: Option<&str>) -> Option<Self> {
        match (url, field) {
            (Some(u), Some(f)) => Some(Self::new(u.into(), f.into(), Duration::from_secs(5))),
            _ => None,
        }
    }

    /// Poll the endpoint and return (field_name, value).
    pub fn poll(&self) -> Option<(String, f64)> {
        let body = self.http_get()?;
        let json: serde_json::Value = serde_json::from_str(&body).ok()?;
        let value = json.get(&self.field)?.as_f64()?;
        Some((self.field.clone(), value))
    }

    /// Create a closure suitable for `set_signal_source`.
    pub fn into_source(self) -> impl FnMut() -> Vec<(String, f64)> + Send {
        move || self.poll().into_iter().collect()
    }

    fn http_get(&self) -> Option<String> {
        // Parse host:port from URL
        let url = &self.url;
        let without_scheme = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, path) = without_scheme
            .split_once('/')
            .map(|(h, p)| (h, format!("/{p}")))
            .unwrap_or((without_scheme, "/".into()));

        let mut stream = TcpStream::connect(host_port).ok()?;
        stream.set_read_timeout(Some(self.timeout)).ok()?;
        stream.set_write_timeout(Some(self.timeout)).ok()?;

        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).ok()?;

        // Read response
        let mut reader = BufReader::new(&mut stream);
        let mut status = String::new();
        reader.read_line(&mut status).ok()?;

        // Skip headers
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).ok()?;
            if line.trim().is_empty() {
                break;
            }
        }

        // Read body
        let mut body = String::new();
        reader.read_to_string(&mut body).ok()?;
        Some(body)
    }
}
