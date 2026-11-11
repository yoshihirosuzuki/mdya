//! Measuring the memory of the process again and again, and stopping the
//! process when it is above the limit.

#[cfg(test)]
mod tests;

use std::convert::Infallible;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::WatchdogSettings;

/// 1 MB of the memory limit, in bytes.
const BYTES_PER_MB: u64 = 1_048_576;

/// The process that the monitor watches. The tests replace the real one.
pub(super) trait Process: Send + 'static {
    /// How much memory the process has resident, in bytes, or `None` when it
    /// cannot be measured.
    fn resident_bytes(&mut self) -> Option<u64>;

    /// Writes `message` to stderr and stops the process with `exit_code`,
    /// without waiting for anything else in the process.
    fn stop(&mut self, message: &str, exit_code: i32);
}

/// A running monitor. Dropping it ends the monitor.
pub(super) struct Monitor {
    /// The sender never sends anything: dropping it ends the wait on the
    /// thread. `None` only once the monitor is being dropped.
    running: Option<(Sender<Infallible>, JoinHandle<()>)>,
}

impl Drop for Monitor {
    /// Returns once the thread has ended, so that the monitor cannot stop the
    /// process after it has been dropped.
    fn drop(&mut self) {
        let Some((end, thread)) = self.running.take() else {
            return;
        };
        drop(end);
        // A panic on the thread has already gone to the panic hook.
        let _ = thread.join();
    }
}

impl Monitor {
    /// Starts measuring on a thread of its own, at once and then every
    /// `interval`. Panics when the operating system cannot make the thread.
    pub(super) fn start(
        settings: WatchdogSettings,
        process: impl Process,
        interval: Duration,
    ) -> Self {
        let (end, ended) = channel();
        let check = Check::new(settings, process);
        let thread = thread::spawn(move || watch(check, interval, &ended));
        Self {
            running: Some((end, thread)),
        }
    }
}

/// Checks the process until it has stopped it or the monitor is dropped.
fn watch(mut check: Check<impl Process>, interval: Duration, ended: &Receiver<Infallible>) {
    while !check.run() {
        if let Err(RecvTimeoutError::Disconnected) = ended.recv_timeout(interval) {
            return;
        }
    }
}

/// Measures the process and stops it when it is above the limit.
struct Check<P> {
    settings: WatchdogSettings,
    process: P,
    warned_of_a_failed_measurement: bool,
}

impl<P: Process> Check<P> {
    fn new(settings: WatchdogSettings, process: P) -> Self {
        Self {
            settings,
            process,
            warned_of_a_failed_measurement: false,
        }
    }

    /// Measures once. Returns whether it stopped the process.
    fn run(&mut self) -> bool {
        let Some(resident_bytes) = self.process.resident_bytes() else {
            self.warn_of_a_failed_measurement();
            return false;
        };
        if resident_bytes <= limit_in_bytes(self.settings.memory_limit_mb) {
            return false;
        }
        tracing::error!(
            resident_bytes,
            memory_limit_mb = self.settings.memory_limit_mb,
            "the memory limit was exceeded"
        );
        self.process
            .stop(&self.settings.message, self.settings.exit_code);
        true
    }

    /// Warns once: the measurement is tried again every interval, and the
    /// same warning each time would fill the log.
    fn warn_of_a_failed_measurement(&mut self) {
        if !self.warned_of_a_failed_measurement {
            tracing::warn!(
                "cannot measure the memory of this process, so it may go over the memory limit"
            );
            self.warned_of_a_failed_measurement = true;
        }
    }
}

/// The limit in bytes. A limit too large for a `u64` becomes the largest
/// `u64`, which no amount of memory is above.
fn limit_in_bytes(memory_limit_mb: u64) -> u64 {
    memory_limit_mb.saturating_mul(BYTES_PER_MB)
}
