//! HTTP-based implementations of the three distributed traits.
//!
//! MVP implementations using raw TCP HTTP requests to communicate with
//! agent nodes. No external HTTP client dependency needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netanvil_types::{
    MetricsFetcher, NetAnvilError, NodeCommander, NodeDiscovery, NodeId, NodeInfo, NodeState,
    RemoteMetrics, TestConfig, TlsConfig,
};

/// Discovers nodes from a static list provided at construction time.
/// Probes each agent's `GET /info` on first call to populate core counts.
pub struct StaticDiscovery {
    nodes: Mutex<Vec<NodeInfo>>,
    failed: Mutex<Vec<NodeId>>,
}

impl StaticDiscovery {
    /// Create from a list of "host:port" addresses.
    /// Probes each agent for node info.
    pub fn new(addrs: Vec<String>) -> Self {
        let mut nodes = Vec::new();
        for addr in &addrs {
            match probe_node_info(addr) {
                Some(info) => {
                    tracing::info!(id = %info.id, cores = info.cores, "discovered agent at {addr}");
                    nodes.push(info);
                }
                None => {
                    tracing::warn!("failed to probe agent at {addr}, adding with defaults");
                    nodes.push(NodeInfo {
                        id: NodeId(addr.clone()),
                        addr: addr.clone(),
                        cores: 1,
                        state: NodeState::Idle,
                    });
                }
            }
        }
        Self {
            nodes: Mutex::new(nodes),
            failed: Mutex::new(Vec::new()),
        }
    }
}

impl NodeDiscovery for StaticDiscovery {
    fn discover(&self) -> Vec<NodeInfo> {
        let failed = self.failed.lock().unwrap();
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| !failed.contains(&n.id))
            .cloned()
            .collect()
    }

    fn mark_failed(&self, id: &NodeId) {
        let mut failed = self.failed.lock().unwrap();
        if !failed.contains(id) {
            tracing::warn!(node = %id, "marking node as failed");
            failed.push(id.clone());
        }
    }
}

/// Fetches metrics from agents via `GET /metrics`.
pub struct HttpMetricsFetcher {
    timeout: Duration,
}

impl HttpMetricsFetcher {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl MetricsFetcher for HttpMetricsFetcher {
    fn fetch_metrics(&self, node: &NodeInfo) -> Option<RemoteMetrics> {
        let body = http_get(&node.addr, "/metrics", self.timeout)?;

        #[derive(serde::Deserialize)]
        struct MetricsResp {
            current_rps: Option<f64>,
            target_rps: Option<f64>,
            total_requests: Option<u64>,
            total_errors: Option<u64>,
            error_rate: Option<f64>,
            latency_p50_ms: Option<f64>,
            latency_p90_ms: Option<f64>,
            latency_p99_ms: Option<f64>,
        }

        let resp: MetricsResp = serde_json::from_str(&body).ok()?;

        Some(RemoteMetrics {
            node_id: node.id.clone(),
            current_rps: resp.current_rps.unwrap_or(0.0),
            target_rps: resp.target_rps.unwrap_or(0.0),
            total_requests: resp.total_requests.unwrap_or(0),
            total_errors: resp.total_errors.unwrap_or(0),
            error_rate: resp.error_rate.unwrap_or(0.0),
            latency_p50_ms: resp.latency_p50_ms.unwrap_or(0.0),
            latency_p90_ms: resp.latency_p90_ms.unwrap_or(0.0),
            latency_p99_ms: resp.latency_p99_ms.unwrap_or(0.0),
        })
    }
}

/// Sends commands to agents via HTTP.
pub struct HttpNodeCommander {
    timeout: Duration,
}

impl HttpNodeCommander {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl NodeCommander for HttpNodeCommander {
    fn start_test(&self, node: &NodeInfo, config: &TestConfig) -> Result<(), NetAnvilError> {
        let body = serde_json::to_string(config)
            .map_err(|e| NetAnvilError::Other(format!("serialize config: {e}")))?;
        http_post(&node.addr, "/test/start", &body, self.timeout)
            .ok_or_else(|| NetAnvilError::Other(format!("failed to start test on {}", node.id)))?;
        Ok(())
    }

