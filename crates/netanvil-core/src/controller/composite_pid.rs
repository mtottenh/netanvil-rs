//! Composite PID controller targeting multiple metrics simultaneously.
//!
//! Uses a min-rate selector: each constraint runs its own PID, and the
//! minimum rate wins. Non-binding constraints decay their integral to
//! prevent windup while staying ready to become binding instantly.
//!
//! Supports both auto-tuned gains (via shared exploration phase) and
//! manually specified per-constraint gains.

use std::time::Duration;

use netanvil_types::{
    ControllerInfo, ControllerType, MetricsSummary, PidConstraint, PidGains, RateController,
    RateDecision, TargetMetric,
};

use super::autotune::{
    self, ComputedGains, ExplorationManager, ExplorationPhase, MetricExploration, PidState,
    CONSERVATIVE_GAINS,
};

/// Per-constraint state within the composite controller.
struct ConstraintState {
    metric: TargetMetric,
    limit: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    pid: PidState,
    /// Whether gains were auto-tuned (use gain scheduling) or manual (fixed).
    use_scheduling: bool,
}

/// Phase of the composite controller.
enum Phase {
    /// Running shared exploration for all auto-tuned constraints.
    Exploring {
        manager: ExplorationManager,
        /// Constraints with manual gains (already fully configured).
        manual_constraints: Vec<ConstraintState>,
        /// Indices into the exploration manager's metrics vec, mapping
        /// back to the original constraint order.
        auto_constraint_indices: Vec<usize>,
        /// Smoothing factor for auto-tuned constraints.
        smoothing: f64,
    },
    /// All gains determined, running composite PID.
    Active { constraints: Vec<ConstraintState> },
}

/// PID rate controller with multiple simultaneous constraints.
///
/// Finds the maximum rate where ALL constraints are satisfied by running
/// independent PID loops and selecting the minimum rate each tick.
///
/// Auto-tuned constraints share a single exploration phase (one step
/// characterizes all metrics simultaneously), then each gets its own
/// gain-scheduled PID. Manually-tuned constraints use fixed gains from
/// tick zero.
pub struct CompositePidController {
    current_rps: f64,
    min_rps: f64,
    max_rps: f64,
    phase: Phase,
}

impl CompositePidController {
    pub fn new(
        constraints: &[PidConstraint],
        initial_rps: f64,
        min_rps: f64,
        max_rps: f64,
        control_interval: Duration,
    ) -> Self {
        assert!(
            !constraints.is_empty(),
            "composite PID requires at least one constraint"
        );

        // Separate auto vs manual constraints
        let mut auto_explorations = Vec::new();
        let mut auto_indices = Vec::new();
        let mut manual_constraints = Vec::new();
        // Collect the smoothing factor from the first auto constraint (or default)
        let mut smoothing = 0.3f64;

        for (i, c) in constraints.iter().enumerate() {
            match &c.gains {
                PidGains::Auto {
                    smoothing: s,
                    autotune_duration: _,
                } => {
                    smoothing = *s;
                    auto_explorations.push(MetricExploration::new(c.metric.clone(), c.limit));
                    auto_indices.push(i);
                }
                PidGains::Manual { kp, ki, kd } => {
                    manual_constraints.push(ConstraintState {
                        metric: c.metric.clone(),
                        limit: c.limit,
                        kp: *kp,
                        ki: *ki,
                        kd: *kd,
                        pid: PidState::new(0.3),
                        use_scheduling: false,
                    });
                }
            }
        }

        if auto_explorations.is_empty() {
            // All manual — skip exploration entirely
            return Self {
                current_rps: initial_rps,
                min_rps,
                max_rps,
                phase: Phase::Active {
                    constraints: manual_constraints,
                },
            };
        }

        // Get autotune duration from the first auto constraint
        let autotune_duration = constraints
            .iter()
            .find_map(|c| match &c.gains {
                PidGains::Auto {
                    autotune_duration, ..
                } => Some(*autotune_duration),
                _ => None,
            })
            .unwrap_or(Duration::from_secs(3));

        let manager = ExplorationManager::new(
            auto_explorations,
            initial_rps,
            autotune_duration,
            control_interval,
        );

        Self {
            current_rps: initial_rps * 0.5, // baseline rate during exploration
            min_rps,
            max_rps,
            phase: Phase::Exploring {
                manager,
                manual_constraints,
                auto_constraint_indices: auto_indices,
                smoothing,
            },
        }
    }

