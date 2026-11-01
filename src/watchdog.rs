//! The `watchdog` component: stops the process when it uses more memory than
//! the limit.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchdogSettings {
    /// The memory limit in MB. 0 turns the watchdog off.
    pub memory_limit_mb: u64,
    /// What to write to stderr when the limit is exceeded.
    pub message: String,
    /// The exit code when the limit is exceeded.
    pub exit_code: i32,
}

/// A running watchdog. Dropping it stops the watchdog. What it holds is up to
/// this component.
pub struct Watchdog(());

/// Starts the watchdog.
pub fn start(settings: WatchdogSettings) -> Watchdog {
    let _ = settings;
    todo!()
}
