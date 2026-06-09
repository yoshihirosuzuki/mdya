//! Hidden `mdya stress allocate <mb>` subcommand. Test-only entry point that
//! lets the integration suite trigger the runtime memory guard against
//! the real watchdog install path. Marked `hide = true` in clap so
//! it does not appear in `mdya --help`.
//!
//! The allocation is held in a `Vec<u8>` and the function then parks the
//! thread forever. Production code never reaches this — the watchdog kills
//! the process from a different thread once RSS crosses the cap.

use std::thread;
use std::time::Duration;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum StressCommand {
    /// Allocate `mb` MB of RSS and park the main thread. Test-only helper.
    Allocate {
        /// Megabytes to allocate (filled with non-zero bytes so the pages are
        /// actually touched and resident, not just reserved).
        mb: u64,
    },
}

pub fn run(cmd: &StressCommand) {
    match cmd {
        StressCommand::Allocate { mb } => allocate_and_park(*mb),
    }
}

fn allocate_and_park(mb: u64) {
    // `usize::try_from` handles a hypothetical future 32-bit port (current
    // builds are 64-bit only) by saturating instead of silently truncating;
    // `saturating_mul` then absorbs unreasonably large `mb`.
    let bytes = usize::try_from(mb)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024);
    // `vec![1u8; n]` writes every byte so the kernel actually maps the pages.
    // A `Vec::with_capacity` would only reserve virtual address space and
    // leave RSS untouched, which is the opposite of what we want to test.
    let buf: Vec<u8> = vec![1; bytes];
    // Keep `buf` alive: the guard runs on a separate thread and only needs
    // 250 ms (POLL_INTERVAL) to see the spike. Sleeping for minutes is safe
    // because the watchdog will exit(137) long before this returns.
    loop {
        thread::sleep(Duration::from_secs(60));
        std::hint::black_box(&buf);
    }
}
