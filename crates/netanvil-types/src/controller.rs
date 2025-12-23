//! Types for mid-test rate controller introspection and parameter updates.

use serde::{Deserialize, Serialize};

/// Controller type discriminant for introspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerType {
    Static,
    Step,
    Pid,
    PidAutotune,
    CompositePid,
    Ramp,
}

/// Structured description of a rate controller's current state.
/// Returned by `RateController::controller_info()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerInfo {
    /// Controller type discriminant.
    pub controller_type: ControllerType,
    /// Current output RPS.
    pub current_rps: f64,
    /// Actions that can be sent via PUT /controller.
    pub editable_actions: Vec<String>,
    /// Controller-specific parameters (opaque JSON).
    pub params: serde_json::Value,
}

/// Full controller state returned by GET /tests/{id}/controller.
/// Wraps `ControllerInfo` with hold state from the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerView {
    #[serde(flatten)]
    pub info: ControllerInfo,
    /// Whether the rate is currently held.
    pub held: bool,
    /// The held RPS value (if held).
    pub held_rps: Option<f64>,
}

/// Hold state for coordinator-level rate override.
#[derive(Debug, Clone, Default)]
pub enum HoldState {
    /// Normal operation: controller.update() runs each tick.
    #[default]
    Released,
    /// Rate is held: skip controller.update(), distribute held_rps.
    Held { rps: f64 },
}

/// Commands for hold/release sent from API to coordinator.
#[derive(Debug, Clone)]
pub enum HoldCommand {
    Hold(f64),
    Release,
}
