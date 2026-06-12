//! MCP tool input schema for the `search` tool.
//!
//! The search mode is a `mode` field (defaulting to hybrid), not encoded
//! in the tool name, so one tool covers BM25 / vector / hybrid.
//!
//! This is intentionally distinct from [`crate::search::SearchRequest`]:
//! the wire field is `k` (MCP convention) whereas the engine field is
//! `limit` (CLI convention). [`SearchRequest::into_engine_request`] does
//! the `k` → `limit` rename once, at the tool boundary, so the engine
//! never sees the MCP-specific spelling. The `level` field defaults to
//! `Doc`, matching the CLI default, and the boundary flows it straight
//! into the engine request; `mode` is read by the tool to pick the engine
//! method and so is not part of the engine request.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::search::{SearchLevel, SearchMode};

/// Input for the `search` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchRequest {
    /// Search query string. Empty or whitespace-only is rejected.
    pub query: String,

    /// Top-N results. Defaults to 20 when the client omits it.
    #[serde(default = "default_k")]
    pub k: u32,

    /// Collection filter. Empty (or omitted) searches every collection.
    #[serde(default)]
    pub collections: Vec<String>,

    /// Hit granularity. `"doc"` (default) collapses each
    /// `(collection, path)` to one hit carrying the max chunk score
    /// and a `matched_chunks` count; `"chunk"` returns the raw
    /// chunk-level rows for callers that need to locate a specific
    /// passage. Omitting the field is equivalent to `"doc"`.
    #[serde(default)]
    pub level: SearchLevel,

    /// Search backend: `"fts"` (BM25 keyword), `"vector"` (semantic
    /// cosine), or `"hybrid"` (both, fused with RRF). Omitting the field
    /// is equivalent to `"hybrid"`, the general-purpose default.
    #[serde(default)]
    pub mode: SearchMode,
}

fn default_k() -> u32 {
    20
}

/// Input for the `get_document` tool: the composite key that
/// identifies one document, optionally narrowed to a single chunk.
/// Mirrors the `mdya get <collection> <path> [--chunk <N>]` CLI
/// surface — omitting `chunk` returns the faithful full document from
/// `sources`; supplying `chunk` returns that chunk's `body` from
/// `chunks`, the locator coming from a search hit's `chunk_sequence`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetDocumentRequest {
    /// Collection name (must be declared in config.yml).
    pub collection: String,
    /// Document path, relative to the collection root.
    pub path: String,
    /// 0-indexed `chunk_sequence` to fetch a single chunk's body
    /// instead of the full document. Omit for the faithful full
    /// document.
    #[serde(default)]
    pub chunk: Option<u32>,
}

impl SearchRequest {
    /// Rename `k` → `limit` and hand the validated input to the engine.
    /// Bounds checks (empty query, `limit == 0`, unknown collection) live
    /// in [`crate::search::SearchEngine::validate_request`], shared with
    /// the CLI, so the MCP boundary stays a pure field rename.
    pub fn into_engine_request(self) -> crate::search::SearchRequest {
        crate::search::SearchRequest {
            query: self.query,
            collections: self.collections,
            limit: self.k,
            level: self.level,
        }
    }
}
