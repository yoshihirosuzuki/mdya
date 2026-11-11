//! The `watchdog` component: stops the process when it uses more memory than
//! the limit.

mod monitor;
mod own_process;
#[cfg(test)]
mod tests;

use std::time::Duration;

use monitor::{Monitor, Process};
use own_process::OwnProcess;

/// How often the watchdog measures. A process that allocates 1 GB a second
/// gets about 250 MB past the limit before the watchdog stops it.
const MEASURE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchdogSettings {
    /// The memory limit in MB. 0 turns the watchdog off.
    pub memory_limit_mb: u64,
    /// What to write to stderr when the limit is exceeded.
    pub message: String,
    /// The exit code when the limit is exceeded.
    pub exit_code: i32,
}

/// A running watchdog. Dropping it stops the watchdog.
pub struct Watchdog {
    /// Kept for what dropping it does. `None` when the watchdog is off.
    _monitor: Option<Monitor>,
}

/// Starts the watchdog.
pub fn start(settings: WatchdogSettings) -> Watchdog {
    start_watching(settings, OwnProcess, MEASURE_INTERVAL)
}

fn start_watching(
    settings: WatchdogSettings,
    process: impl Process,
    interval: Duration,
) -> Watchdog {
    let monitor =
        (settings.memory_limit_mb != 0).then(|| Monitor::start(settings, process, interval));
    Watchdog { _monitor: monitor }
}
