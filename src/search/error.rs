//! `SearchError` enum for the search module.
//!
//! Each library module owns its `thiserror::Error` enum; the binary
//! lifts it through `anyhow::Error`. The variants are operation-
//! specific (mirroring `IngestError`) so the `Caused by:` chain
//! printed by `main` tells the user *which* DB operation failed.

use std::path::PathBuf;

use thiserror::Error;

use crate::config::ConfigError;
use crate::embedding::EmbedError;

#[derive(Debug, Error)]
pub enum SearchError {
    /// Empty / whitespace-only `query`. Message is shared with the
    /// MCP surface so CLI and MCP are interchangeable.
    #[error("query must be non-empty")]
    EmptyQuery,

    /// `limit == 0`. Top-N of 0 has no meaningful result envelope, so
    /// reject up front rather than returning `{ hits: [] }`.
    #[error("limit must be >= 1")]
    InvalidLimit,

    /// `-c <name>` referenced a collection not in `config.yml`. Typo
    /// detection — better to fail loudly than silently return zero
    /// hits.
    #[error("unknown collection: '{name}'")]
    UnknownCollection { name: String },

    #[error("LanceDB path {path} is not valid UTF-8")]
    LancedbPathNotUtf8 { path: PathBuf },

    #[error("connect LanceDB at {path}: {source}")]
    LancedbConnect {
        path: PathBuf,
        #[source]
        source: lancedb::Error,
    },

    #[error("open chunks table: {0}")]
    OpenChunksTable(#[source] lancedb::Error),

    #[error("execute search query: {0}")]
    Query(#[source] lancedb::Error),

    /// Embedding failure propagated from the vector / hybrid path.
    #[error("embedding: {0}")]
    Embed(#[from] EmbedError),

    #[error(transparent)]
    Config(#[from] ConfigError),

    /// `chunks` table exists but its `Schema::metadata` lacks one or
    /// both pin keys. `mdya init` always writes both, so this signals
    /// a pre-pin DB, direct tampering, or a foreign-tool table. Search
    /// aborts with exit 1; recovery is the same as the ingest path
    /// (remove `~/.mdya/index/`, `mdya init`, retry).
    #[error(
        "schema metadata pin missing: chunks table's Schema::metadata \
         lacks key(s) {absent_keys:?}. Remove `~/.mdya/index/`, run \
         `mdya init`, and re-run `mdya update-all` to rebuild."
    )]
    SchemaMetadataMissing { absent_keys: Vec<&'static str> },
}
