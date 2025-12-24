use netanvil_types::{
    ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision, TargetMetric,
};

/// Resolved PID gains (kp, ki, kd) for use with PID controllers.
pub struct PidGainValues {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
}

/// PID rate controller that adjusts request rate to maintain a target metric.
///
/// Uses the shared `pid_step_fixed()` function for PID computation,
/// ensuring consistent anti-windup logic across all PID variants.
pub struct PidRateController {
    target_metric: TargetMetric,
    target_value: f64,
    current_rps: f64,
    min_rps: f64,
    max_rps: f64,
    kp: f64,
    ki: f64,
    kd: f64,
    state: super::autotune::PidState,
}

impl PidRateController {
    pub fn new(
        target_metric: TargetMetric,
        target_value: f64,
        initial_rps: f64,
        min_rps: f64,
        max_rps: f64,
        gains: PidGainValues,
    ) -> Self {
        Self {
            target_metric,
            target_value,
            current_rps: initial_rps,
            min_rps,
            max_rps,
            kp: gains.kp,
            ki: gains.ki,
            kd: gains.kd,
            state: super::autotune::PidState::new(1.0), // no smoothing for manual PID
        }
    }

    fn extract_current_value(&self, summary: &MetricsSummary) -> f64 {
        crate::controller::autotune::extract_metric(&self.target_metric, summary)
    }

    pub fn set_target_value(&mut self, value: f64) {
        self.target_value = value;
        // Do NOT reset integrator — let the PID adapt naturally.
    }

    pub fn set_gains(&mut self, kp: f64, ki: f64, kd: f64) {
        self.kp = kp;
        self.ki = ki;
        self.kd = kd;
        // Do NOT reset integrator — the operator may be fine-tuning.
    }

    pub fn reset_integrator(&mut self) {
        self.state.reset();
    }

    pub fn set_min_rps(&mut self, min_rps: f64) {
        self.min_rps = min_rps;
        if self.min_rps > self.max_rps {
            self.min_rps = self.max_rps;
        }
    }
}

impl RateController for PidRateController {
    fn update(&mut self, summary: &MetricsSummary) -> RateDecision {
        // Need enough samples for meaningful feedback
        if summary.total_requests < 5 {
            return RateDecision {
                target_rps: self.current_rps,
            };
        }

        let current_value = self.extract_current_value(summary);

        self.current_rps = super::autotune::pid_step_fixed(
            &super::autotune::PidStepInput {
                current_value,
                target_value: self.target_value,
                current_rps: self.current_rps,
                min_rps: self.min_rps,
                max_rps: self.max_rps,
                kp: self.kp,
                ki: self.ki,
                kd: self.kd,
            },
            &mut self.state,
        );

        RateDecision {
            target_rps: self.current_rps,
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.current_rps = rps.clamp(self.min_rps, self.max_rps);
        // Reset integral to avoid fighting the new setpoint
        self.state.reset();
    }

    fn set_max_rps(&mut self, max_rps: f64) {
        self.max_rps = max_rps.max(self.min_rps);
        if self.current_rps > self.max_rps {
            self.current_rps = self.max_rps;
        }
    }

    fn controller_info(&self) -> ControllerInfo {
        ControllerInfo {
            controller_type: ControllerType::Pid,
            current_rps: self.current_rps,
            editable_actions: vec![
                "set_target_value".into(),
                "set_gains".into(),
                "reset_integrator".into(),
                "set_max_rps".into(),
                "set_min_rps".into(),
            ],
            params: serde_json::json!({
                "target_metric": format!("{:?}", self.target_metric),
                "target_value": self.target_value,
                "min_rps": self.min_rps,
                "max_rps": self.max_rps,
                "gains": {
                    "kp": self.kp,
                    "ki": self.ki,
                    "kd": self.kd,
                },
                "integrator": self.state.integral,
                "last_error": self.state.last_error,
            }),
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_target_value" => {
                let value = params.get("value").and_then(|v| v.as_f64())
                    .ok_or("missing 'value' field")?;
                let old = self.target_value;
                self.set_target_value(value);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_target_value": old,
                    "new_target_value": value,
                    "note": "integrator preserved",
                }))
            }
            "set_gains" => {
                let kp = params.get("kp").and_then(|v| v.as_f64())
                    .ok_or("missing 'kp' field")?;
                let ki = params.get("ki").and_then(|v| v.as_f64())
                    .ok_or("missing 'ki' field")?;
                let kd = params.get("kd").and_then(|v| v.as_f64())
                    .ok_or("missing 'kd' field")?;
                let old = serde_json::json!({
                    "kp": self.kp, "ki": self.ki, "kd": self.kd,
                });
                self.set_gains(kp, ki, kd);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_gains": old,
                    "new_gains": {"kp": kp, "ki": ki, "kd": kd},
                }))
            }
            "reset_integrator" => {
                self.reset_integrator();
                Ok(serde_json::json!({
                    "ok": true,
                    "note": "PID integrator and derivative state reset",
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
                "action '{}' is not valid for controller type 'pid'", action
            )),
        }
    }
}
