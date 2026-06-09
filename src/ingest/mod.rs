//! Ingest pipeline — implementation of `mdya update-all`'s backend.
//!
//! Public entry point: [`update_all_collections`]. The CLI subcommand
//! and progress bar live in `src/cli/update_all.rs`; this module
//! deliberately depends only on `chunking` / `embedding` / `store` /
//! `config` and remains UI-free so future callers (MCP, scripted
//! runs) can reuse it.
//!
//! The flow is staged into small leaf modules:
//!
//! - [`walker`] — list `.md` / `.markdown` files under a collection root
//! - [`incremental`] — decide skip / touch-mtime / reingest per file
//! - [`orphan`] — DB-minus-FS set difference for stale chunks
//! - [`writer`] — orchestration: chunk + embed + LanceDB upsert
//! - [`progress`] — UI hook trait (interactive CLI plugs in `indicatif`)

pub(crate) mod error;
mod incremental;
mod orphan;
mod progress;
mod walker;
mod writer;

pub use error::IngestError;
pub use progress::{FileOutcome, IngestProgress, NullProgress};
pub use writer::{UpdateSummary, update_all_collections};
