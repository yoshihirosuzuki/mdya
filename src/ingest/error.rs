//! Error type for the ingest module.
//!
//! Every lib module owns a `thiserror::Error` enum; the binary lifts
//! it through `anyhow::Error`. Most file-level errors (read / chunk /
//! DB write of a single file) are intentionally NOT variants here —
//! they are reported via `IngestProgress` and counted into
//! `UpdateSummary.failed`, so the surrounding `update_all_collections`
//! call returns `Ok(summary)` even when some files fail. The exceptions are
//! `Embedding` (`EmbedError`) and `EmbedJoin` (spawn_blocking panic):
//! both surface as variants so log output distinguishes embed
//! errors from generic file-level I/O, but they are still caught by
//! the per-file `unwrap_or_else` in the writer loop and counted as
//! `Failed` rather than aborting the whole run.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    #[error("connect LanceDB at {path}: {source}")]
    LancedbConnect {
        path: PathBuf,
        #[source]
        source: lancedb::Error,
    },

    #[error("LanceDB path {path} is not valid UTF-8")]
    LancedbPathNotUtf8 { path: PathBuf },

    #[error("open chunks table: {0}")]
    OpenChunksTable(#[source] lancedb::Error),

    #[error("open sources table: {0}")]
    OpenSourcesTable(#[source] lancedb::Error),

    #[error("query chunks: {0}")]
    QueryChunks(#[source] lancedb::Error),

    #[error("query sources: {0}")]
    QuerySources(#[source] lancedb::Error),

    #[error("delete chunks: {0}")]
    DeleteChunks(#[source] lancedb::Error),

    #[error("delete sources: {0}")]
    DeleteSources(#[source] lancedb::Error),

    #[error("write chunks: {0}")]
    WriteChunks(#[source] lancedb::Error),

    #[error("write sources: {0}")]
    WriteSources(#[source] lancedb::Error),

    #[error("update chunks: {0}")]
    UpdateChunks(#[source] lancedb::Error),

    #[error(transparent)]
    LanceLm(#[from] crate::store::lance_lm::LanceLmError),

    #[error("count chunks rows: {0}")]
    CountRows(#[source] lancedb::Error),

    #[error("list chunks indices: {0}")]
    ListIndices(#[source] lancedb::Error),

    #[error("create FTS index on `{column}`: {source}")]
    CreateFtsIndex {
        column: &'static str,
        #[source]
        source: lancedb::Error,
    },

    #[error("create vector index on `embedding`: {0}")]
    CreateVectorIndex(#[source] lancedb::Error),

    #[error("optimize chunks indices: {0}")]
    OptimizeIndex(#[source] lancedb::Error),

    #[error("embedding: {0}")]
    Embedding(#[from] crate::embedding::EmbedError),

    /// `tokio::task::spawn_blocking` task panicked or was cancelled while
    /// running the embedder. Surfaced as a file-level error so the
    /// surrounding loop logs it and continues; only the affected file
    /// fails. Distinct from `Embedding` (`EmbedError`) so log output
    /// makes the JoinError / panic origin clear.
    #[error("embed task join failed: {0}")]
    EmbedJoin(#[source] tokio::task::JoinError),

    #[error("chunking: {0}")]
    Chunking(#[from] crate::chunking::ChunkingError),

    /// Format extractor failed — non-UTF-8 Markdown bytes or a malformed /
    /// unsupported PDF. File-level: caught by the per-file
    /// `unwrap_or_else` in the writer loop, logged, and counted as
    /// `Failed` rather than aborting the run.
    #[error("extract: {0}")]
    Extract(#[from] crate::extract::ExtractError),

    #[error("file I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("build Arrow record batch: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),

    /// Declared `embedding.model` / `vector_dim` disagree with the
    /// `chunks` table's `Schema::metadata` pins. Ingest is refused
    /// before any row is touched (loud-corruption policy). Recovery:
    /// remove `~/.mdya/index/`, `mdya init`, retry.
    #[error(
        "schema metadata pin mismatch: config.yml declares \
         embedding_model='{declared_model}' / vector_dim={declared_dim}, \
         but the chunks table reports embedding_model='{actual_embedding_model}' / \
         vector_dim={actual_vector_dim}. \
         To recover, remove `~/.mdya/index/`, run `mdya init`, and re-run \
         `mdya update-all`."
    )]
    SchemaMetadataMismatch {
        declared_model: String,
        declared_dim: i32,
        actual_embedding_model: String,
        actual_vector_dim: String,
    },

    /// `chunks` table exists but its `Schema::metadata` is missing one
    /// or both pin keys. `mdya init` always writes both, so this signals
    /// an older schema (no metadata pin), direct DB tampering, or a
    /// table opened from a foreign tool. Treated as ingest-abort.
    #[error(
        "schema metadata pin missing: chunks table's Schema::metadata \
         lacks key(s) {absent_keys:?}. This usually means the database \
         was created before the metadata pin was added; remove \
         `~/.mdya/index/`, run `mdya init`, and re-run `mdya update-all`."
    )]
    SchemaMetadataMissing { absent_keys: Vec<&'static str> },
}
