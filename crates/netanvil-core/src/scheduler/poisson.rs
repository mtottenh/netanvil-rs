use std::time::{Duration, Instant};

use rand::rngs::SmallRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Exp};

use netanvil_types::RequestScheduler;

/// Scheduler that generates requests following a Poisson process.
///
/// Inter-arrival times are exponentially distributed with mean 1/rate.
/// This models realistic independent user arrivals — the standard
/// assumption in queuing theory.
pub struct PoissonScheduler {
    rate: f64,
    next_time: Instant,
    end_time: Option<Instant>,
    rng: SmallRng,
}

impl PoissonScheduler {
    pub fn new(rps: f64, start_time: Instant, duration: Option<Duration>) -> Self {
        Self {
            rate: rps,
            next_time: start_time,
            end_time: duration.map(|d| start_time + d),
            rng: SmallRng::from_entropy(),
        }
    }

    /// Create with a fixed seed for deterministic testing.
    pub fn with_seed(rps: f64, start_time: Instant, duration: Option<Duration>, seed: u64) -> Self {
        Self {
            rate: rps,
            next_time: start_time,
            end_time: duration.map(|d| start_time + d),
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    fn sample_interval(&mut self) -> Duration {
        if self.rate <= 0.0 {
            return Duration::from_secs(1);
        }
        let exp = Exp::new(self.rate).unwrap_or_else(|_| Exp::new(1.0).unwrap());
        let secs = exp.sample(&mut self.rng);
        Duration::from_secs_f64(secs)
    }
}

impl RequestScheduler for PoissonScheduler {
    fn next_request_time(&mut self) -> Option<Instant> {
        if let Some(end) = self.end_time {
            if self.next_time >= end {
                return None;
            }
        }
        let t = self.next_time;
        let interval = self.sample_interval();
        self.next_time += interval;
        Some(t)
    }

    fn update_rate(&mut self, rps: f64) {
        self.rate = rps;
    }
}
