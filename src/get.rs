//! `mdya get` / MCP `get_document` read paths: return either the
//! faithful full-document text from `sources`, or a single chunk's
//! `body` from `chunks` when the caller supplies a `chunk_sequence`.
//!
//! Full-doc lookup is a point lookup by `(collection, path)`. `chunks.body`
//! is lossy (front matter stripped, section bodies trimmed, overflow
//! sub-chunks overlap), so the raw source cannot be reconstructed from
//! `chunks` and is stored verbatim in `sources` at ingest time. Chunk
//! lookup is the symmetric `(collection, path, chunk_sequence)` point
//! lookup against `chunks` — the missing middle ground between the
//! 60-char snippet and the full document, using the `chunk_sequence`
//! locator returned by search. Neither path touches the filesystem.

use std::path::{Path, PathBuf};

use arrow_array::cast::AsArray;
use arrow_array::{Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::Table;
use lancedb::expr::{col, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use thiserror::Error;

use crate::config;
use crate::store::{
    CHUNKS_TABLE_NAME, COL_BODY, COL_CHUNK_SEQUENCE, COL_COLLECTION, COL_CONTENT, COL_PATH,
    SOURCES_TABLE_NAME,
};

#[derive(Debug, Error)]
pub enum GetError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    /// `collection` is not declared in `config.yml`. Reported separately
    /// from `NotFound` so a typo'd collection name is not mistaken for a
    /// missing document (mirrors `SearchError::UnknownCollection`).
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

    #[error("open sources table: {0}")]
    OpenSourcesTable(#[source] lancedb::Error),

    #[error("query sources: {0}")]
    QuerySources(#[source] lancedb::Error),

    #[error("open chunks table: {0}")]
    OpenChunksTable(#[source] lancedb::Error),

    #[error("query chunks: {0}")]
    QueryChunks(#[source] lancedb::Error),

    /// The collection is known but no row exists for `(collection, path)`.
    #[error("document not found: {collection}/{path}")]
    NotFound { collection: String, path: String },

    /// The document exists but has no chunk at `chunk_sequence`.
    /// Reported separately from `NotFound` so a stale `chunk_sequence` from
    /// an older index is distinguishable from a missing document.
    #[error("chunk {chunk_sequence} not found in {collection}/{path}")]
    ChunkNotFound {
        collection: String,
        path: String,
        chunk_sequence: u32,
    },

    /// A full document exceeded the configured output-size cap (the
    /// full-document read path only — chunk reads are never size-checked).
    /// The message is channel-neutral on purpose: the MCP layer surfaces it
    /// verbatim, where suggesting the CLI's `--no-size-limit` flag would
    /// mislead a client that has no such bypass. The CLI re-renders its own
    /// MiB-with-override sentence from the byte fields.
    #[error("document too large: {size_bytes} bytes exceeds the {limit_bytes}-byte limit")]
    DocumentTooLarge { size_bytes: u64, limit_bytes: u64 },
}

/// Translate a configured byte cap into an enforceable limit: `0` is the
/// documented "disable" sentinel (mirrors `runtime.memory_limit_mb`), so it
/// maps to `None` (no check). Any positive value is the cap to enforce.
pub fn configured_size_limit(max_bytes: u64) -> Option<u64> {
    (max_bytes != 0).then_some(max_bytes)
}

/// Reject `content` whose UTF-8 byte length exceeds `limit`. `None` disables
/// the check (the cap was set to `0`, or the CLI caller passed
/// `--no-size-limit`). Byte length is `str::len`, an O(1) read of the
/// already-loaded string — the cap guards what a full-document read *emits*
/// (a terminal flush or an LLM's context budget), not what ingest consumes.
/// Callers run this only on the full-document path; chunk bodies are out of
/// scope (they stay well under the cap by construction).
pub fn check_size_limit(content: &str, limit: Option<u64>) -> Result<(), GetError> {
    let Some(limit_bytes) = limit else {
        return Ok(());
    };
    let size_bytes = content.len() as u64;
    if size_bytes > limit_bytes {
        return Err(GetError::DocumentTooLarge {
            size_bytes,
            limit_bytes,
        });
    }
    Ok(())
}

/// Return the faithful original text of `collection`/`path` from the
/// `sources` table. Validates `collection` against `config.yml` first so
/// a typo surfaces as [`GetError::UnknownCollection`] rather than a
/// confusing [`GetError::NotFound`]. The `(collection, path)` predicate
/// flows through the typed `col`/`lit` builder, so the inputs never reach
/// a hand-built SQL string.
pub async fn get_document(
    config_dir: &Path,
    collection: &str,
    path: &str,
) -> Result<String, GetError> {
    let cfg = config::load(&config_dir.join("config.yml"))?;
    if !cfg.collections.contains_key(collection) {
        return Err(GetError::UnknownCollection {
            name: collection.to_string(),
        });
    }
    let table = open_sources_table(config_dir).await?;
    let stream = table
        .query()
        .only_if_expr(
            col(COL_COLLECTION)
                .eq(lit(collection))
                .and(col(COL_PATH).eq(lit(path))),
        )
        .select(Select::Columns(vec![COL_CONTENT.to_string()]))
        .limit(1)
        .execute()
        .await
        .map_err(GetError::QuerySources)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(GetError::QuerySources)?;
    first_string_column(&batches, COL_CONTENT).ok_or_else(|| GetError::NotFound {
        collection: collection.to_string(),
        path: path.to_string(),
    })
}

/// Return the `body` of one chunk identified by
/// `(collection, path, chunk_sequence)`. `chunks.body` is the lossy
/// chunked text (front matter stripped, overflow sub-chunks may overlap),
/// not the faithful source — that is the documented contract of the
/// `chunk` granularity. Validates `collection` against `config.yml` first
/// so a typo surfaces as [`GetError::UnknownCollection`] rather than a
/// confusing [`GetError::ChunkNotFound`], mirroring [`get_document`].
pub async fn get_chunk(
    config_dir: &Path,
    collection: &str,
    path: &str,
    chunk_sequence: u32,
) -> Result<String, GetError> {
    let cfg = config::load(&config_dir.join("config.yml"))?;
    if !cfg.collections.contains_key(collection) {
        return Err(GetError::UnknownCollection {
            name: collection.to_string(),
        });
    }
    let table = open_chunks_table(config_dir).await?;
    let stream = table
        .query()
        .only_if_expr(
            col(COL_COLLECTION)
                .eq(lit(collection))
                .and(col(COL_PATH).eq(lit(path)))
                .and(col(COL_CHUNK_SEQUENCE).eq(lit(chunk_sequence))),
        )
        .select(Select::Columns(vec![COL_BODY.to_string()]))
        .limit(1)
        .execute()
        .await
        .map_err(GetError::QueryChunks)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(GetError::QueryChunks)?;
    first_string_column(&batches, COL_BODY).ok_or_else(|| GetError::ChunkNotFound {
        collection: collection.to_string(),
        path: path.to_string(),
        chunk_sequence,
    })
}

/// Pull the first row's value of `col_name` out of the query result, or
/// `None` when no row matched. Both `get_document` (`content`) and
/// `get_chunk` (`body`) hit unique composite keys with `limit(1)`, so
/// there is at most one row. The caller selects `col_name` explicitly, so
/// its absence in a non-empty batch would be a structural invariant
/// violation, hence `expect` rather than folding it into the `None`
/// (not-found) path.
fn first_string_column(batches: &[RecordBatch], col_name: &str) -> Option<String> {
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let column: &StringArray = batch
            .column_by_name(col_name)
            .expect("query selected the requested column")
            .as_string();
        if column.is_valid(0) {
            return Some(column.value(0).to_string());
        }
    }
    None
}

async fn open_sources_table(config_dir: &Path) -> Result<Table, GetError> {
    let index_dir = config_dir.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| GetError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    let db = lancedb::connect(index_str)
        .execute()
        .await
        .map_err(|source| GetError::LancedbConnect {
            path: index_dir.clone(),
            source,
        })?;
    db.open_table(SOURCES_TABLE_NAME)
        .execute()
        .await
        .map_err(GetError::OpenSourcesTable)
}

// Intentionally parallel to `open_sources_table` rather than a single
// `open_table(name)` helper: the only inter-table difference is which
// `GetError::Open*Table` variant the failure maps to, and threading that
// through a string `table_name` would replace a typed dispatch with a
// runtime branch (any new table = silent fall-through). Two callsites is
// not yet "Rule of Three"; revisit once a third caller lands.
async fn open_chunks_table(config_dir: &Path) -> Result<Table, GetError> {
    let index_dir = config_dir.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| GetError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    let db = lancedb::connect(index_str)
        .execute()
        .await
        .map_err(|source| GetError::LancedbConnect {
            path: index_dir.clone(),
            source,
        })?;
    db.open_table(CHUNKS_TABLE_NAME)
        .execute()
        .await
        .map_err(GetError::OpenChunksTable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_size_limit_treats_zero_as_disabled() {
        assert_eq!(configured_size_limit(0), None);
    }

    #[test]
    fn configured_size_limit_passes_positive_value_through() {
        assert_eq!(configured_size_limit(1_048_576), Some(1_048_576));
    }

    #[test]
    fn check_size_limit_allows_content_at_or_below_the_cap() {
        // Exactly at the cap is allowed: the cap is the largest permitted size.
        assert!(check_size_limit("abc", Some(3)).is_ok());
        assert!(check_size_limit("ab", Some(3)).is_ok());
    }

    #[test]
    fn check_size_limit_rejects_content_over_the_cap_with_byte_fields() {
        let err = check_size_limit("abcd", Some(3)).expect_err("4 bytes over a 3-byte cap");
        match err {
            GetError::DocumentTooLarge {
                size_bytes,
                limit_bytes,
            } => {
                assert_eq!(size_bytes, 4);
                assert_eq!(limit_bytes, 3);
            }
            other => panic!("expected DocumentTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn check_size_limit_counts_utf8_bytes_not_chars() {
        // "あ" is 3 UTF-8 bytes but 1 char; the cap is a byte budget, so a
        // 2-byte cap must reject it (a char count would wrongly admit it).
        let err = check_size_limit("あ", Some(2)).expect_err("3 bytes over a 2-byte cap");
        assert!(
            matches!(err, GetError::DocumentTooLarge { size_bytes: 3, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn check_size_limit_skips_the_check_when_disabled() {
        // `None` (cap set to 0, or `--no-size-limit`) lets any size through.
        let huge = "x".repeat(10_000);
        assert!(check_size_limit(&huge, None).is_ok());
    }
}
