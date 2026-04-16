//! API-facing test specification type.
//!
//! `TestSpec` is the JSON body accepted by `POST /tests`. It mirrors
//! `TestConfig` but uses human-readable duration strings (`"60s"`, `"5m"`)
//! and omits daemon-level fields (workers, mTLS, metrics port).
//!
//! The leader daemon converts `TestSpec` → `TestConfig` by parsing
//! durations and merging daemon-level defaults.

use std::time::{Duration, SystemTime};

use netanvil_types::{
    BoundsConfig, ConnectionConfig, ConnectionPolicy, ConstraintConfig, GainsConfig, HttpVersion,
    PluginConfig, ProtocolConfig, RateConfig, SchedulerConfig, SetpointConstraintConfig,
    TestConfig, WarmupConfig,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Test ID
// ---------------------------------------------------------------------------

/// Unique identifier for a test run.
///
/// Format: `t-YYYYMMDD-HHMMSS-XXXX` where XXXX is 4 random hex chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestId(pub String);

impl TestId {
    /// Generate a new test ID from the current wall-clock time.
    pub fn generate() -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();

        // Convert to UTC date/time components (no chrono dependency).
        let (y, m, d, hh, mm, ss) = secs_to_utc(secs);
        let rand_suffix: u16 = (now.subsec_nanos() ^ (secs as u32).wrapping_mul(2654435761)) as u16;

        Self(format!(
            "t-{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}-{rand_suffix:04x}"
        ))
    }
}

impl std::fmt::Display for TestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Convert unix timestamp to (year, month, day, hour, minute, second) in UTC.
fn secs_to_utc(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let ss = secs % 60;
    let total_mins = secs / 60;
    let mm = total_mins % 60;
    let total_hours = total_mins / 60;
    let hh = total_hours % 24;
    let mut days = total_hours / 24;

    // Compute year from days since epoch (1970-01-01).
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    // Compute month and day.
    let leap = is_leap(year);
    let month_days: [u64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1, hh, mm, ss)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

// ---------------------------------------------------------------------------
// TestSpec
// ---------------------------------------------------------------------------

/// API-facing test specification. Accepted as the body of `POST /tests`.
///
/// Duration fields are strings (`"60s"`, `"5m"`, `"500ms"`) for ergonomic
/// JSON authoring. The leader parses these during `TestSpec::into_config()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSpec {
    /// Target URL(s) or addresses.
    pub targets: Vec<String>,

    /// HTTP method. Default: "GET".
    #[serde(default = "default_method")]
    pub method: String,

    /// Test duration as a human-readable string (e.g., "60s", "5m").
    pub duration: String,

    /// Rate configuration (same variants as `RateConfig`).
    pub rate: RateSpecConfig,

    /// Request headers.
    #[serde(default)]
    pub headers: Vec<(String, String)>,

    /// Number of worker cores per agent (0 = auto-detect).
    #[serde(default)]
    pub num_cores: usize,

    /// Per-request timeout as a human-readable string. Default: "30s".
    #[serde(default = "default_timeout")]
    pub timeout: String,

    /// HTTP status codes >= this threshold count as errors. Default: 400.
    #[serde(default = "default_error_threshold")]
    pub error_status_threshold: u16,

    /// Scheduler type.
    #[serde(default)]
    pub scheduler: SchedulerConfig,

    /// HTTP version selection.
    #[serde(default)]
    pub http_version: HttpVersion,

    /// Protocol-specific configuration (TCP/UDP/DNS/Redis).
    #[serde(default)]
    pub protocol: Option<ProtocolConfig>,

    /// Plugin configuration.
    #[serde(default)]
    pub plugin: Option<PluginConfig>,

    /// Connection policy.
    #[serde(default)]
    pub connection_policy: Option<ConnectionPolicy>,

    /// External metrics URL for PID controllers.
    #[serde(default)]
    pub external_metrics_url: Option<String>,

    /// External metrics field name.
    #[serde(default)]
    pub external_metrics_field: Option<String>,

    /// Stop after this many total requests.
    #[serde(default)]
    pub max_requests: Option<u64>,

    /// Stop if error count exceeds this.
    #[serde(default)]
    pub autostop_threshold: Option<u64>,

    /// Fraction of TCP reads to sample for health observation (0.0–1.0).
    #[serde(default)]
    pub health_sample_rate: f64,

    /// Path for control-plane trace recording (JSONL).
    #[serde(default)]
    pub control_trace: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

fn default_timeout() -> String {
    "30s".into()
}

fn default_error_threshold() -> u16 {
    400
}

/// Rate configuration with human-readable duration strings.
///
/// Mirrors `RateConfig` but uses `String` for durations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RateSpecConfig {
    Static {
        rps: f64,
    },
    Step {
        /// Vec of ("offset_string", rps).
        steps: Vec<(String, f64)>,
    },
    Adaptive {
        bounds: BoundsConfig,
        #[serde(default)]
        warmup: Option<WarmupSpecConfig>,
        #[serde(default)]
        initial_rps: Option<f64>,
        constraints: Vec<ConstraintSpecConfig>,
    },
}