    fn set_rate(&self, node: &NodeInfo, rps: f64) -> Result<(), NetAnvilError> {
        let body = format!(r#"{{"rps":{rps}}}"#);
        http_put(&node.addr, "/rate", &body, self.timeout)
            .ok_or_else(|| NetAnvilError::Other(format!("failed to set rate on {}", node.id)))?;
        Ok(())
    }

    fn stop_test(&self, node: &NodeInfo) -> Result<(), NetAnvilError> {
        http_post(&node.addr, "/stop", "", self.timeout)
            .ok_or_else(|| NetAnvilError::Other(format!("failed to stop test on {}", node.id)))?;
        Ok(())
    }
}

// --- Raw HTTP helpers ---

fn probe_node_info(addr: &str) -> Option<NodeInfo> {
    let body = http_get(addr, "/info", Duration::from_secs(5))?;
    serde_json::from_str(&body).ok()
}

fn http_get(addr: &str, path: &str, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;

    read_http_response_body(&mut stream)
}

fn http_put(addr: &str, path: &str, body: &str, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let request = format!(
        "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;

    read_http_response_body(&mut stream)
}

fn http_post(addr: &str, path: &str, body: &str, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;

    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).ok()?;

    read_http_response_body(&mut stream)
}

// --- mTLS-aware implementations ---

/// Sends commands to agents over mTLS.
pub struct MtlsNodeCommander {
    client_config: Arc<rustls::ClientConfig>,
}

impl MtlsNodeCommander {
    pub fn new(tls: &TlsConfig) -> Result<Self, String> {
        let client_config = build_rustls_client_config(tls)?;
        Ok(Self { client_config })
    }
}

impl NodeCommander for MtlsNodeCommander {
    fn start_test(&self, node: &NodeInfo, config: &TestConfig) -> Result<(), NetAnvilError> {
        let body = serde_json::to_string(config)
            .map_err(|e| NetAnvilError::Other(format!("serialize config: {e}")))?;
        mtls_post(&node.addr, "/test/start", &body, &self.client_config)
            .map_err(|e| NetAnvilError::Other(format!("mTLS start_test {}: {e}", node.id)))?;
        Ok(())
    }

    fn set_rate(&self, node: &NodeInfo, rps: f64) -> Result<(), NetAnvilError> {
        let body = format!(r#"{{"rps":{rps}}}"#);
        mtls_put(&node.addr, "/rate", &body, &self.client_config)
            .map_err(|e| NetAnvilError::Other(format!("mTLS set_rate {}: {e}", node.id)))?;
        Ok(())
    }

    fn stop_test(&self, node: &NodeInfo) -> Result<(), NetAnvilError> {
        mtls_post(&node.addr, "/stop", "", &self.client_config)
            .map_err(|e| NetAnvilError::Other(format!("mTLS stop_test {}: {e}", node.id)))?;
        Ok(())
    }
}

/// Fetches metrics from agents over mTLS.
pub struct MtlsMetricsFetcher {
    client_config: Arc<rustls::ClientConfig>,
}

impl MtlsMetricsFetcher {
    pub fn new(tls: &TlsConfig) -> Result<Self, String> {
        let client_config = build_rustls_client_config(tls)?;
        Ok(Self { client_config })
    }
}

impl MetricsFetcher for MtlsMetricsFetcher {
    fn fetch_metrics(&self, node: &NodeInfo) -> Option<RemoteMetrics> {
        let body = mtls_get(&node.addr, "/metrics", &self.client_config).ok()?;

        #[derive(serde::Deserialize)]
        struct MetricsResp {
            current_rps: Option<f64>,
            target_rps: Option<f64>,
            total_requests: Option<u64>,
            total_errors: Option<u64>,
            error_rate: Option<f64>,
            latency_p50_ms: Option<f64>,
            latency_p90_ms: Option<f64>,
            latency_p99_ms: Option<f64>,
        }

        let resp: MetricsResp = serde_json::from_str(&body).ok()?;

        Some(RemoteMetrics {
            node_id: node.id.clone(),
            current_rps: resp.current_rps.unwrap_or(0.0),
            target_rps: resp.target_rps.unwrap_or(0.0),
            total_requests: resp.total_requests.unwrap_or(0),
            total_errors: resp.total_errors.unwrap_or(0),
            error_rate: resp.error_rate.unwrap_or(0.0),
            latency_p50_ms: resp.latency_p50_ms.unwrap_or(0.0),
            latency_p90_ms: resp.latency_p90_ms.unwrap_or(0.0),
            latency_p99_ms: resp.latency_p99_ms.unwrap_or(0.0),
        })
    }
}

/// Discovery that probes agents over mTLS.
pub struct MtlsStaticDiscovery {
    nodes: Mutex<Vec<NodeInfo>>,
    failed: Mutex<Vec<NodeId>>,
}

impl MtlsStaticDiscovery {
    pub fn new(addrs: Vec<String>, tls: &TlsConfig) -> Result<Self, String> {
        let client_config = build_rustls_client_config(tls)?;
        let mut nodes = Vec::new();
        for addr in &addrs {
            match mtls_get(addr, "/info", &client_config) {
                Ok(body) => match serde_json::from_str::<NodeInfo>(&body) {
                    Ok(info) => {
                        tracing::info!(id = %info.id, cores = info.cores, "discovered agent at {addr} (mTLS)");
                        nodes.push(info);
                    }
                    Err(e) => {
                        tracing::warn!("bad /info response from {addr}: {e}");
                        nodes.push(NodeInfo {
                            id: NodeId(addr.clone()),
                            addr: addr.clone(),
                            cores: 1,
                            state: NodeState::Idle,
                        });
                    }
                },
                Err(e) => {
                    tracing::warn!("mTLS probe failed for {addr}: {e}");
                    nodes.push(NodeInfo {
                        id: NodeId(addr.clone()),
                        addr: addr.clone(),
                        cores: 1,
                        state: NodeState::Idle,
                    });
                }
            }
        }
        Ok(Self {
            nodes: Mutex::new(nodes),
            failed: Mutex::new(Vec::new()),
        })
    }
}

impl NodeDiscovery for MtlsStaticDiscovery {
    fn discover(&self) -> Vec<NodeInfo> {
        let failed = self.failed.lock().unwrap();
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| !failed.contains(&n.id))
            .cloned()
            .collect()
    }

