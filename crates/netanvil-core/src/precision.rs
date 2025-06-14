use std::time::{Duration, Instant};

/// Minimum sleep duration — below this, we skip the sleep and return
/// immediately rather than busy-spinning (which would starve the runtime).
const MIN_SLEEP: Duration = Duration::from_micros(500);

/// Sleep until `target` using the compio runtime timer.
///
/// Never busy-spins — the compio runtime uses cooperative scheduling,
/// so any synchronous spin would starve spawned I/O tasks on the same
/// thread. Instead, we use the runtime timer for all delays.
///
/// For intervals shorter than `MIN_SLEEP`, we return immediately.
/// The scheduling loop handles the catch-up naturally: when the next
/// intended time is already in the past, it fires the request
/// immediately without sleeping.
pub async fn precision_sleep_until(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }

    let remaining = target - now;
    if remaining >= MIN_SLEEP {
        compio::time::sleep(remaining).await;
    }
    // For sub-500μs sleeps: return immediately.
    // The request fires slightly early, but we don't starve the runtime.
}
