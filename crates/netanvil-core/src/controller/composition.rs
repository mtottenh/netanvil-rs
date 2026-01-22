//! Composition strategies for override control.
//!
//! A composition strategy takes N constraint evaluations and produces a single
//! rate decision. The strategy is the pluggable "selector" in override control.
//! It does NOT apply arbiter-level policies (ceiling, floor, cooldown).

use std::cmp::Ordering;

use super::constraints::ConstraintClass;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A constraint evaluation packaged for the composition strategy.
///
/// Only constraints that returned `Some(ConstraintOutput)` with a
/// non-`NoObjection` intent appear here. NoObjection and inactive
/// constraints are filtered by the arbiter before reaching the strategy.
pub struct ConstraintEval {
    /// Index into the arbiter's constraint list (for binding identification).
    pub constraint_index: usize,
    /// The rate this constraint wants (extracted from ConstraintIntent).
    pub desired_rate: f64,
    /// How stressed (for strategies that weight by severity).
    pub severity: f64,
    /// For class-aware floor application after composition.
    pub class: ConstraintClass,
}

/// Result of composition.
pub struct CompositionResult {
    /// The selected rate (before arbiter-level policy).
    pub rate: f64,
    /// Which constraint was binding (index into the arbiter's constraint
    /// list). None if no constraints had a rate opinion.
    pub binding: Option<usize>,
}

// ---------------------------------------------------------------------------
// CompositionStrategy trait
// ---------------------------------------------------------------------------

/// Composes N constraint evaluations into a single rate decision.
///
/// `default_rate` is the rate the arbiter will use if no constraints object
/// (typically the increase_rate). When `evaluations` is empty, the strategy
/// should return `default_rate` — not `current_rate`, which would stall.
pub trait CompositionStrategy {
    /// Select a rate from active constraint evaluations.
    ///
    /// - `evaluations`: constraints that have a rate opinion
    /// - `abstainers`: count of active constraints that returned NoObjection
    /// - `default_rate`: fallback when no constraints have an opinion
    fn compose(
        &mut self,
        evaluations: &[ConstraintEval],
        abstainers: usize,
        default_rate: f64,
    ) -> CompositionResult;
}

// ---------------------------------------------------------------------------
// MinSelector
// ---------------------------------------------------------------------------

/// Most conservative constraint wins. The standard override control selector.
pub struct MinSelector;

impl CompositionStrategy for MinSelector {
    fn compose(
        &mut self,
        evals: &[ConstraintEval],
        _abstainers: usize,
        default_rate: f64,
    ) -> CompositionResult {
        evals
            .iter()
            .min_by(|a, b| {
                a.desired_rate
                    .partial_cmp(&b.desired_rate)
                    .unwrap_or(Ordering::Equal)
            })
            .map(|e| CompositionResult {
                rate: e.desired_rate,
                binding: Some(e.constraint_index),
            })
            .unwrap_or(CompositionResult {
                rate: default_rate,
                binding: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(index: usize, rate: f64, severity: f64) -> ConstraintEval {
        ConstraintEval {
            constraint_index: index,
            desired_rate: rate,
            severity,
            class: ConstraintClass::OperatingPoint,
        }
    }

    #[test]
    fn min_selector_picks_lowest_rate() {
        let mut s = MinSelector;
        let evals = vec![
            eval(0, 500.0, 0.5),
            eval(1, 300.0, 1.2),
            eval(2, 800.0, 0.1),
        ];
        let r = s.compose(&evals, 0, 1000.0);
        assert_eq!(r.rate, 300.0);
        assert_eq!(r.binding, Some(1));
    }

    #[test]
    fn min_selector_empty_returns_default() {
        let mut s = MinSelector;
        let r = s.compose(&[], 0, 1100.0);
        assert_eq!(r.rate, 1100.0);
        assert!(r.binding.is_none());
    }

    #[test]
    fn min_selector_empty_returns_increase_rate_not_current() {
        let mut s = MinSelector;
        // default_rate = increase_rate (e.g., 1100), not current_rate (1000).
        // This is the fix for the "empty evals stalls the ramp" bug.
        let r = s.compose(&[], 3, 1100.0);
        assert_eq!(r.rate, 1100.0);
    }

    #[test]
    fn min_selector_single_constraint() {
        let mut s = MinSelector;
        let evals = vec![eval(0, 750.0, 0.8)];
        let r = s.compose(&evals, 2, 1100.0);
        assert_eq!(r.rate, 750.0);
        assert_eq!(r.binding, Some(0));
    }

    #[test]
    fn min_selector_tie_picks_first() {
        let mut s = MinSelector;
        let evals = vec![eval(0, 500.0, 0.5), eval(1, 500.0, 1.0)];
        let r = s.compose(&evals, 0, 1000.0);
        assert_eq!(r.rate, 500.0);
        assert_eq!(r.binding, Some(0));
    }
}
