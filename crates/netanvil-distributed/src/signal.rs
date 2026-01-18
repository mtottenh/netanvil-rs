//! External signal polling for the distributed leader.
//!
//! Polls an HTTP endpoint on the target server to extract a named metric
//! value (e.g., "load" from `{"load": 82.5}`). Used by the leader to
//! feed server-reported metrics into the PID rate controller.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Polls an HTTP endpoint for an external signal value.
pub struct HttpSignalPoller {
    client: reqwest::Client,
    url: String,
    field: String,
}

impl HttpSignalPoller {
    pub fn new(url: String, field: String, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client for signal poller");
        Self { url, field, client }
    }

    /// Create from optional config fields. Returns None if not configured.
    pub fn from_config(url: Option<&str>, field: Option<&str>) -> Option<Self> {
        match (url, field) {
            (Some(u), Some(f)) => Some(Self::new(u.into(), f.into(), Duration::from_secs(5))),
            _ => None,
        }
    }

    /// Create an async closure suitable for `coordinator.set_signal_source()`.
    pub fn into_source(
        self,
    ) -> impl FnMut() -> Pin<Box<dyn Future<Output = Vec<(String, f64)>> + Send>> + Send {
        let Self { client, url, field } = self;
        move || {
            let client = client.clone();
            let url = url.clone();
            let field = field.clone();
            Box::pin(async move {
                let result: Option<(String, f64)> = async {
                    let resp = client.get(&url).send().await.ok()?;
                    let json: serde_json::Value = resp.json().await.ok()?;
                    let value = json.get(&field)?.as_f64()?;
                    Some((field, value))
                }
                .await;
                result.into_iter().collect()
            })
        }
    }
}