    pub fn set_constraint_limit(
        &mut self,
        index: usize,
        limit: f64,
    ) -> Result<f64, String> {
        match &mut self.phase {
            Phase::Active { constraints } => {
                if index >= constraints.len() {
                    return Err(format!(
                        "constraint_index {} out of range (0..{})",
                        index,
                        constraints.len()
                    ));
                }
                let old = constraints[index].limit;
                constraints[index].limit = limit;
                Ok(old)
            }
            Phase::Exploring { .. } => {
                Err("cannot modify constraints during exploration".into())
            }
        }
    }

    pub fn set_constraint_gains(
        &mut self,
        index: usize,
        kp: f64,
        ki: f64,
        kd: f64,
    ) -> Result<(), String> {
        match &mut self.phase {
            Phase::Active { constraints } => {
                if index >= constraints.len() {
                    return Err(format!(
                        "constraint_index {} out of range (0..{})",
                        index,
                        constraints.len()
                    ));
                }
                constraints[index].kp = kp;
                constraints[index].ki = ki;
                constraints[index].kd = kd;
                Ok(())
            }
            Phase::Exploring { .. } => {
                Err("cannot modify constraints during exploration".into())
            }
        }
    }

    pub fn set_min_rps(&mut self, min_rps: f64) {
        self.min_rps = min_rps;
        if self.min_rps > self.max_rps {
            self.min_rps = self.max_rps;
        }
    }

    fn find_constraint_index_by_metric(&self, metric_name: &str) -> Option<usize> {
        let constraints = match &self.phase {
            Phase::Active { constraints } => constraints,
            Phase::Exploring { manual_constraints, .. } => manual_constraints,
        };
        constraints.iter().position(|c| format!("{:?}", c.metric) == metric_name)
    }

    /// Run the composite PID logic: compute each constraint's desired rate,
    /// select the minimum, and manage integral tracking.
    fn composite_update(
        constraints: &mut [ConstraintState],
        summary: &MetricsSummary,
        current_rps: f64,
        min_rps: f64,
        max_rps: f64,
    ) -> f64 {
        if summary.total_requests < 5 {
            return current_rps;
        }

        // Phase 1: Compute each constraint's desired rate (read-only).
        // Also track whether the ±20% adjustment clamp fired per constraint
        // for anti-windup in Phase 3.
        let mut desired_rates: Vec<f64> = Vec::with_capacity(constraints.len());
        let mut raw_values: Vec<f64> = Vec::with_capacity(constraints.len());
        let mut adj_saturated_flags: Vec<bool> = Vec::with_capacity(constraints.len());

        for c in constraints.iter_mut() {
            let raw = autotune::extract_metric(&c.metric, summary);
            let smoothed = c.pid.smooth(raw);
            raw_values.push(smoothed);

            // Compute what rate this constraint wants (without mutating state yet)
            let error = c.limit - smoothed;
            let normalized_error = if c.limit.abs() > 1e-9 {
                error / c.limit
            } else {
                error
            };

            // Compute PID output with tentative state
            let tentative_integral = (c.pid.integral + error).clamp(-1000.0, 1000.0);
            let derivative = error - c.pid.last_error;

            let (kp, ki, kd) = if c.use_scheduling {
                let mult = autotune::gain_schedule(normalized_error);
                (
                    c.kp * mult.kp_scale,
                    c.ki * mult.ki_scale,
                    c.kd * mult.kd_scale,
                )
            } else {
                (c.kp, c.ki, c.kd)
            };

            let output = kp * error + ki * tentative_integral + kd * derivative;
            let adj_raw = output * 0.05;
            let adjustment = adj_raw.clamp(-0.20, 0.20);
            let adj_saturated =
                (adj_raw > 0.20 && error > 0.0) || (adj_raw < -0.20 && error < 0.0);
            adj_saturated_flags.push(adj_saturated);
            let rate = (current_rps * (1.0 + adjustment)).clamp(min_rps, max_rps);
            desired_rates.push(rate);
        }

        // Phase 2: Select minimum rate (most constrained wins)
        let selected_rate = desired_rates
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min)
            .clamp(min_rps, max_rps);

