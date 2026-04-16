//! Control-plane trace recording.
//!
//! Records per-tick controller decisions to a structured file for post-test
//! analysis and replay. The trace captures the full decision context at each
//! tick: input metrics (opaque), per-constraint evaluations (dynamic
//! key-value state), arbiter-level policy decisions, and the final rate.
//!
//! The trace format is deliberately agnostic to specific metrics or constraint
//! types. Input metrics are serialized as an opaque blob (for replay).
//! Per-constraint state flows through the generic `constraint_state()` export
//! that each constraint type fills with its own relevant internals.
//!
//! ## Design: enum, not trait
//!
//! The recorder is an enum (`TraceRecorder`) rather than a trait object. The
//! set of backends is closed (JSONL, future Arrow), and the recorder must be
//! constructible from serializable config (a path string in `TestConfig`)
//! because tests can arrive via HTTP API in distributed daemon mode.

use std::io::{BufWriter, Write};

use serde::{Deserialize, Serialize};

use super::constraints::ConstraintClass;

// ---------------------------------------------------------------------------
// Per-constraint evaluation record
// ---------------------------------------------------------------------------

/// What a constraint did this tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Participation {
    /// Constraint evaluated, no rate opinion (`NoObjection`).
    Abstained { severity: f64 },
    /// Constraint expressed a rate opinion (`Hold` or `DesireRate`).
    Active {
        severity: f64,
        desired_rate: f64,
        is_binding: bool,
    },
    /// Constraint returned `None` (inactive — e.g., smoother warming up,
    /// awaiting exploration gains).
    Inactive,
}

/// One constraint's evaluation and state for a single tick.
///
/// Generic over constraint type: the `state` vec carries whatever the
/// constraint exports via `constraint_state()`. A threshold constraint
/// exports `severity, smoothed, streak, recovery_ema`. A PID exports
/// `severity, integral, smoothed, target, desired_rate`. The trace
/// infrastructure doesn't know or care which.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationRecord {
    pub id: String,
    pub class: ConstraintClassLabel,
    pub participation: Participation,
    /// Dynamic key-value state from `Constraint::constraint_state()`.
    /// Contents depend on the constraint type — the trace is agnostic.
    pub state: Vec<(String, f64)>,
}

/// Serializable mirror of `ConstraintClass`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintClassLabel {
    OperatingPoint,
    Catastrophic,
}

impl From<ConstraintClass> for ConstraintClassLabel {
    fn from(c: ConstraintClass) -> Self {
        match c {
            ConstraintClass::OperatingPoint => Self::OperatingPoint,
            ConstraintClass::Catastrophic => Self::Catastrophic,
        }
    }
}

// ---------------------------------------------------------------------------
// Arbiter-level decision record
// ---------------------------------------------------------------------------

/// Arbiter policy state for a single tick.
///
/// These are structural fields of the arbiter's design (ceiling, floor,
/// cooldown), not metric-domain concepts — safe to name explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbiterRecord {
    /// Rate selected by the composition strategy (before policies).
    pub composition_rate: f64,
    /// Which constraint was binding (`None` if no constraints had an opinion).
    pub binding: Option<String>,
    /// Current progressive ceiling.
    pub ceiling: f64,
    /// Current known-good floor.
    pub floor: f64,
    /// Whether post-backoff cooldown is suppressing increases.
    pub cooldown_active: bool,
    /// Ticks remaining in cooldown.
    pub cooldown_remaining: u32,
    /// Whether this tick triggered a new backoff + cooldown engagement.
    pub backoff_engaged: bool,
    /// Ticks since the last rate increase.
    pub ticks_since_increase: u32,
    /// EMA of backoff frequency (failure rate).
    pub failure_rate_ema: f64,
    /// Total backoff count since test start.
    pub backoff_count: u32,
}

// ---------------------------------------------------------------------------
// Tick record (the top-level trace entry)
// ---------------------------------------------------------------------------

