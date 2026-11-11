//! The process that the watchdog watches when it is not under test: this one.

use std::io::{Write, stderr};
use std::process::exit;

use super::monitor::Process;

/// This process, measured through the operating system: the resident set size
/// on Linux and macOS, and the working set on Windows.
pub(super) struct OwnProcess;

impl Process for OwnProcess {
    fn resident_bytes(&mut self) -> Option<u64> {
        memory_stats::memory_stats().map(|stats| stats.physical_mem as u64)
    }

    fn stop(&mut self, message: &str, exit_code: i32) {
        // The process stops whether or not stderr takes the message. `eprint!`
        // would panic on a closed stderr, and leave the process running with
        // nothing watching it.
        let _ = stderr().write_all(message.as_bytes());
        exit(exit_code);
    }
}