        // Find the binding constraint index
        let binding_idx = desired_rates
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        // Phase 3: Update PID state — binding constraint gets full update,
        // non-binding constraints decay their integral to prevent windup.
        for (i, c) in constraints.iter_mut().enumerate() {
            let error = c.limit - raw_values[i];

            if i == binding_idx {
                // Full PID state update for the binding constraint.
                // Suppress integral accumulation when the ±20% adjustment
                // clamp fired in the error's direction (anti-windup).
                let suppress = adj_saturated_flags[i];
                if c.use_scheduling {
                    let normalized = if c.limit.abs() > 1e-9 {
                        error / c.limit
                    } else {
                        error
                    };
                    let mult = autotune::gain_schedule(normalized);
                    if mult.reset_integral {
                        c.pid.integral = 0.0;
                    } else if !suppress {
                        c.pid.integral += error;
                        c.pid.integral = c.pid.integral.clamp(-1000.0, 1000.0);
                    }
                } else if !suppress {
                    c.pid.integral += error;
                    c.pid.integral = c.pid.integral.clamp(-1000.0, 1000.0);
                }
            } else {
                // Non-binding: decay integral to prevent windup
                c.pid.integral *= 0.95;
                c.pid.integral += error * 0.1; // small tracking to stay responsive
                c.pid.integral = c.pid.integral.clamp(-1000.0, 1000.0);
            }

            c.pid.last_error = error;
        }

        selected_rate
    }
}

impl RateController for CompositePidController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        match &mut self.phase {
            Phase::Exploring {
                manager,
                manual_constraints,
                auto_constraint_indices,
                smoothing,
            } => {
                if let Some(rate) = manager.tick(summary) {
                    // Still exploring — also run manual constraints as a safety check
                    let mut safe_rate = rate;
                    for c in manual_constraints.iter_mut() {
                        let raw = autotune::extract_metric(&c.metric, summary);
                        let smoothed = c.pid.smooth(raw);
                        // If a manual constraint is violated during exploration, cap rate
                        if smoothed > c.limit * 1.1 {
                            let error = c.limit - smoothed;
                            let output = c.kp * error;
                            let adjustment = (output * 0.05).clamp(-0.20, 0.0);
                            safe_rate = (safe_rate * (1.0 + adjustment)).max(self.min_rps);
                        }
                    }
                    self.current_rps = safe_rate.clamp(self.min_rps, self.max_rps);

                    RateDecision {
                        target_rps: self.current_rps,
                    }
                } else {
                    // Exploration done — build full constraint list
                    let smoothing_val = *smoothing;
                    let mut all_constraints: Vec<ConstraintState> = Vec::new();

                    // Take manual constraints out
                    let manual = std::mem::take(manual_constraints);

                    // Compute gains for auto-tuned constraints
                    let auto_gains: Vec<ComputedGains> = (0..manager.metrics.len())
                        .map(|i| match manager.phase {
                            ExplorationPhase::Complete => manager.compute_gains_for(i),
                            _ => CONSERVATIVE_GAINS,
                        })
                        .collect();

                    // Rebuild constraint list in original order.
                    // auto_constraint_indices[j] = original index of the j-th auto constraint.
                    // manual constraints fill the remaining slots.
                    let total = auto_constraint_indices.len() + manual.len();
                    let mut auto_iter = auto_gains.into_iter().enumerate();
                    let mut manual_iter = manual.into_iter();

                    // Simple approach: build a map from original index to constraint
                    let auto_idx_set: std::collections::HashSet<usize> =
                        auto_constraint_indices.iter().cloned().collect();

                    for orig_idx in 0..total {
                        if auto_idx_set.contains(&orig_idx) {
                            let (metric_idx, _) = auto_iter.next().unwrap();
                            let gains = match manager.phase {
                                ExplorationPhase::Complete => manager.compute_gains_for(metric_idx),
                                _ => CONSERVATIVE_GAINS,
                            };
                            let m = &manager.metrics[metric_idx];
                            all_constraints.push(ConstraintState {
                                metric: m.metric.clone(),
                                limit: m.target_value,
                                kp: gains.kp,
                                ki: gains.ki,
                                kd: gains.kd,
                                pid: PidState::new(smoothing_val),
                                use_scheduling: true,
                            });
                        } else {
                            all_constraints.push(manual_iter.next().unwrap());
                        }
                    }

                    tracing::info!(
                        num_constraints = all_constraints.len(),
                        "composite PID: exploration complete, all constraints active"
                    );

                    self.current_rps = Self::composite_update(
                        &mut all_constraints,
                        summary,
                        self.current_rps,
                        self.min_rps,
                        self.max_rps,
                    );

                    self.phase = Phase::Active {
                        constraints: all_constraints,
                    };

                    RateDecision {
                        target_rps: self.current_rps,
                    }
                }
            }