/// Full per-tick control-plane record.
///
/// Captures the controller's input, per-constraint evaluations, arbiter
/// decisions, and output — everything needed for post-test analysis and
/// replay through a different controller configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickRecord {
    pub tick: u32,

    // ── Input ──
    // Full metrics summary, serialized as-is. The trace doesn't interpret
    // this — it's an opaque blob for replay. Analysis tools use the
    // per-constraint state (below) for observability, not the raw input.
    pub metrics: serde_json::Value,

    // ── Per-constraint evaluations ──
    pub evaluations: Vec<EvaluationRecord>,
    pub abstainer_count: u32,

    // ── Arbiter decisions ──
    pub arbiter: ArbiterRecord,

    // ── Output ──
    /// Rate at the start of this tick (before the controller acted).
    pub previous_rps: f64,
    /// Rate at the end of this tick (the controller's decision).
    pub target_rps: f64,
}

// ---------------------------------------------------------------------------
// TraceRecorder enum
// ---------------------------------------------------------------------------

/// Runtime trace recorder. Constructed from a path string at execution time.
///
/// Uses an enum (not a trait object) because:
/// - The set of backends is closed (JSONL, future Arrow)
/// - Must be constructible from serializable config (path in `TestConfig`)
/// - Zero-size `Noop` variant eliminates all overhead when disabled
pub enum TraceRecorder {
    /// No recording — zero cost.
    Noop,
    /// JSON Lines: one JSON object per tick.
    Jsonl(JsonlTraceRecorder),
}

impl TraceRecorder {
    /// Construct a recorder from a file path. Format is inferred from
    /// the file extension (`.arrow`/`.ipc` → Arrow in the future,
    /// everything else → JSONL).
    pub fn from_path(path: &str) -> std::io::Result<Self> {
        // Future: match on extension for Arrow support
        Ok(Self::Jsonl(JsonlTraceRecorder::new(path)?))
    }

    /// Whether recording is active. When false, the arbiter skips trace
    /// assembly entirely (avoids allocating per-constraint state vecs).
    #[inline]
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Noop)
    }

    /// Record a single tick.
    pub fn record(&mut self, tick: &TickRecord) {
        match self {
            Self::Noop => {}
            Self::Jsonl(r) => r.record(tick),
        }
    }

    /// Flush any buffered data.
    pub fn flush(&mut self) {
        match self {
            Self::Noop => {}
            Self::Jsonl(r) => r.flush(),
        }
    }
}

// ---------------------------------------------------------------------------
// JsonlTraceRecorder
// ---------------------------------------------------------------------------

/// Writes one JSON object per tick to a JSON Lines file.
///
/// Readable by Python, Polars (`read_ndjson`), jq, DuckDB.
/// Typical size: ~200 bytes/tick → 100 Hz × 1 hr ≈ 70 MB.
pub struct JsonlTraceRecorder {
    writer: BufWriter<std::fs::File>,
    ticks_written: u64,
}

impl JsonlTraceRecorder {
    /// Create a new recorder writing to the given path.
    /// Creates parent directories if needed.
    pub fn new(path: &str) -> std::io::Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = std::fs::File::create(path)?;
        Ok(Self {
            writer: BufWriter::with_capacity(64 * 1024, file),
            ticks_written: 0,
        })
    }

    fn record(&mut self, tick: &TickRecord) {
        let result = serde_json::to_writer(&mut self.writer, tick)
            .map_err(|e| e.to_string())
            .and_then(|_| self.writer.write_all(b"\n").map_err(|e| e.to_string()));
        if let Err(e) = result {
            if self.ticks_written == 0 {
                tracing::warn!("control trace: failed to write tick record: {e}");
            }
        }
        self.ticks_written += 1;
    }

    fn flush(&mut self) {
        let _ = self.writer.flush();
    }
}

impl Drop for JsonlTraceRecorder {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}
