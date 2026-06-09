//! Internal request type carrying the validated input every search
//! mode shares. CLI `SearchArgs` and the MCP request schema both
//! normalise into this struct so `SearchEngine` does not see
//! surface-level differences (`-c`/`--collections`, `k` vs `limit`,
//! etc.).

use super::response::SearchLevel;

/// Validated search request. `limit` is named to match the CLI
/// `--limit` flag rather than the MCP `k` field; the MCP layer
/// renames `k` → `limit` once at its boundary so the high-frequency
/// CLI path is rename-free. `level` selects doc-level (default) vs
/// chunk-level granularity for the returned hits — CLI derives it
/// from `--chunks`, MCP from the `level` request field.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub collections: Vec<String>,
    pub limit: u32,
    pub level: SearchLevel,
}
