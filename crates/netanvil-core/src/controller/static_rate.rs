use netanvil_types::{MetricsSummary, RateController, RateDecision};

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
}
