//! Progress reporting trait for the ingest pipeline.
//!
//! `update_all_collections` is pure backend logic; the CLI plugs in
//! progress bars where they can be drawn on stderr and periodic status
//! lines otherwise. Keeping the ingest module free of any TUI dependency lets
//! unit and integration tests run with `NullProgress` (the default
//! no-op), and lets future callers (MCP server, scripted runs) reuse the
//! same backend without pulling a progress bar.

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
    /// The surrounding loop logs it and continues with the next file.
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
    /// Called once per collection before any of its files are processed.
    /// The files finished so far count from zero again for each
    /// collection.
    fn start_collection(&self, name: &str, total_files: usize);

    /// Called when the writer starts processing one file.
    fn start_file(&self, path: &Path);

    /// Called while one file's chunks are embedded: once with `done == 0`
    /// after the file is chunked, then after each embedding batch. `done`
    /// counts every chunk of the batches finished so far, including the
    /// empty placeholder chunk that is stored without an embedding. Only
    /// files whose chunks are written report this; a skipped file does
    /// not.
    fn chunks_embedded(&self, path: &Path, done: usize, total: usize);

    /// Called when the writer finishes one file (success or failure).
    /// `path` matches the value passed to the paired `start_file` call.
    fn finish_file(&self, path: &Path, outcome: FileOutcome);

    /// Called once per `update_all_collections` call after every
    /// collection's files are processed, before the search indexes are
    /// built or refreshed.
    fn start_index_maintenance(&self);

    /// Called once per `update_all_collections` call at the very end.
    fn finish(&self);
}

/// No-op default used by tests and by callers that do not want UI output.
pub struct NullProgress;

impl IngestProgress for NullProgress {
    fn start_collection(&self, _name: &str, _total_files: usize) {}
    fn start_file(&self, _path: &Path) {}
    fn chunks_embedded(&self, _path: &Path, _done: usize, _total: usize) {}
    fn finish_file(&self, _path: &Path, _outcome: FileOutcome) {}
    fn start_index_maintenance(&self) {}
    fn finish(&self) {}
}
