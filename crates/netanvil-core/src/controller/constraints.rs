//! Constraint vocabulary types and generic helpers.
//!
//! These primitives are shared infrastructure for any controller that uses
//! constraint-based rate decisions. Currently used by the ramp controller;
//! available for composite PID or future controllers.

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Severity above which persistence is bypassed (immediate backoff).
pub(crate) const SEVERITY_BYPASS_PERSISTENCE: f64 = 3.0;

/// Minimum ticks after backoff before recovery can be declared.
const RECOVERY_MIN_TICKS: u32 = 3;

/// Minimum recovery samples before EMA is trusted over seed.
const RECOVERY_MIN_SAMPLES: u32 = 3;

/// Severity threshold below which recovery is declared.
const RECOVERY_SEVERITY_THRESHOLD: f64 = 0.9;

/// EMA smoothing factor for recovery time estimation.
const RECOVERY_EMA_ALPHA: f64 = 0.3;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Actions a constraint can request, ordered from least to most severe.
/// `Ord` is derived with variants in ascending severity order so that
/// `action.min(Hold)` caps severity at Hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConstraintAction {
    AllowIncrease,
    Hold,
    GentleBackoff,
    ModerateBackoff,
    HardBackoff,
}

impl ConstraintAction {
    pub(crate) fn backoff_factor(self, gentle: f64, moderate: f64, hard: f64) -> f64 {
        match self {
            Self::GentleBackoff => gentle,
            Self::ModerateBackoff => moderate,
            Self::HardBackoff => hard,
            _ => 1.0,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AllowIncrease => "increase",
            Self::Hold => "hold",
            Self::GentleBackoff => "backoff_gentle",
            Self::ModerateBackoff => "backoff_moderate",
            Self::HardBackoff => "backoff_hard",
        }
    }

    pub(crate) fn is_backoff(self) -> bool {
        matches!(
            self,
            Self::GentleBackoff | Self::ModerateBackoff | Self::HardBackoff
        )
    }
}

/// Whether a constraint bypasses or respects the known-good floor on backoff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstraintClass {
    /// Floor applies on backoff (latency, error rate, external).
    OperatingPoint,
    /// Floor is BYPASSED on backoff (timeout, in-flight drops).
    Catastrophic,
}

/// Identifies a constraint type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConstraintId {
    Timeout,
    InFlightDrop,
    Latency,
    ErrorRate,
    External(String),
}

impl ConstraintId {
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Timeout => "timeout",
            Self::InFlightDrop => "inflight_drop",
            Self::Latency => "latency",
            Self::ErrorRate => "error_rate",
            Self::External(name) => name.as_str(),
        }
    }
}

/// Result of evaluating a single constraint.
pub(crate) struct ConstraintEvaluation {
    pub(crate) id: ConstraintId,
    pub(crate) class: ConstraintClass,
    pub(crate) severity: f64,
    pub(crate) action: ConstraintAction,
}

/// Per-constraint state tracking: streaks, persistence, recovery EMA.
pub(crate) struct ConstraintState {
    /// Consecutive ticks where severity >= 1.0.
    pub(crate) violation_streak: u32,
    /// Ticks of violation required before backoff is confirmed.
    pub(crate) persistence: u32,
    /// Ticks since this signal was last seen (external constraints only).
    pub(crate) ticks_since_seen: u32,
    /// Consecutive ticks where the signal is stale/missing (external only).
    pub(crate) missing_streak: u32,
    /// EMA of observed recovery time in ticks.
    pub(crate) recovery_ema_ticks: f64,
    /// Number of recovery observations. EMA not trusted until >= RECOVERY_MIN_SAMPLES.
    pub(crate) recovery_samples: u32,
    /// Fallback recovery estimate used until EMA earns trust.
    pub(crate) initial_seed: f64,
    /// Ticks since this constraint last triggered backoff (for measuring recovery).
    /// `None` when not tracking a recovery.
    pub(crate) ticks_since_backoff: Option<u32>,
}