/// Warmup configuration with human-readable duration string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarmupSpecConfig {
    pub rps: f64,
    pub duration: String,
}

/// Gains configuration with human-readable duration string for auto-tune.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GainsSpecConfig {
    Auto {
        #[serde(default = "default_autotune_duration_str")]
        autotune_duration: String,
        #[serde(default = "default_smoothing")]
        smoothing: f64,
    },
    Manual {
        kp: f64,
        ki: f64,
        kd: f64,
    },
}

impl Default for GainsSpecConfig {
    fn default() -> Self {
        GainsSpecConfig::Auto {
            autotune_duration: default_autotune_duration_str(),
            smoothing: default_smoothing(),
        }
    }
}

fn default_autotune_duration_str() -> String {
    "3s".into()
}

fn default_smoothing() -> f64 {
    0.3
}

fn default_tracking_gain() -> f64 {
    0.5
}

/// Setpoint constraint config with string-based durations for the spec layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetpointSpecConstraintConfig {
    pub id: String,
    pub metric: netanvil_types::MetricRef,
    #[serde(default)]
    pub smoother: Option<netanvil_types::SmootherConfig>,
    pub target: f64,
    #[serde(default)]
    pub gains: GainsSpecConfig,
    #[serde(default = "default_tracking_gain")]
    pub tracking_gain: f64,
}

/// Constraint config for the spec layer, with string-based durations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstraintSpecConfig {
    Threshold(netanvil_types::ThresholdConstraintConfig),
    Setpoint(SetpointSpecConstraintConfig),
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Error type for TestSpec → TestConfig conversion.
#[derive(Debug)]
pub struct SpecError(pub String);

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

impl TestSpec {
    /// Convert this spec into a `TestConfig`, parsing duration strings
    /// and filling in daemon-level defaults.
    pub fn into_config(self) -> Result<TestConfig, SpecError> {
        let duration = parse_duration(&self.duration)
            .map_err(|e| SpecError(format!("invalid duration '{}': {e}", self.duration)))?;

        let timeout = parse_duration(&self.timeout)
            .map_err(|e| SpecError(format!("invalid timeout '{}': {e}", self.timeout)))?;

        let rate = convert_rate(self.rate)?;

        let mut connections = ConnectionConfig::default();
        connections.request_timeout = timeout;
        if let Some(policy) = self.connection_policy {
            connections.connection_policy = policy;
        }

        Ok(TestConfig {
            targets: self.targets,
            method: self.method,
            duration,
            rate,
            scheduler: self.scheduler,
            headers: self.headers,
            num_cores: self.num_cores,
            connections,
            control_interval: Duration::from_secs(3),
            error_status_threshold: self.error_status_threshold,
            external_metrics_url: self.external_metrics_url,
            external_metrics_field: self.external_metrics_field,
            bandwidth_limit_bps: None,
            warmup_duration: None,
            max_requests: self.max_requests,
            autostop_threshold: self.autostop_threshold,
            refusestop_threshold: None,
            graceful_shutdown: true,
            plugin: self.plugin,
            protocol: self.protocol,
            http_version: self.http_version,
            tls_client: None,
            target_bytes: None,
            tracked_response_headers: Vec::new(),
            md5_check_enabled: false,
            response_signal_headers: Vec::new(),
            event_log: None,
            health_sample_rate: self.health_sample_rate,
            control_trace: self.control_trace,
        })
    }

