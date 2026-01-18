//! CPU isolation: migrate system threads off hot-path cores.
//!
//! Walks `/proc/*/task/*/comm` and uses `sched_setaffinity` to move
//! non-essential threads to housekeeping cores, shielding the timer
//! and I/O worker cores from scheduling noise.
//!
//! Similar in spirit to `cset shield` or `tuna --isolate`, but done
//! in-process so no external tooling is required.

/// Attempt to migrate all system threads off `hot_cores` onto `housekeeping_core`.
///
/// Iterates every thread on the system (via `/proc/*/task/`), reads its
/// name from `comm`, and calls `sched_setaffinity` to confine it to the
/// housekeeping core — unless the thread is a per-CPU kernel thread that
/// must not be moved (softirq handlers, migration threads, etc.).
///
/// Best-effort: permission errors and races (threads exiting mid-scan)
/// are silently skipped. Returns `(migrated, skipped, failed)` counts.
///
/// # When to call
///
/// Call **before** spawning hot-path threads. The caller's own thread
/// should already be pinned to `housekeeping_core` so that any children
/// it spawns afterwards inherit the restricted affinity.
#[cfg(target_os = "linux")]
pub fn shield_hot_cores(hot_cores: &[usize], housekeeping_core: usize) -> ShieldResult {
    use std::collections::HashSet;
    use std::fs;

    let hot_set: HashSet<usize> = hot_cores.iter().copied().collect();

    // Build the target CPU set (just the housekeeping core).
    let mut housekeeping_cpuset: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut housekeeping_cpuset);
        libc::CPU_SET(housekeeping_core, &mut housekeeping_cpuset);
    }

    let mut result = ShieldResult::default();

    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("cannot read /proc: {e}");
            return result;
        }
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();

        // Only numeric directories (PIDs).
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let task_dir = format!("/proc/{pid}/task");
        let tasks = match fs::read_dir(&task_dir) {
            Ok(d) => d,
            Err(_) => continue, // process may have exited
        };

        for task_entry in tasks.flatten() {
            let tid: libc::pid_t = match task_entry.file_name().to_string_lossy().parse() {
                Ok(t) => t,
                Err(_) => continue,
            };

            // Read thread name.
            let comm_path = format!("/proc/{pid}/task/{tid}/comm");
            let comm = match fs::read_to_string(&comm_path) {
                Ok(c) => c.trim().to_string(),
                Err(_) => continue,
            };

            // Skip per-CPU kernel threads that must stay pinned.
            if should_skip(&comm) {
                result.skipped += 1;
                continue;
            }

            // Read current affinity — skip if it doesn't overlap hot cores.
            let mut current: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            let ret = unsafe {
                libc::sched_getaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &mut current)
            };
            if ret != 0 {
                continue; // EPERM or thread exited
            }

            let on_hot = hot_set
                .iter()
                .any(|&core| unsafe { libc::CPU_ISSET(core, &current) });
            if !on_hot {
                continue; // already off hot cores
            }

            // Migrate to housekeeping core.
            let ret = unsafe {
                libc::sched_setaffinity(
                    tid,
                    std::mem::size_of::<libc::cpu_set_t>(),
                    &housekeeping_cpuset,
                )
            };
            if ret == 0 {
                result.migrated += 1;
                tracing::trace!(pid, tid, comm, "migrated to housekeeping core");
            } else {
                result.failed += 1;
                tracing::trace!(pid, tid, comm, "failed to migrate");
            }
        }
    }

    result
}

#[cfg(not(target_os = "linux"))]
pub fn shield_hot_cores(_hot_cores: &[usize], _housekeeping_core: usize) -> ShieldResult {
    ShieldResult::default()
}

