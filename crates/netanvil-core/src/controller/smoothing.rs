//! Per-signal smoothing strategies.
//!
//! Smoothing is a property of the signal being smoothed, not the controller
//! using it. Latency p99 is outlier-prone → median. Error rate is approximately
//! Gaussian → EMA. Configured per-constraint.

use std::collections::VecDeque;

/// Signal smoother, chosen per-constraint based on signal character.
#[derive(Debug, Clone)]
pub enum Smoother {
    /// No smoothing. Passes through raw values.
    None { value: f64 },
    /// Exponential moving average. Good for approximately Gaussian signals
    /// (error rate, throughput). Alpha in (0, 1]; higher = more responsive.
    Ema {
        alpha: f64,
        value: f64,
        initialized: bool,
    },
    /// Sliding-window median. Robust to outliers. Good for heavy-tailed
    /// signals (latency percentiles). Window size is in ticks.
    Median {
        window: VecDeque<f64>,
        size: usize,
        cached: f64,
    },
}

impl Smoother {
    /// Create a no-smoothing passthrough.
    pub fn none() -> Self {
        Self::None { value: 0.0 }
    }

    /// Create an EMA smoother with the given alpha (responsiveness).
    pub fn ema(alpha: f64) -> Self {
        Self::Ema {
            alpha: alpha.clamp(0.0, 1.0),
            value: 0.0,
            initialized: false,
        }
    }

    /// Create a sliding-median smoother with the given window size.
    pub fn median(size: usize) -> Self {
        let size = size.max(1);
        Self::Median {
            window: VecDeque::with_capacity(size + 1),
            size,
            cached: 0.0,
        }
    }

    /// Feed a raw value and return the smoothed value.
    pub fn smooth(&mut self, raw: f64) -> f64 {
        match self {
            Self::None { value } => {
                *value = raw;
                raw
            }
            Self::Ema {
                alpha,
                value,
                initialized,
            } => {
                if !*initialized {
                    *value = raw;
                    *initialized = true;
                } else {
                    *value = *alpha * raw + (1.0 - *alpha) * *value;
                }
                *value
            }
            Self::Median {
                window,
                size,
                cached,
            } => {
                window.push_back(raw);
                while window.len() > *size {
                    window.pop_front();
                }
                let mut sorted: Vec<f64> = window.iter().copied().collect();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                *cached = sorted[sorted.len() / 2];
                *cached
            }
        }
    }

    /// Return the current smoothed value without feeding a new sample.
    pub fn current(&self) -> f64 {
        match self {
            Self::None { value } | Self::Ema { value, .. } => *value,
            Self::Median { cached, .. } => *cached,
        }
    }

    /// Minimum number of samples needed before the smoother's output is meaningful.
    pub fn min_samples(&self) -> usize {
        match self {
            Self::None { .. } => 1,
            Self::Ema { .. } => 1,
            Self::Median { size, .. } => *size,
        }
    }

    /// Reset to initial state.
    pub fn reset(&mut self) {
        match self {
            Self::None { value } => *value = 0.0,
            Self::Ema {
                value, initialized, ..
            } => {
                *value = 0.0;
                *initialized = false;
            }
            Self::Median { window, cached, .. } => {
                window.clear();
                *cached = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_passes_through() {
        let mut s = Smoother::none();
        assert_eq!(s.smooth(42.0), 42.0);
        assert_eq!(s.smooth(99.0), 99.0);
        assert_eq!(s.current(), 99.0);
    }

    #[test]
    fn ema_first_sample_is_raw() {
        let mut s = Smoother::ema(0.3);
        assert_eq!(s.smooth(100.0), 100.0);
    }

    #[test]
    fn ema_subsequent_samples_blend() {
        let mut s = Smoother::ema(0.3);
        s.smooth(100.0);
        let v = s.smooth(200.0);
        // 0.3 * 200 + 0.7 * 100 = 60 + 70 = 130
        assert!((v - 130.0).abs() < 0.01);
    }

    #[test]
    fn ema_handles_zero_first_sample() {
        // Bug fix: first sample of 0.0 should initialize, not stay uninitialized.
        let mut s = Smoother::ema(0.3);
        assert_eq!(s.smooth(0.0), 0.0);
        // Second sample should blend, not anchor
        let v = s.smooth(100.0);
        // 0.3 * 100 + 0.7 * 0 = 30
        assert!((v - 30.0).abs() < 0.01);
    }

    #[test]
    fn ema_reset_clears() {
        let mut s = Smoother::ema(0.3);
        s.smooth(100.0);
        s.smooth(200.0);
        s.reset();
        assert_eq!(s.current(), 0.0);
        assert_eq!(s.smooth(50.0), 50.0);
    }

    #[test]
    fn median_single_sample() {
        let mut s = Smoother::median(3);
        assert_eq!(s.smooth(100.0), 100.0);
    }

    #[test]
    fn median_rejects_outlier() {
        let mut s = Smoother::median(3);
        s.smooth(100.0);
        s.smooth(100.0);
        let v = s.smooth(999.0); // outlier
        assert_eq!(v, 100.0); // median of [100, 100, 999] = 100
    }

    #[test]
    fn median_window_slides() {
        let mut s = Smoother::median(3);
        s.smooth(10.0);
        s.smooth(20.0);
        s.smooth(30.0);
        assert_eq!(s.current(), 20.0);

        s.smooth(40.0); // window is now [20, 30, 40]
        assert_eq!(s.current(), 30.0);
    }

    #[test]
    fn median_current_uses_cache() {
        let mut s = Smoother::median(3);
        s.smooth(10.0);
        s.smooth(20.0);
        s.smooth(30.0);
        // current() should return cached median, no recomputation
        assert_eq!(s.current(), 20.0);
        assert_eq!(s.current(), 20.0); // idempotent
    }

    #[test]
    fn median_reset_clears() {
        let mut s = Smoother::median(3);
        s.smooth(10.0);
        s.smooth(20.0);
        s.reset();
        assert_eq!(s.current(), 0.0);
    }
}