    fn mark_failed(&self, id: &NodeId) {
        let mut failed = self.failed.lock().unwrap();
        if !failed.contains(id) {
            tracing::warn!(node = %id, "marking node as failed");
            failed.push(id.clone());
        }
    }
}

// --- mTLS raw HTTP helpers ---

fn build_rustls_client_config(tls: &TlsConfig) -> Result<Arc<rustls::ClientConfig>, String> {
    let ca_certs = load_pem_certs(&tls.ca_cert)?;
    let mut root_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        root_store.add(cert).map_err(|e| format!("add CA: {e}"))?;
    }

    let client_certs = load_pem_certs(&tls.cert)?;
    let client_key = load_pem_key(&tls.key)?;

    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("protocol versions: {e}"))?
    .with_root_certificates(root_store)
    .with_client_auth_cert(client_certs, client_key)
    .map_err(|e| format!("client config: {e}"))?;

    Ok(Arc::new(config))
}

fn load_pem_certs(path: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse certs {path}: {e}"))
}

fn load_pem_key(path: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("parse key {path}: {e}"))?
        .ok_or_else(|| format!("no key in {path}"))
}

fn mtls_request(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    config: &Arc<rustls::ClientConfig>,
) -> Result<String, String> {
    let hostname = addr.split(':').next().unwrap_or(addr);
    let server_name = rustls::pki_types::ServerName::try_from(hostname.to_string())
        .map_err(|e| format!("bad server name: {e}"))?;

    let stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    let conn = rustls::ClientConnection::new(config.clone(), server_name)
        .map_err(|e| format!("TLS handshake: {e}"))?;
    let mut tls = rustls::StreamOwned::new(conn, stream);

    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    tls.flush().map_err(|e| format!("flush: {e}"))?;

    let mut response = String::new();
    tls.read_to_string(&mut response)
        .map_err(|e| format!("read: {e}"))?;

    if let Some(idx) = response.find("\r\n\r\n") {
        Ok(response[idx + 4..].to_string())
    } else {
        Ok(response)
    }
}

fn mtls_get(addr: &str, path: &str, config: &Arc<rustls::ClientConfig>) -> Result<String, String> {
    mtls_request(addr, "GET", path, "", config)
}

fn mtls_post(
    addr: &str,
    path: &str,
    body: &str,
    config: &Arc<rustls::ClientConfig>,
) -> Result<String, String> {
    mtls_request(addr, "POST", path, body, config)
}

fn mtls_put(
    addr: &str,
    path: &str,
    body: &str,
    config: &Arc<rustls::ClientConfig>,
) -> Result<String, String> {
    mtls_request(addr, "PUT", path, body, config)
}

// --- Plain HTTP helpers ---

fn read_http_response_body(stream: &mut TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream);

    // Read status line
    let mut status_line = String::new();
    reader.read_line(&mut status_line).ok()?;

    // Read headers until blank line
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(cl) = line
            .strip_prefix("Content-Length: ")
            .or_else(|| line.strip_prefix("content-length: "))
        {
            content_length = cl.trim().parse().ok();
        }
    }

    // Read body
    if let Some(len) = content_length {
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).ok()?;
        String::from_utf8(body).ok()
    } else {
        // Read until close
        let mut body = String::new();
        reader.read_to_string(&mut body).ok()?;
        Some(body)
    }
}