/// Reset the calling process's CPU affinity to all online CPUs.
///
/// A previous `shield_hot_cores` run may have migrated PID 1 (systemd) to a
/// single housekeeping core. Child processes inherit that restricted mask,
/// causing `available_parallelism()` to return 1 on multi-core machines.
///
/// Call this **before** `available_parallelism()` to ensure the process sees
/// all cores. Returns the number of online CPUs, or 0 on failure.
#[cfg(target_os = "linux")]
pub fn reset_affinity() -> usize {
    use std::fs;

    // Read the set of online CPUs from sysfs (e.g. "0-3" or "0-3,5,7-9").
    let online = match fs::read_to_string("/sys/devices/system/cpu/online") {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot read /sys/devices/system/cpu/online: {e}");
            return 0;
        }
    };

    let mut cpuset: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_ZERO(&mut cpuset) };
    let mut count = 0usize;

    for part in online.trim().split(',') {
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: usize = lo.parse().unwrap_or(0);
            let hi: usize = hi.parse().unwrap_or(0);
            for cpu in lo..=hi {
                unsafe { libc::CPU_SET(cpu, &mut cpuset) };
                count += 1;
            }
        } else if let Ok(cpu) = part.parse::<usize>() {
            unsafe { libc::CPU_SET(cpu, &mut cpuset) };
            count += 1;
        }
    }

    if count == 0 {
        return 0;
    }

    let ret =
        unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset) };
    if ret != 0 {
        tracing::warn!(
            "sched_setaffinity reset failed: {}",
            std::io::Error::last_os_error()
        );
        return 0;
    }

    tracing::info!(
        online_cpus = count,
        "reset process CPU affinity to all online CPUs"
    );
    count
}

#[cfg(not(target_os = "linux"))]
pub fn reset_affinity() -> usize {
    0
}

/// Counts from a [`shield_hot_cores`] sweep.
#[derive(Debug, Default)]
pub struct ShieldResult {
    /// Threads successfully migrated to the housekeeping core.
    pub migrated: usize,
    /// Threads intentionally skipped (per-CPU kernel threads).
    pub skipped: usize,
    /// Threads where `sched_setaffinity` failed (EPERM, etc.).
    pub failed: usize,
}

// ---------------------------------------------------------------------------
// Per-CPU kernel thread detection
// ---------------------------------------------------------------------------

/// Kernel thread comm prefixes that must NOT be migrated.
/// These are per-CPU bound and handle hardware/software interrupts.
const SKIP_COMM_PREFIXES: &[&str] = &[
    "ksoftirqd/",   // per-CPU software interrupt handler
    "migration/",   // per-CPU thread migration
    "watchdog/",    // per-CPU NMI watchdog
    "cpuhp/",       // CPU hotplug callback
    "idle_inject/", // per-CPU power management
    "rcuc/",        // per-CPU RCU callback
];

/// Returns `true` if the thread name indicates a per-CPU kernel thread
/// that should not be migrated off its assigned core.
fn should_skip(comm: &str) -> bool {
    // Explicit per-CPU prefixes.
    if SKIP_COMM_PREFIXES
        .iter()
        .any(|prefix| comm.starts_with(prefix))
    {
        return true;
    }

    // kworker/N:M is per-CPU bound; kworker/uN:M is unbound (can move).
    if let Some(rest) = comm.strip_prefix("kworker/") {
        return !rest.starts_with('u');
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_softirqd() {
        assert!(should_skip("ksoftirqd/0"));
        assert!(should_skip("ksoftirqd/3"));
    }

    #[test]
    fn skip_percpu_kworker() {
        assert!(should_skip("kworker/2:0"));
        assert!(should_skip("kworker/0:1H"));
    }

    #[test]
    fn allow_unbound_kworker() {
        assert!(!should_skip("kworker/u8:2"));
        assert!(!should_skip("kworker/u16:0"));
    }

    #[test]
    fn skip_migration_and_watchdog() {
        assert!(should_skip("migration/1"));
        assert!(should_skip("watchdog/0"));
    }

    #[test]
    fn allow_regular_threads() {
        assert!(!should_skip("sshd"));
        assert!(!should_skip("nginx"));
        assert!(!should_skip("netanvil-io-0"));
        assert!(!should_skip("systemd-journal"));
    }
}
