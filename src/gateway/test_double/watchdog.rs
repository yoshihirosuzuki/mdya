//! A test double for the function that starts the memory watchdog.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use crate::watchdog::WatchdogSettings;

/// Hands out a function that records the settings each watchdog is started
/// with, and whether the value that keeps it running is still kept.
#[derive(Clone, Default)]
pub(crate) struct TestWatchdog {
    state: Arc<Mutex<WatchdogState>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchdogState {
    /// The settings of every start, in order.
    pub(crate) started: Vec<WatchdogSettings>,
    /// How many returned values are still kept.
    pub(crate) running: u32,
}

impl TestWatchdog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The function to put in `Factories::start_watchdog`.
    pub(crate) fn start_fn(&self) -> Box<dyn Fn(WatchdogSettings) -> Box<dyn Send> + Send + Sync> {
        let state = Arc::clone(&self.state);
        Box::new(move |settings| {
            let mut locked = state.lock().unwrap();
            locked.started.push(settings);
            locked.running += 1;
            Box::new(Running(Arc::clone(&state)))
        })
    }

    pub(crate) fn state(&self) -> WatchdogState {
        self.state.lock().unwrap().clone()
    }
}

/// Counts the watchdog as stopped when dropped.
struct Running(Arc<Mutex<WatchdogState>>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.lock().unwrap().running -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> WatchdogSettings {
        WatchdogSettings {
            memory_limit_mb: 8192,
            message: "over the limit".to_owned(),
            exit_code: 137,
        }
    }

    #[test]
    fn starting_records_the_settings_and_dropping_stops() {
        let watchdog = TestWatchdog::new();
        let start = watchdog.start_fn();

        let running = start(settings());
        let while_running = watchdog.state();
        drop(running);

        assert_eq!(while_running.started, vec![settings()]);
        assert_eq!(while_running.running, 1);
        assert_eq!(watchdog.state().running, 0);
    }
}
