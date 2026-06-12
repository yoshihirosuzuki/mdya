//! A stderr writer for `tracing` that yields to an active progress bar.
//!
//! `mdya update-all` / `mdya vector use` render an `indicatif`
//! `MultiProgress` on stderr. `indicatif` only keeps its bars intact for
//! writes routed through the same `MultiProgress`; a raw `tracing` line
//! emitted mid-redraw corrupts them. While such a bar is live, the
//! command registers it here and the fmt layer's writer suspends it
//! around each event, so diagnostics print cleanly above the bar. With
//! no bar registered (every other command) writes go straight to stderr.

use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};

use indicatif::MultiProgress;
use tracing_subscriber::fmt::MakeWriter;

/// The progress bar currently drawing on stderr, if any. A `MultiProgress`
/// is `Arc`-backed, so the stored clone shares draw state with the bars
/// the active command renders and suspends exactly those.
fn slot() -> &'static Mutex<Option<MultiProgress>> {
    static SLOT: OnceLock<Mutex<Option<MultiProgress>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Register `bars` as the bar that stderr writes must yield to. The
/// returned guard clears the registration on drop, so the bar is honored
/// only for the lifetime of the ingest run.
///
/// A second registration overwrites the first; mdya runs at most one
/// progress-bearing command at a time, so last-writer-wins suffices.
#[must_use]
pub fn register(bars: &MultiProgress) -> ProgressGuard {
    // On a poisoned lock the registration is skipped; writes then fall
    // through to raw stderr, which only risks a redraw glitch — never lost
    // diagnostics — so the panic path is not worth escalating here.
    if let Ok(mut current) = slot().lock() {
        *current = Some(bars.clone());
    }
    ProgressGuard
}

/// Clears the active-progress registration when dropped.
pub struct ProgressGuard;

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        // Poisoned-lock fall-through, as in `register`.
        if let Ok(mut current) = slot().lock() {
            *current = None;
        }
    }
}

/// Clone the active bar out of the lock so `suspend` runs without holding
/// our mutex (it takes `indicatif`'s internal lock; the two are never
/// held together).
fn active() -> Option<MultiProgress> {
    slot().lock().ok()?.clone()
}

/// `MakeWriter` for the fmt layer: emits each event to stderr, suspending
/// the active progress bar (if any) so the redraw and the log line do not
/// interleave.
#[derive(Clone, Copy)]
pub struct ProgressAwareStderr;

impl<'a> MakeWriter<'a> for ProgressAwareStderr {
    type Writer = StderrLine;

    /// Snapshot the active bar once per event so the whole formatted line
    /// is hidden/redrawn a single time, not once per `write` call.
    fn make_writer(&'a self) -> Self::Writer {
        StderrLine {
            bars: active(),
            buffered: Vec::new(),
        }
    }
}

/// Writer for a single fmt event. The fmt layer may `write` one event in
/// several calls, so bytes are buffered and emitted once on `flush` / drop
/// under a single `suspend` — one bar hide/redraw per event.
pub struct StderrLine {
    bars: Option<MultiProgress>,
    buffered: Vec<u8>,
}

impl StderrLine {
    fn emit(&mut self) -> io::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let line = std::mem::take(&mut self.buffered);
        // `suspend` hides the bars, runs the write, then redraws them, so
        // the log line lands above an intact bar.
        match &self.bars {
            Some(bars) => bars.suspend(|| io::stderr().write_all(&line))?,
            None => io::stderr().write_all(&line)?,
        }
        io::stderr().flush()
    }
}

impl Write for StderrLine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffered.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit()
    }
}

impl Drop for StderrLine {
    fn drop(&mut self) {
        // The fmt layer drops the writer without a trailing `flush`, so the
        // event is emitted here. Errors writing diagnostics are dropped —
        // there is nowhere left to report them.
        let _ = self.emit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_active_until_the_guard_drops() {
        // This is the only unit test that touches the process-wide slot,
        // so it owns the registration it asserts on.
        let bars = MultiProgress::new();
        {
            let _guard = register(&bars);
            assert!(active().is_some(), "registered bar must be active");
        }
        assert!(active().is_none(), "guard drop must clear the active bar");
    }
}
