//! Progress reporting trait for the ingest pipeline.
//!
//! `update_all_collections` is pure backend logic; the interactive CLI
//! plugs in an `indicatif`-backed implementation. Keeping the
//! ingest module free of any TUI dependency lets unit and integration
//! tests run with `NullProgress` (the default no-op), and lets future
//! callers (MCP server, scripted runs) reuse the same backend without
//! pulling a progress bar.

use std::path::Path;

/// Outcome reported back for each file the ingest writer touches.
/// Bumped into the matching `UpdateSummary` counter and forwarded to the
/// progress sink for live UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// New file: not present in DB before this run.
    New,
    /// File content changed since last ingest: chunks were replaced.
    Updated,
    /// File unchanged (mtime + hash match): nothing to do.
    Skipped,
    /// File-level error during processing (read / chunk / embed / write).
    /// The surrounding loop logs and continues per grill Q5.
    Failed,
}

/// Sink for live progress updates. Implementations are `Send + Sync` so
/// callers can store one inside an `Arc` for parallel ingest.
///
/// `finish_file` carries the matching `path` so implementations can
/// pair it with the earlier `start_file(path)` even when several files
/// are in flight simultaneously. A pool-backed UI needs the path to
/// release the right slot; the no-op `NullProgress` just ignores it.
pub trait IngestProgress: Send + Sync {
    /// Called once per collection before any file work begins.
    fn set_total_files(&self, total: usize);

    /// Called when the writer starts processing one file.
    fn start_file(&self, path: &Path);

    /// Called when the writer finishes one file (success or failure).
    /// `path` matches the value passed to the paired `start_file` call.
    fn finish_file(&self, path: &Path, outcome: FileOutcome);

    /// Called once per `update_all_collections` call at the very end.
    fn finish(&self);
}

/// No-op default used by tests and by callers that do not want UI output.
pub struct NullProgress;

impl IngestProgress for NullProgress {
    fn set_total_files(&self, _total: usize) {}
    fn start_file(&self, _path: &Path) {}
    fn finish_file(&self, _path: &Path, _outcome: FileOutcome) {}
    fn finish(&self) {}
}
