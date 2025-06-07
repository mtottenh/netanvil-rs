use std::time::{Duration, Instant};

/// Threshold below which we switch from runtime timer to busy-spin.
/// 1ms is a good default — the runtime timer is imprecise below this.
const SPIN_THRESHOLD: Duration = Duration::from_millis(1);

/// Sleep until `target` with high precision.
///
/// Uses the compio runtime timer for the coarse portion (saving CPU),
/// then busy-spins for the final microseconds (maximum precision).
pub async fn precision_sleep_until(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }

    let remaining = target - now;
    if remaining > SPIN_THRESHOLD {
        compio::time::sleep(remaining - SPIN_THRESHOLD).await;
    }

    // Busy-spin for the final stretch
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}