    /// One-line summary of the test configuration for display in listings.
    pub fn summary(&self) -> String {
        let rate_desc = match &self.rate {
            RateSpecConfig::Static { rps } => format!("Static {rps:.0} RPS"),
            RateSpecConfig::Step { steps } => format!("Step ({} phases)", steps.len()),
            RateSpecConfig::Adaptive {
                bounds,
                constraints,
                ..
            } => format!(
                "Adaptive {:.0}-{:.0} RPS, {} constraints",
                bounds.min_rps,
                bounds.max_rps,
                constraints.len()
            ),
        };
        let n_targets = self.targets.len();
        format!("{rate_desc}, {}, {n_targets} target(s)", self.duration)
    }
}

fn convert_rate(spec: RateSpecConfig) -> Result<RateConfig, SpecError> {
    match spec {
        RateSpecConfig::Static { rps } => Ok(RateConfig::Static { rps }),
        RateSpecConfig::Step { steps } => {
            let mut parsed = Vec::with_capacity(steps.len());
            for (dur_str, rps) in steps {
                let d = parse_duration(&dur_str)
                    .map_err(|e| SpecError(format!("invalid step duration '{dur_str}': {e}")))?;
                parsed.push((d, rps));
            }
            Ok(RateConfig::Step { steps: parsed })
        }
        RateSpecConfig::Adaptive {
            bounds,
            warmup,
            initial_rps,
            constraints,
        } => {
            let warmup = match warmup {
                Some(w) => {
                    let d = parse_duration(&w.duration).map_err(|e| {
                        SpecError(format!("invalid warmup duration '{}': {e}", w.duration))
                    })?;
                    Some(WarmupConfig {
                        rps: w.rps,
                        duration: d,
                    })
                }
                None => None,
            };
            let constraints = constraints
                .into_iter()
                .map(|c| convert_constraint_spec(c))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(RateConfig::Adaptive {
                bounds,
                warmup,
                initial_rps,
                constraints,
                increase: None,
                cooldown: None,
                floor: None,
                rate_change_limits: None,
            })
        }
    }
}

/// Convert a `ConstraintSpecConfig` to a `ConstraintConfig`, parsing
/// string durations in `GainsSpecConfig::Auto`.
fn convert_constraint_spec(spec: ConstraintSpecConfig) -> Result<ConstraintConfig, SpecError> {
    match spec {
        ConstraintSpecConfig::Threshold(tc) => Ok(ConstraintConfig::Threshold(tc)),
        ConstraintSpecConfig::Setpoint(sc) => {
            let gains = convert_gains_spec(sc.gains)?;
            Ok(ConstraintConfig::Setpoint(SetpointConstraintConfig {
                id: sc.id,
                metric: sc.metric,
                smoother: sc.smoother,
                target: sc.target,
                gains,
                tracking_gain: sc.tracking_gain,
            }))
        }
    }
}

/// Convert a `GainsSpecConfig` to a `GainsConfig`, parsing autotune_duration.
fn convert_gains_spec(spec: GainsSpecConfig) -> Result<GainsConfig, SpecError> {
    match spec {
        GainsSpecConfig::Auto {
            autotune_duration,
            smoothing,
        } => {
            let d = parse_duration(&autotune_duration).map_err(|e| {
                SpecError(format!(
                    "invalid autotune_duration '{autotune_duration}': {e}"
                ))
            })?;
            Ok(GainsConfig::Auto {
                autotune_duration: d,
                smoothing,
            })
        }
        GainsSpecConfig::Manual { kp, ki, kd } => Ok(GainsConfig::Manual { kp, ki, kd }),
    }
}

// ---------------------------------------------------------------------------
// Duration parsing (self-contained, no external dependency)
// ---------------------------------------------------------------------------

