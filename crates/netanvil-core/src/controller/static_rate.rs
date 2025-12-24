use netanvil_types::{ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision};

/// Rate controller that maintains a constant request rate.
pub struct StaticRateController {
    rps: f64,
}

impl StaticRateController {
    pub fn new(rps: f64) -> Self {
        Self { rps }
    }
}

impl RateController for StaticRateController {
    fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
        RateDecision {
            target_rps: self.rps,
        }
    }

    fn current_rate(&self) -> f64 {
        self.rps
    }

    fn set_rate(&mut self, rps: f64) {
        self.rps = rps;
    }

    fn controller_info(&self) -> ControllerInfo {
        ControllerInfo {
            controller_type: ControllerType::Static,
            current_rps: self.rps,
            editable_actions: vec![
                "set_rate".into(),
                "set_max_rps".into(),
            ],
            params: serde_json::json!({
                "rps": self.rps,
            }),
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "set_rate" => {
                let rps = params.get("rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'rps' field")?;
                let old = self.rps;
                self.set_rate(rps);
                Ok(serde_json::json!({
                    "ok": true,
                    "previous_rps": old,
                    "new_rps": self.rps,
                }))
            }
            "set_max_rps" => {
                // Static controller doesn't have a max_rps, but accept for consistency
                let max = params.get("max_rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'max_rps' field")?;
                self.set_max_rps(max);
                Ok(serde_json::json!({
                    "ok": true,
                    "new_max_rps": max,
                }))
            }
            _ => Err(format!(
                "action '{}' is not valid for controller type 'static'", action
            )),
        }
    }
}