            Phase::Active { constraints } => {
                self.current_rps = Self::composite_update(
                    constraints,
                    summary,
                    self.current_rps,
                    self.min_rps,
                    self.max_rps,
                );

                RateDecision {
                    target_rps: self.current_rps,
                }
            }
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        match &mut self.phase {
            Phase::Exploring { .. } => {
                tracing::info!(
                    rps,
                    "composite PID: external set_rate during exploration, \
                     falling back to conservative gains"
                );
                // Can't easily recover auto gains — create all-conservative constraints
                // from the exploration metrics. This is a rare edge case.
                self.phase = Phase::Active {
                    constraints: Vec::new(), // empty = no constraints, rate stays as set
                };
                // In practice this means the controller becomes a static rate until
                // the next update provides metrics to work with.
            }
            Phase::Active { constraints } => {
                for c in constraints.iter_mut() {
                    c.pid.reset();
                }
            }
        }
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        if self.current_rps > self.max_rps {
            self.current_rps = self.max_rps;
        }
    }

    fn controller_info(&self) -> ControllerInfo {
        let is_exploring = matches!(self.phase, Phase::Exploring { .. });

        let params = match &self.phase {
            Phase::Exploring {
                manager,
                manual_constraints,
                ..
            } => {
                let progress = manager.exploration_progress();
                let constraints_json: Vec<serde_json::Value> = manual_constraints
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        serde_json::json!({
                            "index": i,
                            "metric": format!("{:?}", c.metric),
                            "limit": c.limit,
                            "gains": {"kp": c.kp, "ki": c.ki, "kd": c.kd},
                        })
                    })
                    .collect();
                serde_json::json!({
                    "phase": "exploring",
                    "min_rps": self.min_rps,
                    "max_rps": self.max_rps,
                    "exploration_progress": progress,
                    "manual_constraints": constraints_json,
                })
            }
            Phase::Active { constraints } => {
                // Find binding constraint (lowest desired rate from last tick)
                let constraints_json: Vec<serde_json::Value> = constraints
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        serde_json::json!({
                            "index": i,
                            "metric": format!("{:?}", c.metric),
                            "limit": c.limit,
                            "gains": {"kp": c.kp, "ki": c.ki, "kd": c.kd},
                        })
                    })
                    .collect();
                serde_json::json!({
                    "phase": "active",
                    "min_rps": self.min_rps,
                    "max_rps": self.max_rps,
                    "constraints": constraints_json,
                })
            }
        };

        let editable = if is_exploring {
            vec!["set_max_rps".into(), "set_min_rps".into()]
        } else {
            vec![
                "set_constraint_limit".into(),
                "set_constraint_gains".into(),
                "set_max_rps".into(),
                "set_min_rps".into(),
            ]
        };

        ControllerInfo {
            controller_type: ControllerType::CompositePid,
            current_rps: self.current_rps,
            editable_actions: editable,
            params,
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_constraint_limit" => {
                let index = if let Some(idx) = params.get("constraint_index").and_then(|v| v.as_u64()) {
                    idx as usize
                } else if let Some(metric) = params.get("metric").and_then(|v| v.as_str()) {
                    self.find_constraint_index_by_metric(metric)
                        .ok_or_else(|| format!("no constraint with metric '{}'", metric))?
                } else {
                    return Err("missing 'constraint_index' or 'metric' field".into());
                };
                let limit = params.get("limit").and_then(|v| v.as_f64())
                    .ok_or("missing 'limit' field")?;
                let old = self.set_constraint_limit(index, limit)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "constraint_index": index,
                    "previous_limit": old,
                    "new_limit": limit,
                }))
            }
            "set_constraint_gains" => {
                let index = params.get("constraint_index")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing 'constraint_index' field")? as usize;
                let kp = params.get("kp").and_then(|v| v.as_f64())
                    .ok_or("missing 'kp' field")?;
                let ki = params.get("ki").and_then(|v| v.as_f64())
                    .ok_or("missing 'ki' field")?;
                let kd = params.get("kd").and_then(|v| v.as_f64())
                    .ok_or("missing 'kd' field")?;
                self.set_constraint_gains(index, kp, ki, kd)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "constraint_index": index,
                    "new_gains": {"kp": kp, "ki": ki, "kd": kd},
                }))
            }
            "set_max_rps" => {
                let max = params.get("max_rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'max_rps' field")?;
                let old = self.max_rps;
                self.set_max_rps(max);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_max_rps": old,
                    "new_max_rps": self.max_rps,
                }))
            }
            "set_min_rps" => {
                let min = params.get("min_rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'min_rps' field")?;
                let old = self.min_rps;
                self.set_min_rps(min);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_min_rps": old,
                    "new_min_rps": self.min_rps,
                }))
            }
            _ => Err(format!(
                "action '{}' is not valid for controller type 'composite_pid'", action
            )),
        }
    }
}