impl ConstraintState {
    pub(crate) fn new(persistence: u32, initial_seed: f64) -> Self {
        Self {
            violation_streak: 0,
            persistence,
            ticks_since_seen: 0,
            missing_streak: 0,
            recovery_ema_ticks: initial_seed,
            recovery_samples: 0,
            initial_seed,
            ticks_since_backoff: None,
        }
    }

    /// Cooldown estimate: trust EMA only with enough samples, otherwise
    /// use the larger of EMA and initial seed.
    pub(crate) fn cooldown_estimate(&self) -> f64 {
        if self.recovery_samples >= RECOVERY_MIN_SAMPLES {
            self.recovery_ema_ticks
        } else {
            self.recovery_ema_ticks.max(self.initial_seed)
        }
    }

    /// Advance recovery tracking. Call each tick with the constraint's
    /// current severity. Recovery is declared when enough ticks have passed
    /// AND severity drops below the threshold.
    pub(crate) fn advance_recovery(&mut self, severity: f64) {
        if let Some(ref mut ticks) = self.ticks_since_backoff {
            *ticks += 1;
            if *ticks >= RECOVERY_MIN_TICKS && severity < RECOVERY_SEVERITY_THRESHOLD {
                let recovery_ticks = *ticks as f64;
                self.recovery_samples += 1;
                self.recovery_ema_ticks = RECOVERY_EMA_ALPHA * recovery_ticks
                    + (1.0 - RECOVERY_EMA_ALPHA) * self.recovery_ema_ticks;
                self.ticks_since_backoff = None;
            }
        }
    }

    /// Start or restart recovery tracking. If already tracking (re-backoff
    /// before recovery completed), discard the in-progress measurement.
    pub(crate) fn start_recovery_tracking(&mut self) {
        self.ticks_since_backoff = Some(0);
    }
}

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

/// Maps continuous severity to a graduated action.
///
/// Thresholds: `<0.7` AllowIncrease, `[0.7,1.0)` Hold, `[1.0,1.5)` Gentle,
/// `[1.5,2.0)` Moderate, `≥2.0` Hard.
pub(crate) fn graduated_action(severity: f64) -> ConstraintAction {
    if severity < 0.7 {
        ConstraintAction::AllowIncrease
    } else if severity < 1.0 {
        ConstraintAction::Hold
    } else if severity < 1.5 {
        ConstraintAction::GentleBackoff
    } else if severity < 2.0 {
        ConstraintAction::ModerateBackoff
    } else {
        ConstraintAction::HardBackoff
    }
}

/// Updates violation streak with hysteresis.
///
/// Streak increments on violation (severity >= 1.0), resets only in the
/// AllowIncrease zone (severity < 0.7), and is maintained in the Hold
/// band `[0.7, 1.0)`. This prevents rapid flapping at the boundary.
pub(crate) fn update_streak(streak: &mut u32, severity: f64) {
    if severity >= 1.0 {
        *streak += 1;
    } else if severity < 0.7 {
        *streak = 0;
    }
    // [0.7, 1.0): maintained (no increment, no reset)
}

