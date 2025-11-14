//! Filesystem-backed result storage for test entries.
//!
//! Stores test metadata and results as JSON files in a results directory.
//! An `index.json` manifest provides fast listing without reading every file.
//! Writes use temp-file + rename for crash safety.

use std::fs;
use std::path::{Path, PathBuf};

use crate::coordinator::DistributedTestResult;
use crate::test_spec::{TestId, TestInfo, TestSpec, TestStatus};

use serde::{Deserialize, Serialize};

/// A complete test entry stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTestEntry {
    pub info: TestInfo,
    pub spec: TestSpec,
    pub result: Option<StoredTestResult>,
}

/// Serializable form of `DistributedTestResult` for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTestResult {
    pub total_requests: u64,
    pub total_errors: u64,
    pub duration_secs: f64,
    pub request_rate: f64,
    pub error_rate: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p99_ms: f64,
    pub nodes: Vec<StoredNodeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredNodeResult {
    pub node_id: String,
    pub total_requests: u64,
    pub total_errors: u64,
    pub current_rps: f64,
    pub target_rps: f64,
    pub error_rate: f64,
    pub latency_p50_ms: f64,
    pub latency_p90_ms: f64,
    pub latency_p99_ms: f64,
}

impl StoredTestResult {
    /// Convert from a `DistributedTestResult`.
    pub fn from_distributed(r: &DistributedTestResult) -> Self {
        let secs = r.duration.as_secs_f64().max(0.001);
        let error_rate = if r.total_requests > 0 {
            r.total_errors as f64 / r.total_requests as f64
        } else {
            0.0
        };

        let nodes: Vec<StoredNodeResult> = r
            .nodes
            .iter()
            .map(|(id, m)| StoredNodeResult {
                node_id: id.0.clone(),
                total_requests: m.total_requests,
                total_errors: m.total_errors,
                current_rps: m.current_rps,
                target_rps: m.target_rps,
                error_rate: m.error_rate,
                latency_p50_ms: m.latency_p50_ms,
                latency_p90_ms: m.latency_p90_ms,
                latency_p99_ms: m.latency_p99_ms,
            })
            .collect();

        let p50 = r
            .nodes
            .iter()
            .map(|(_, m)| m.latency_p50_ms)
            .fold(0.0f64, f64::max);
        let p90 = r
            .nodes
            .iter()
            .map(|(_, m)| m.latency_p90_ms)
            .fold(0.0f64, f64::max);
        let p99 = r
            .nodes
            .iter()
            .map(|(_, m)| m.latency_p99_ms)
            .fold(0.0f64, f64::max);

        Self {
            total_requests: r.total_requests,
            total_errors: r.total_errors,
            duration_secs: secs,
            request_rate: r.total_requests as f64 / secs,
            error_rate,
            latency_p50_ms: p50,
            latency_p90_ms: p90,
            latency_p99_ms: p99,
            nodes,
        }
    }
}

/// Filesystem-backed store for test entries.
pub struct ResultStore {
    dir: PathBuf,
    max_results: usize,
}

