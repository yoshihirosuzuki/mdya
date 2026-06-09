//! Process-level memory guard.
//!
//! Spawns a std::thread watchdog that wakes every [`POLL_INTERVAL`] and reads
//! its own RSS via `sysinfo::Process::memory()`. If RSS exceeds the
//! configured cap, the watchdog writes one `tracing::error!` line to stderr
//! and calls [`std::process::exit`] with code [`EXIT_CODE_MEMORY_GUARD`].
//!
//! `install(0)` is a no-op: the YAML sentinel for disabling the guard is
//! `runtime.memory_limit_mb: 0`.

use std::thread;
use std::time::Duration;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, get_current_pid};
use tracing::error;

/// Polling cadence for the RSS watchdog. 250 ms gives roughly 4 checks per
/// second; on a 1 GB/s allocation burst the over-allocation window stays
/// under ~250 MB, which is small relative to the 8192 MB default cap.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Exit code used when the guard fires (POSIX `128 + SIGKILL`).
pub const EXIT_CODE_MEMORY_GUARD: i32 = 137;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Install the RSS watchdog. Returns immediately. Passing `limit_mb == 0`
/// is the disable path: nothing is spawned, mdya runs without a cap.
pub fn install(limit_mb: u64) {
    if limit_mb == 0 {
        return;
    }
    spawn_watchdog(limit_mb, exit_on_breach);
}

fn spawn_watchdog(limit_mb: u64, on_breach: fn(used_mb: u64, limit_mb: u64) -> !) {
    thread::Builder::new()
        .name("mdya-memory-guard".to_string())
        .spawn(move || run_watchdog(limit_mb, on_breach))
        .expect("spawn memory-guard thread");
}

fn run_watchdog(limit_mb: u64, on_breach: fn(used_mb: u64, limit_mb: u64) -> !) -> ! {
    let pid = get_current_pid().expect("sysinfo: current pid resolvable on supported platforms");
    let mut sys = System::new();
    loop {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            // remove entry if self-process is gone — unreachable in practice
            // (the guard calls `exit(137)` from inside this thread before the
            // process could die for other reasons), but kept consistent with
            // sysinfo's recommended call shape.
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        if let Some(used_mb) = sys.process(pid).map(|p| p.memory() / BYTES_PER_MB)
            && used_mb > limit_mb
        {
            on_breach(used_mb, limit_mb);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn exit_on_breach(used_mb: u64, limit_mb: u64) -> ! {
    error!("memory limit exceeded ({used_mb} MB > {limit_mb} MB), self-terminating");
    std::process::exit(EXIT_CODE_MEMORY_GUARD);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_with_zero_limit_is_a_noop_and_spawns_no_thread() {
        // The disable sentinel must not start a watchdog. We can't observe the
        // absence of a thread directly, but `install(0)` must return without
        // panicking or otherwise side-effecting the calling thread.
        install(0);
    }

    #[test]
    fn install_with_huge_limit_spawns_a_thread_that_never_fires() {
        // A cap orders of magnitude above what the test harness uses must let
        // the watchdog run quietly. We don't assert on it firing — that path
        // is covered by the integration test in `tests/runtime_memory_guard.rs`.
        install(u64::MAX / BYTES_PER_MB);
        thread::sleep(Duration::from_millis(50));
    }
}
