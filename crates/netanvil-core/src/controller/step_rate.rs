use std::time::{Duration, Instant};

use netanvil_types::{ControllerInfo, ControllerType, MetricsSummary, RateController, RateDecision};


/// Rate controller that changes rates at predefined time offsets.
///
/// Steps are `(offset_from_start, rps)` pairs. The controller looks up
/// the current step based on elapsed time since construction.
pub struct StepRateController {
    steps: Vec<(Duration, f64)>,
    current_rps: f64,
    start_time: Instant,
}

impl StepRateController {
    pub fn new(steps: Vec<(Duration, f64)>) -> Self {
        assert!(
            !steps.is_empty(),
            "step controller requires at least one step"
        );
        let initial_rps = steps[0].1;
        Self {
            steps,
            current_rps: initial_rps,
            start_time: Instant::now(),
        }
    }

    /// Create with an explicit start time (for testing).
    pub fn with_start_time(steps: Vec<(Duration, f64)>, start_time: Instant) -> Self {
        assert!(
            !steps.is_empty(),
            "step controller requires at least one step"
        );
        let initial_rps = steps[0].1;
        Self {
            steps,
            current_rps: initial_rps,
            start_time,
        }
    }

    /// Jump to the given step index, adjusting the internal time reference
    /// so the step plays out from its full duration.
    pub fn jump_to_step(&mut self, index: usize) -> Result<f64, String> {
        if index >= self.steps.len() {
            return Err(format!(
                "step_index {} out of range (0..{})",
                index,
                self.steps.len()
            ));
        }
        self.current_rps = self.steps[index].1;
        // Adjust start_time so that elapsed matches this step's offset,
        // causing the schedule to continue from here.
        self.start_time = Instant::now() - self.steps[index].0;
        Ok(self.current_rps)
    }

    fn current_step_index(&self) -> usize {
        let elapsed = self.start_time.elapsed();
        let mut idx = 0;
        for (i, &(offset, _)) in self.steps.iter().enumerate() {
            if elapsed >= offset {
                idx = i;
            } else {
                break;
            }
        }
        idx
    }

    fn update_from_time(&mut self) {
        let elapsed = self.start_time.elapsed();
        let mut rps = self.steps[0].1;
        for &(offset, step_rps) in &self.steps {
            if elapsed >= offset {
                rps = step_rps;
            } else {
                break;
            }
        }
        self.current_rps = rps;
    }
}

impl RateController for StepRateController {
    fn update(&mut self, _summary: &MetricsSummary) -> RateDecision {
        self.update_from_time();
        RateDecision {
            target_rps: self.current_rps,
        }
    }

    fn current_rate(&self) -> f64 {
        self.current_rps
    }

    fn set_rate(&mut self, rps: f64) {
        // Override the current step; next time-based update will reclaim
        self.current_rps = rps;
    }

    fn controller_info(&self) -> ControllerInfo {
        let steps_json: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|(offset, rps)| {
                serde_json::json!({
                    "offset_secs": offset.as_secs_f64(),
                    "rps": rps,
                })
            })
            .collect();

        ControllerInfo {
            controller_type: ControllerType::Step,
            current_rps: self.current_rps,
            editable_actions: vec![
                "jump_to_step".into(),
                "set_step_rps".into(),
                "set_max_rps".into(),
            ],
            params: serde_json::json!({
                "current_step_index": self.current_step_index(),
                "steps": steps_json,
            }),
        }
    }

    fn apply_update(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match action {
            "jump_to_step" => {
                let index = params.get("step_index")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing 'step_index' field")? as usize;
                let rps = self.jump_to_step(index)?;
                Ok(serde_json::json!({
                    "ok": true,
                    "step_index": index,
                    "step_rps": rps,
                }))
            }
            "set_step_rps" => {
                let rps = params.get("rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'rps' field")?;
                self.set_rate(rps);
                Ok(serde_json::json!({
                    "ok": true,
                    "note": "overrides current step until next time boundary",
                }))
            }
            "set_max_rps" => {
                let max = params.get("max_rps").and_then(|v| v.as_f64())
                    .ok_or("missing 'max_rps' field")?;
                self.set_max_rps(max);
                Ok(serde_json::json!({
                    "ok": true,
                    "new_max_rps": max,
                }))
            }
            _ => Err(format!(
                "action '{}' is not valid for controller type 'step'", action
            )),
        }
    }
}
