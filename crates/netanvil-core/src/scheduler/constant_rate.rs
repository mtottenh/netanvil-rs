use std::time::{Duration, Instant};

use netanvil_types::RequestScheduler;

/// Scheduler that generates requests at constant intervals.
///
/// Given a target rate of R requests/second, the interval between
/// consecutive requests is 1/R seconds. The scheduler advances
/// `next_time` by this interval on each call, never drifting from
/// the ideal schedule regardless of actual dispatch timing.
pub struct ConstantRateScheduler {
    interval: Duration,
    next_time: Instant,
    end_time: Option<Instant>,
}

impl ConstantRateScheduler {
    pub fn new(rps: f64, start_time: Instant, duration: Option<Duration>) -> Self {
        Self {
            interval: interval_from_rps(rps),
            next_time: start_time,
            end_time: duration.map(|d| start_time + d),
        }
    }
}

impl RequestScheduler for ConstantRateScheduler {
    fn next_request_time(&mut self) -> Option<Instant> {
        if let Some(end) = self.end_time {
            if self.next_time >= end {
                return None;
            }
        }
        let t = self.next_time;
        self.next_time += self.interval;
        Some(t)
    }

    fn update_rate(&mut self, rps: f64) {
        self.interval = interval_from_rps(rps);
        // Don't reset next_time — smooth transition from wherever we are
    }
}

fn interval_from_rps(rps: f64) -> Duration {
    if rps <= 0.0 {
        Duration::from_secs(1)
    } else {
        Duration::from_secs_f64(1.0 / rps)
    }
}
