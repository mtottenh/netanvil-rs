//! HTTP-based implementations of the three distributed traits.
//!
//! Uses `reqwest` for HTTP communication with agent nodes, replacing
//! the original raw TCP HTTP implementation.

use std::sync::Mutex;
use std::time::Duration;

use netanvil_types::{
    MetricsFetcher, NetAnvilError, NodeCommander, NodeDiscovery, NodeId, NodeInfo, NodeState,
    RemoteMetrics, TestConfig, TlsConfig,
};

// ---------------------------------------------------------------------------
// Plain HTTP implementations
// ---------------------------------------------------------------------------

/// Discovers nodes from a static list provided at construction time.
/// Probes each agent's `GET /info` on construction to populate core counts.
pub struct StaticDiscovery {
    nodes: Mutex<Vec<NodeInfo>>,
    failed: Mutex<Vec<NodeId>>,
}

impl StaticDiscovery {
    /// Create from a list of "host:port" addresses.
    /// Probes each agent for node info (async, uses shared reqwest client).
    pub async fn new(addrs: Vec<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client");

        let mut nodes = Vec::new();
        for addr in &addrs {
            match probe_node_info(&client, addr).await {
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
    async fn discover(&self) -> Vec<NodeInfo> {
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
    client: reqwest::Client,
}

impl HttpMetricsFetcher {
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        Self { client }
    }
}

impl MetricsFetcher for HttpMetricsFetcher {
    async fn fetch_metrics(&self, node: &NodeInfo) -> Option<RemoteMetrics> {
        let url = format!("http://{}/metrics", node.addr);
        let resp = self.client.get(&url).send().await.ok()?;

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

        let resp: MetricsResp = resp.json().await.ok()?;

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
    client: reqwest::Client,
}

impl HttpNodeCommander {
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client");
        Self { client }
    }
}

impl NodeCommander for HttpNodeCommander {
    async fn start_test(&self, node: &NodeInfo, config: &TestConfig) -> Result<(), NetAnvilError> {
        let url = format!("http://{}/test/start", node.addr);
        self.client
            .post(&url)
            .json(config)
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("start_test {}: {e}", node.id)))?;
        Ok(())
    }

    async fn set_rate(&self, node: &NodeInfo, rps: f64) -> Result<(), NetAnvilError> {
        let url = format!("http://{}/rate", node.addr);
        self.client
            .put(&url)
            .json(&serde_json::json!({"rps": rps}))
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("set_rate {}: {e}", node.id)))?;
        Ok(())
    }

    async fn stop_test(&self, node: &NodeInfo) -> Result<(), NetAnvilError> {
        let url = format!("http://{}/stop", node.addr);
        self.client
            .post(&url)
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("stop_test {}: {e}", node.id)))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// mTLS implementations
// ---------------------------------------------------------------------------

/// Sends commands to agents over mTLS.
pub struct MtlsNodeCommander {
    client: reqwest::Client,
}

impl MtlsNodeCommander {
    pub fn new(tls: &TlsConfig) -> Result<Self, String> {
        let client = build_mtls_client(tls)?;
        Ok(Self { client })
    }
}

impl NodeCommander for MtlsNodeCommander {
    async fn start_test(&self, node: &NodeInfo, config: &TestConfig) -> Result<(), NetAnvilError> {
        let url = format!("https://{}/test/start", node.addr);
        self.client
            .post(&url)
            .json(config)
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("mTLS start_test {}: {e}", node.id)))?;
        Ok(())
    }

    async fn set_rate(&self, node: &NodeInfo, rps: f64) -> Result<(), NetAnvilError> {
        let url = format!("https://{}/rate", node.addr);
        self.client
            .put(&url)
            .json(&serde_json::json!({"rps": rps}))
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("mTLS set_rate {}: {e}", node.id)))?;
        Ok(())
    }

    async fn stop_test(&self, node: &NodeInfo) -> Result<(), NetAnvilError> {
        let url = format!("https://{}/stop", node.addr);
        self.client
            .post(&url)
            .send()
            .await
            .map_err(|e| NetAnvilError::Other(format!("mTLS stop_test {}: {e}", node.id)))?;
        Ok(())
    }
}

/// Fetches metrics from agents over mTLS.
pub struct MtlsMetricsFetcher {
    client: reqwest::Client,
}

impl MtlsMetricsFetcher {
    pub fn new(tls: &TlsConfig) -> Result<Self, String> {
        let client = build_mtls_client(tls)?;
        Ok(Self { client })
    }
}

impl MetricsFetcher for MtlsMetricsFetcher {
    async fn fetch_metrics(&self, node: &NodeInfo) -> Option<RemoteMetrics> {
        let url = format!("https://{}/metrics", node.addr);
        let resp = self.client.get(&url).send().await.ok()?;

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

        let resp: MetricsResp = resp.json().await.ok()?;

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
    pub async fn new(addrs: Vec<String>, tls: &TlsConfig) -> Result<Self, String> {
        let client = build_mtls_client(tls)?;
        let mut nodes = Vec::new();
        for addr in &addrs {
            let url = format!("https://{}/info", addr);
            match client.get(&url).send().await {
                Ok(resp) => match resp.json::<NodeInfo>().await {
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
    async fn discover(&self) -> Vec<NodeInfo> {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Probe an agent's /info endpoint.
async fn probe_node_info(client: &reqwest::Client, addr: &str) -> Option<NodeInfo> {
    let url = format!("http://{}/info", addr);
    client.get(&url).send().await.ok()?.json().await.ok()
}

/// Build an async reqwest client with mTLS.
fn build_mtls_client(tls: &TlsConfig) -> Result<reqwest::Client, String> {
    let client_config = netanvil_api::build_client_config(tls)?;

    reqwest::Client::builder()
        .use_preconfigured_tls((*client_config).clone())
        .build()
        .map_err(|e| format!("reqwest mTLS client: {e}"))
}
