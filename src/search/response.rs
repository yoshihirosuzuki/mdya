//! Shared response shape for `mdya search` (CLI `--format json`) and
//! the MCP `search_*` tools. Both sides MUST serialise identically so
//! a client cannot tell which transport produced the payload.
//!
//! `SearchHit` is a `#[serde(untagged)]` enum splitting the `Doc`
//! (default) and `Chunk` shapes so the wire payload stays identical
//! across CLI and MCP. Schema-metadata mismatch warnings reach
//! `stderr` via `tracing::warn!` at `SearchEngine::open` time (see
//! `engine.rs`), keeping the wire response wrapper-free.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which search backend produced the response. `lowercase` rename keeps
/// the wire format (`"fts"` / `"vector"` / `"hybrid"`) aligned with the
/// MCP spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Fts,
    Vector,
    Hybrid,
}

impl SearchMode {
    /// The lowercase wire token (`"fts"` / `"vector"` / `"hybrid"`). The
    /// `md` / `xml` renderers need a borrowed string and cannot pull it from
    /// serde without allocating; `mode_token_matches_serde_rename` pins this
    /// against the `#[serde(rename_all = "lowercase")]` output so the two
    /// sources never drift.
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchMode::Fts => "fts",
            SearchMode::Vector => "vector",
            SearchMode::Hybrid => "hybrid",
        }
    }
}

/// Granularity of the rows in `SearchResponse.hits`. `Doc` (default)
/// collapses each `(collection, path)` to one hit; `Chunk` returns
/// the raw chunk-level rows for `--chunks` / `level: "chunk"` callers
/// that want to locate a specific passage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchLevel {
    #[default]
    Doc,
    Chunk,
}

impl SearchLevel {
    /// The lowercase wire token (`"doc"` / `"chunk"`). Pinned against
    /// the `#[serde(rename_all = "lowercase")]` output by
    /// `level_token_matches_serde_rename` so the borrowed-string path
    /// used by the `md`/`xml` renderers can never drift from serde.
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchLevel::Doc => "doc",
            SearchLevel::Chunk => "chunk",
        }
    }
}

/// One result row. Doc-level (`Doc`) is the default; chunk-level
/// (`Chunk`) is returned only when the caller opts in via the CLI
/// `--chunks` flag or the MCP `level: "chunk"` parameter.
///
/// The two variants share `collection` / `path` / `score` / `snippet`
/// and differ only in the third "granularity-specific" field:
/// `matched_chunks` (Doc) carries the breadth signal — how many of the
/// document's chunks contributed to the hit — while `chunk_sequence`
/// (Chunk) carries the 0-indexed chunk number so callers can locate the
/// passage.
///
/// `#[serde(untagged)]` means the wire JSON has no discriminator field:
/// consumers tell variants apart by the presence of `matched_chunks` vs
/// `chunk_sequence`. The `SearchResponse.level` envelope field echoes
/// the request granularity so a consumer can stay on one variant per
/// response without inspecting hit shapes (schemars 1.2 emits this as
/// `anyOf` of two object schemas, see
/// `schemars_derive-1.2.1/src/schema_exprs.rs:389-391`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SearchHit {
    Doc {
        collection: String,
        path: String,
        score: f32,
        snippet: String,
        matched_chunks: u32,
    },
    Chunk {
        collection: String,
        path: String,
        chunk_sequence: u32,
        score: f32,
        snippet: String,
    },
}

impl SearchHit {
    /// Collection name shared by both variants. Renderers use this to
    /// build the `collection/path` header without matching on the
    /// variant first.
    pub fn collection(&self) -> &str {
        match self {
            SearchHit::Doc { collection, .. } | SearchHit::Chunk { collection, .. } => collection,
        }
    }

    /// Collection-relative path shared by both variants.
    pub fn path(&self) -> &str {
        match self {
            SearchHit::Doc { path, .. } | SearchHit::Chunk { path, .. } => path,
        }
    }

    /// Score shared by both variants. The unit depends on the
    /// `SearchMode` (BM25 raw / cosine-similarity / RRF reciprocal-rank
    /// sum) — see `engine.rs` `ScoreSource` for the per-mode contract.
    pub fn score(&self) -> f32 {
        match self {
            SearchHit::Doc { score, .. } | SearchHit::Chunk { score, .. } => *score,
        }
    }

    /// Snippet shared by both variants. For `Doc`, this is the snippet
    /// from the highest-scoring chunk in the document.
    pub fn snippet(&self) -> &str {
        match self {
            SearchHit::Doc { snippet, .. } | SearchHit::Chunk { snippet, .. } => snippet,
        }
    }
}