/// Persistence gate: backoff actions are downgraded to Hold when the
/// streak hasn't met the persistence requirement and severity doesn't
/// bypass it. AllowIncrease and Hold pass through ungated.
pub(crate) fn apply_persistence(
    action: ConstraintAction,
    streak: u32,
    persistence: u32,
    severity: f64,
) -> ConstraintAction {
    if !action.is_backoff() {
        return action;
    }
    if streak >= persistence || severity > SEVERITY_BYPASS_PERSISTENCE {
        action
    } else {
        ConstraintAction::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graduated_action_boundaries() {
        assert_eq!(graduated_action(0.0), ConstraintAction::AllowIncrease);
        assert_eq!(graduated_action(0.69), ConstraintAction::AllowIncrease);
        assert_eq!(graduated_action(0.7), ConstraintAction::Hold);
        assert_eq!(graduated_action(0.99), ConstraintAction::Hold);
        assert_eq!(graduated_action(1.0), ConstraintAction::GentleBackoff);
        assert_eq!(graduated_action(1.49), ConstraintAction::GentleBackoff);
        assert_eq!(graduated_action(1.5), ConstraintAction::ModerateBackoff);
        assert_eq!(graduated_action(1.99), ConstraintAction::ModerateBackoff);
        assert_eq!(graduated_action(2.0), ConstraintAction::HardBackoff);
        assert_eq!(graduated_action(10.0), ConstraintAction::HardBackoff);
    }

    #[test]
    fn streak_hysteresis_rules() {
        let mut streak = 0u32;

        // Violation zone: increments
        update_streak(&mut streak, 1.5);
        assert_eq!(streak, 1);
        update_streak(&mut streak, 1.0);
        assert_eq!(streak, 2);

        // Hold band: maintained
        update_streak(&mut streak, 0.8);
        assert_eq!(streak, 2);
        update_streak(&mut streak, 0.7);
        assert_eq!(streak, 2);

        // Clean zone: reset
        update_streak(&mut streak, 0.69);
        assert_eq!(streak, 0);
    }

    #[test]
    fn persistence_gating() {
        // AllowIncrease and Hold pass through
        assert_eq!(
            apply_persistence(ConstraintAction::AllowIncrease, 0, 2, 0.5),
            ConstraintAction::AllowIncrease
        );
        assert_eq!(
            apply_persistence(ConstraintAction::Hold, 0, 2, 0.8),
            ConstraintAction::Hold
        );

        // Backoff downgraded when streak < persistence
        assert_eq!(
            apply_persistence(ConstraintAction::GentleBackoff, 1, 2, 1.2),
            ConstraintAction::Hold
        );

        // Backoff passes when streak >= persistence
        assert_eq!(
            apply_persistence(ConstraintAction::GentleBackoff, 2, 2, 1.2),
            ConstraintAction::GentleBackoff
        );

        // Bypass at high severity
        assert_eq!(
            apply_persistence(ConstraintAction::HardBackoff, 0, 5, 3.1),
            ConstraintAction::HardBackoff
        );
    }

    #[test]
    fn constraint_action_ordering() {
        assert!(ConstraintAction::AllowIncrease < ConstraintAction::Hold);
        assert!(ConstraintAction::Hold < ConstraintAction::GentleBackoff);
        assert!(ConstraintAction::GentleBackoff < ConstraintAction::ModerateBackoff);
        assert!(ConstraintAction::ModerateBackoff < ConstraintAction::HardBackoff);

        // min() selects less severe
        assert_eq!(
            ConstraintAction::GentleBackoff.min(ConstraintAction::Hold),
            ConstraintAction::Hold
        );
    }

    #[test]
    fn constraint_state_cooldown_estimate() {
        // Fresh state: uses max(ema=seed, seed) = seed
        let state = ConstraintState::new(1, 10.0);
        assert_eq!(state.cooldown_estimate(), 10.0);

        // Partial samples: uses max(ema, seed)
        let mut state = ConstraintState::new(1, 10.0);
        state.recovery_ema_ticks = 15.0;
        state.recovery_samples = 2; // < RECOVERY_MIN_SAMPLES (3)
        assert_eq!(state.cooldown_estimate(), 15.0);

        // Enough samples: trusts EMA directly
        state.recovery_samples = 3;
        state.recovery_ema_ticks = 5.0;
        assert_eq!(state.cooldown_estimate(), 5.0);
    }

    #[test]
    fn advance_recovery_updates_ema() {
        let mut state = ConstraintState::new(1, 10.0);
        state.start_recovery_tracking();

        // Advance 4 ticks (>= RECOVERY_MIN_TICKS=3) with low severity
        state.advance_recovery(0.5);
        state.advance_recovery(0.5);
        state.advance_recovery(0.5);
        // Tick 3: ticks=3, not yet >= MIN_TICKS because ticks starts at 0
        // and gets +1 before check, so tick 1→1, tick 2→2, tick 3→3 >=3 ✓
        // severity 0.5 < 0.9 threshold ✓ → recovery declared
        assert_eq!(state.recovery_samples, 1);
        assert!(state.ticks_since_backoff.is_none());

        // EMA: 0.3 * 3.0 + 0.7 * 10.0 = 0.9 + 7.0 = 7.9
        assert!((state.recovery_ema_ticks - 7.9).abs() < 0.01);
    }
}