impl ResultStore {
    /// Open or create a result store at the given directory.
    pub fn open(dir: impl Into<PathBuf>, max_results: usize) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir, max_results })
    }

    /// Load the index from disk, returning all stored test infos.
    pub fn load_index(&self) -> Vec<TestInfo> {
        let path = self.index_path();
        match fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Save a test entry (metadata + spec + optional result) to disk.
    /// Also updates the index.
    pub fn save(&self, entry: &StoredTestEntry) -> std::io::Result<()> {
        // Write the individual entry file (temp + rename).
        let entry_path = self.entry_path(&entry.info.id);
        let json = serde_json::to_string_pretty(entry)
            .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
        atomic_write(&entry_path, json.as_bytes())?;

        // Update the index.
        let mut index = self.load_index();

        // Replace existing entry or append.
        if let Some(pos) = index.iter().position(|i| i.id == entry.info.id) {
            index[pos] = entry.info.clone();
        } else {
            index.push(entry.info.clone());
        }

        // Prune oldest completed entries if over max_results.
        self.prune(&mut index)?;

        let index_json = serde_json::to_string_pretty(&index)
            .map_err(|e| std::io::Error::other(format!("serialize index: {e}")))?;
        atomic_write(&self.index_path(), index_json.as_bytes())?;

        Ok(())
    }

    /// Load a single test entry by ID.
    pub fn load_entry(&self, id: &TestId) -> Option<StoredTestEntry> {
        let path = self.entry_path(id);
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Remove a test entry from disk and the index.
    pub fn remove(&self, id: &TestId) -> std::io::Result<()> {
        let entry_path = self.entry_path(id);
        if entry_path.exists() {
            fs::remove_file(&entry_path)?;
        }

        let mut index = self.load_index();
        index.retain(|i| i.id != *id);
        let index_json = serde_json::to_string_pretty(&index)
            .map_err(|e| std::io::Error::other(format!("serialize index: {e}")))?;
        atomic_write(&self.index_path(), index_json.as_bytes())?;

        Ok(())
    }

    fn entry_path(&self, id: &TestId) -> PathBuf {
        self.dir.join(format!("{}.json", id.0))
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn prune(&self, index: &mut Vec<TestInfo>) -> std::io::Result<()> {
        // Only prune completed/failed/cancelled tests.
        let terminal: Vec<_> = index
            .iter()
            .filter(|i| {
                matches!(
                    i.status,
                    TestStatus::Completed | TestStatus::Failed | TestStatus::Cancelled
                )
            })
            .cloned()
            .collect();

        if terminal.len() <= self.max_results {
            return Ok(());
        }

        // Remove oldest terminal entries (they're in insertion order).
        let to_remove = terminal.len() - self.max_results;
        let mut removed = 0;
        let mut remove_ids = Vec::new();
        for info in &terminal {
            if removed >= to_remove {
                break;
            }
            remove_ids.push(info.id.clone());
            removed += 1;
        }

        for id in &remove_ids {
            let path = self.entry_path(id);
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }

        index.retain(|i| !remove_ids.contains(&i.id));
        Ok(())
    }
}

/// Write data to a temp file then rename for crash safety.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_spec::format_timestamp;
    use std::time::SystemTime;

    fn make_info(id: &str, status: TestStatus) -> TestInfo {
        TestInfo {
            id: TestId(id.into()),
            status,
            created_at: format_timestamp(SystemTime::now()),
            started_at: None,
            completed_at: None,
            config_summary: "test".into(),
        }
    }

    fn make_spec() -> TestSpec {
        TestSpec {
            targets: vec!["http://localhost".into()],
            method: "GET".into(),
            duration: "10s".into(),
            rate: crate::test_spec::RateSpecConfig::Static { rps: 100.0 },
            headers: vec![],
            num_cores: 0,
            timeout: "30s".into(),
            error_status_threshold: 400,
            scheduler: netanvil_types::SchedulerConfig::default(),
            http_version: netanvil_types::HttpVersion::default(),
            protocol: None,
            plugin: None,
            connection_policy: None,
            external_metrics_url: None,
            external_metrics_field: None,
            max_requests: None,
            autostop_threshold: None,
        }
    }

    #[test]
    fn round_trip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResultStore::open(dir.path(), 100).unwrap();

        let entry = StoredTestEntry {
            info: make_info("t-test-001", TestStatus::Completed),
            spec: make_spec(),
            result: Some(StoredTestResult {
                total_requests: 1000,
                total_errors: 5,
                duration_secs: 10.0,
                request_rate: 100.0,
                error_rate: 0.005,
                latency_p50_ms: 1.0,
                latency_p90_ms: 5.0,
                latency_p99_ms: 10.0,
                nodes: vec![],
            }),
        };

        store.save(&entry).unwrap();

        let loaded = store.load_entry(&TestId("t-test-001".into())).unwrap();
        assert_eq!(loaded.info.id.0, "t-test-001");
        assert_eq!(loaded.result.unwrap().total_requests, 1000);

        let index = store.load_index();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].id.0, "t-test-001");
    }

    #[test]
    fn prune_oldest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResultStore::open(dir.path(), 2).unwrap();

        for i in 0..4 {
            let entry = StoredTestEntry {
                info: make_info(&format!("t-test-{i:03}"), TestStatus::Completed),
                spec: make_spec(),
                result: None,
            };
            store.save(&entry).unwrap();
        }

        let index = store.load_index();
        assert_eq!(index.len(), 2);
        // Oldest (000, 001) should be pruned, newest (002, 003) kept.
        assert_eq!(index[0].id.0, "t-test-002");
        assert_eq!(index[1].id.0, "t-test-003");
    }

    #[test]
    fn remove_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResultStore::open(dir.path(), 100).unwrap();

        let entry = StoredTestEntry {
            info: make_info("t-test-rm", TestStatus::Completed),
            spec: make_spec(),
            result: None,
        };
        store.save(&entry).unwrap();
        assert!(store.load_entry(&TestId("t-test-rm".into())).is_some());

        store.remove(&TestId("t-test-rm".into())).unwrap();
        assert!(store.load_entry(&TestId("t-test-rm".into())).is_none());
        assert!(store.load_index().is_empty());
    }
}