/// Envelope returned by every search mode. `query` / `mode` / `level` /
/// `collections` / `limit` echo the request so MCP clients can route
/// responses without tracking state, and `total` is the pre-truncate
/// hit count for the `"N {unit} hits (showing M max)"` footer. The
/// `unit` (`"doc"` / `"chunk"`) is derived from `level`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub query: String,
    pub mode: SearchMode,
    pub level: SearchLevel,
    pub collections: Vec<String>,
    pub limit: u32,
    pub total: u32,
    pub hits: Vec<SearchHit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_mode_serialises_as_lowercase_string() {
        assert_eq!(serde_json::to_string(&SearchMode::Fts).unwrap(), "\"fts\"");
        assert_eq!(
            serde_json::to_string(&SearchMode::Vector).unwrap(),
            "\"vector\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMode::Hybrid).unwrap(),
            "\"hybrid\""
        );
    }

    #[test]
    fn mode_token_matches_serde_rename() {
        for mode in [SearchMode::Fts, SearchMode::Vector, SearchMode::Hybrid] {
            let serde_token = serde_json::to_string(&mode).unwrap();
            assert_eq!(format!("\"{}\"", mode.as_str()), serde_token);
        }
    }

    #[test]
    fn search_level_serialises_as_lowercase_string() {
        assert_eq!(serde_json::to_string(&SearchLevel::Doc).unwrap(), "\"doc\"");
        assert_eq!(
            serde_json::to_string(&SearchLevel::Chunk).unwrap(),
            "\"chunk\""
        );
    }

    #[test]
    fn level_token_matches_serde_rename() {
        for level in [SearchLevel::Doc, SearchLevel::Chunk] {
            let serde_token = serde_json::to_string(&level).unwrap();
            assert_eq!(format!("\"{}\"", level.as_str()), serde_token);
        }
    }

    #[test]
    fn search_level_default_is_doc() {
        assert_eq!(SearchLevel::default(), SearchLevel::Doc);
    }

    #[test]
    fn empty_response_round_trips_through_json() {
        let resp = SearchResponse {
            query: "release".to_string(),
            mode: SearchMode::Fts,
            level: SearchLevel::Doc,
            collections: vec![],
            limit: 20,
            total: 0,
            hits: vec![],
        };
        let s = serde_json::to_string(&resp).unwrap();
        let back: SearchResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn doc_hit_round_trips_with_matched_chunks_and_no_chunk_sequence() {
        let hit = SearchHit::Doc {
            collection: "notes".to_string(),
            path: "release.md".to_string(),
            score: 0.812,
            snippet: "release checklist...".to_string(),
            matched_chunks: 3,
        };
        let s = serde_json::to_string(&hit).unwrap();
        // The wire form must NOT contain `chunk_sequence` for Doc hits;
        // a consumer that infers the variant by field presence relies on
        // this absence.
        assert!(!s.contains("chunk_sequence"));
        assert!(s.contains("\"matched_chunks\":3"));
        let back: SearchHit = serde_json::from_str(&s).unwrap();
        assert_eq!(back, hit);
    }

    #[test]
    fn chunk_hit_round_trips_with_chunk_sequence_and_no_matched_chunks() {
        let hit = SearchHit::Chunk {
            collection: "notes".to_string(),
            path: "release.md".to_string(),
            chunk_sequence: 4,
            score: 0.65,
            snippet: "...".to_string(),
        };
        let s = serde_json::to_string(&hit).unwrap();
        // Mirror of the Doc test: `matched_chunks` must be absent for
        // Chunk hits so the field-presence discrimination stays clean.
        assert!(!s.contains("matched_chunks"));
        assert!(s.contains("\"chunk_sequence\":4"));
        let back: SearchHit = serde_json::from_str(&s).unwrap();
        assert_eq!(back, hit);
    }

    #[test]
    fn search_hit_accessors_return_shared_fields_for_both_variants() {
        let doc = SearchHit::Doc {
            collection: "notes".to_string(),
            path: "a.md".to_string(),
            score: 0.5,
            snippet: "doc snippet".to_string(),
            matched_chunks: 2,
        };
        assert_eq!(doc.collection(), "notes");
        assert_eq!(doc.path(), "a.md");
        assert_eq!(doc.score(), 0.5);
        assert_eq!(doc.snippet(), "doc snippet");

        let chunk = SearchHit::Chunk {
            collection: "work".to_string(),
            path: "b.md".to_string(),
            chunk_sequence: 7,
            score: 0.9,
            snippet: "chunk snippet".to_string(),
        };
        assert_eq!(chunk.collection(), "work");
        assert_eq!(chunk.path(), "b.md");
        assert_eq!(chunk.score(), 0.9);
        assert_eq!(chunk.snippet(), "chunk snippet");
    }
}