/// Parse a human-readable duration string.
///
/// Supported formats: `"500ms"`, `"30s"`, `"5m"`, `"1h"`, or bare number
/// (interpreted as seconds).
fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration string".into());
    }

    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms
            .parse()
            .map_err(|_| format!("invalid milliseconds: {ms}"))?;
        return Ok(Duration::from_millis(n));
    }
    if let Some(h) = s.strip_suffix('h') {
        let n: f64 = h.parse().map_err(|_| format!("invalid hours: {h}"))?;
        return Ok(Duration::from_secs_f64(n * 3600.0));
    }
    if let Some(m) = s.strip_suffix('m') {
        let n: f64 = m.parse().map_err(|_| format!("invalid minutes: {m}"))?;
        return Ok(Duration::from_secs_f64(n * 60.0));
    }
    if let Some(secs) = s.strip_suffix('s') {
        let n: f64 = secs
            .parse()
            .map_err(|_| format!("invalid seconds: {secs}"))?;
        return Ok(Duration::from_secs_f64(n));
    }
    // Bare number = seconds.
    let n: f64 = s.parse().map_err(|_| format!("invalid duration: {s}"))?;
    Ok(Duration::from_secs_f64(n))
}

// ---------------------------------------------------------------------------
// Test entry (shared state for the test queue)
// ---------------------------------------------------------------------------

/// Status of a test in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Queued,
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Metadata for a test visible in listings.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestInfo {
    pub id: TestId,
    pub status: TestStatus,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub config_summary: String,
}

/// ISO 8601 timestamp from SystemTime (no chrono dependency).
pub fn format_timestamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_utc(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_format() {
        let id = TestId::generate();
        assert!(id.0.starts_with("t-"), "got: {}", id.0);
        assert_eq!(id.0.len(), 22, "got: {}", id.0); // t-YYYYMMDD-HHMMSS-XXXX
    }

    #[test]
    fn test_parse_durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn test_spec_summary() {
        let spec = TestSpec {
            targets: vec!["http://localhost".into()],
            method: "GET".into(),
            duration: "60s".into(),
            rate: RateSpecConfig::Adaptive {
                bounds: BoundsConfig {
                    min_rps: 10.0,
                    max_rps: 50000.0,
                },
                warmup: Some(WarmupSpecConfig {
                    rps: 10.0,
                    duration: "10s".into(),
                }),
                initial_rps: None,
                constraints: vec![],
            },
            headers: vec![],
            num_cores: 0,
            timeout: "30s".into(),
            error_status_threshold: 400,
            scheduler: SchedulerConfig::default(),
            http_version: HttpVersion::default(),
            protocol: None,
            plugin: None,
            connection_policy: None,
            external_metrics_url: None,
            external_metrics_field: None,
            max_requests: None,
            autostop_threshold: None,
            health_sample_rate: 0.0,
        };
        let summary = spec.summary();
        assert!(summary.contains("Adaptive"), "got: {summary}");
        assert!(summary.contains("50000"), "got: {summary}");
    }

    #[test]
    fn test_spec_into_config() {
        let spec = TestSpec {
            targets: vec!["http://localhost:8080".into()],
            method: "POST".into(),
            duration: "2m".into(),
            rate: RateSpecConfig::Static { rps: 500.0 },
            headers: vec![("X-Test".into(), "1".into())],
            num_cores: 4,
            timeout: "10s".into(),
            error_status_threshold: 500,
            scheduler: SchedulerConfig::default(),
            http_version: HttpVersion::Http1,
            protocol: None,
            plugin: None,
            connection_policy: None,
            external_metrics_url: None,
            external_metrics_field: None,
            max_requests: None,
            autostop_threshold: None,
            health_sample_rate: 0.01,
        };
        let config = spec.into_config().unwrap();
        assert_eq!(config.duration, Duration::from_secs(120));
        assert_eq!(config.connections.request_timeout, Duration::from_secs(10));
        assert_eq!(config.method, "POST");
        assert_eq!(config.num_cores, 4);
        assert_eq!(config.error_status_threshold, 500);
        assert!(matches!(config.rate, RateConfig::Static { rps } if (rps - 500.0).abs() < 0.01));
    }

    #[test]
    fn test_format_timestamp() {
        // 2026-01-01T00:00:00Z = 1767225600
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1767225600);
        assert_eq!(format_timestamp(t), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_secs_to_utc_epoch() {
        assert_eq!(secs_to_utc(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn test_secs_to_utc_known_date() {
        // 2024-02-29T12:30:45Z (leap year) = 1709209845
        let (y, m, d, h, mi, s) = secs_to_utc(1709209845);
        assert_eq!((y, m, d, h, mi, s), (2024, 2, 29, 12, 30, 45));
    }
}
