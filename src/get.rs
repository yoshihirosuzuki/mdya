//! `mdya get` / MCP `get_document` read paths: return either the
//! faithful full-document text from `sources`, or the original text a run
//! of chunks covers when the caller supplies a `chunk_sequence` (and
//! optionally a last `chunk_sequence`).
//!
//! Full-doc lookup is a point lookup by `(collection, path)`. `chunks.body`
//! is lossy (front matter stripped, Markdown formatting flattened, link
//! targets and HTML dropped), so the raw source cannot be reconstructed
//! from `chunks` and is stored verbatim in `sources` at ingest time. A
//! chunk read looks up the chunks' source ranges in `chunks` and slices
//! the matching `sources.content` with them, so it returns the original
//! text, formatting included, rather than the lossy body. Neither path
//! touches the filesystem.

use std::path::{Path, PathBuf};

use arrow_array::cast::AsArray;
use arrow_array::types::{UInt32Type, UInt64Type};
use arrow_array::{Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::Table;
use lancedb::expr::{col, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use thiserror::Error;

use crate::config;
use crate::store::{
    CHUNKS_TABLE_NAME, COL_CHUNK_SEQUENCE, COL_COLLECTION, COL_CONTENT, COL_PATH, COL_SOURCE_END,
    COL_SOURCE_HASH, COL_SOURCE_START, SOURCES_TABLE_NAME, chunks_schema_has_source_ranges,
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

    /// The document exists but has no chunk at `chunk_sequence` (the first
    /// chunk requested). Reported separately from `NotFound` so a stale
    /// `chunk_sequence` from an older index is distinguishable from a
    /// missing document.
    #[error("chunk {chunk_sequence} not found in {collection}/{path}")]
    ChunkNotFound {
        collection: String,
        path: String,
        chunk_sequence: u32,
    },

    /// A last chunk was given without a first one. The CLI rejects this at
    /// argument parsing; the MCP request reaches the library and is
    /// refused here.
    #[error("chunk_end requires chunk")]
    ChunkEndWithoutChunk,

    /// The last chunk precedes the first.
    #[error("chunk_end ({chunk_end}) must be >= chunk ({chunk})")]
    ChunkEndBeforeChunk { chunk: u32, chunk_end: u32 },

    /// The `chunks` table was built by an older mdya and has no source
    /// ranges, so a chunk read cannot return original text. `model` is the
    /// declared embedding model, so the suggested command rebuilds for it.
    /// Full-document reads never hit this: they do not touch `chunks`.
    #[error(
        "index is outdated: it was built by an older mdya and lacks chunk source \
         ranges. Rebuild it with `mdya vector use {model}`."
    )]
    IndexOutdated { model: String },

    /// The `chunks` and `sources` rows for a document disagree (different
    /// `source_hash`, or a range that does not fit the stored text). A run
    /// interrupted between the two tables' commits leaves this state, and
    /// the next `update-all` repairs it.
    #[error(
        "chunk index for {collection}/{path} is out of sync with its stored text; \
         run `mdya update-all` to repair it"
    )]
    IndexOutOfSync { collection: String, path: String },

    /// Output exceeded the configured size cap — a full document or a chunk
    /// read alike. The message is channel-neutral on purpose: the MCP layer
    /// surfaces it verbatim, where suggesting the CLI's `--no-size-limit`
    /// flag would mislead a client that has no such bypass. The CLI
    /// re-renders its own MiB-with-override sentence from the byte fields.
    #[error("content too large: {size_bytes} bytes exceeds the {limit_bytes}-byte limit")]
    ContentTooLarge { size_bytes: u64, limit_bytes: u64 },
}

/// The run of chunks a chunk read returns: from `first` through `last`,
/// both inclusive. A single-chunk read has `first == last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpan {
    first: u32,
    last: u32,
}

impl ChunkSpan {
    /// Build a span from the request's first and optional last chunk.
    /// Omitting `last` reads the single chunk `first`.
    pub fn new(first: u32, last: Option<u32>) -> Result<Self, GetError> {
        let last = last.unwrap_or(first);
        if last < first {
            return Err(GetError::ChunkEndBeforeChunk {
                chunk: first,
                chunk_end: last,
            });
        }
        Ok(Self { first, last })
    }

    /// Interpret the optional `chunk` / `chunk_end` pair of a request:
    /// `None` when neither is given (a full-document read), an error for a
    /// last chunk without a first one.
    pub fn from_request(
        chunk: Option<u32>,
        chunk_end: Option<u32>,
    ) -> Result<Option<Self>, GetError> {
        match (chunk, chunk_end) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(GetError::ChunkEndWithoutChunk),
            (Some(first), last) => Self::new(first, last).map(Some),
        }
    }

    pub fn first(&self) -> u32 {
        self.first
    }

    pub fn last(&self) -> u32 {
        self.last
    }
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
/// already-loaded string — the cap guards what a read *emits* (a terminal
/// flush or an LLM's context budget), not what ingest consumes. Callers run
/// this on both the full-document and the chunk path: a chunk range can
/// span a whole document, and even one chunk's range can be large (e.g. an
/// HTML block with no body text).
pub fn check_size_limit(content: &str, limit: Option<u64>) -> Result<(), GetError> {
    let Some(limit_bytes) = limit else {
        return Ok(());
    };
    let size_bytes = content.len() as u64;
    if size_bytes > limit_bytes {
        return Err(GetError::ContentTooLarge {
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
    load_config_for(config_dir, collection)?;
    let source = read_source(config_dir, collection, path).await?;
    source.map(|s| s.content).ok_or_else(|| GetError::NotFound {
        collection: collection.to_string(),
        path: path.to_string(),
    })
}

/// Return the original text chunks `span.first()` through `span.last()` of
/// `collection`/`path` cover: `sources.content` from the first chunk's range
/// start to the last chunk's range end. Chunk ranges tile the document, so
/// the result is one contiguous slice with no repeated text, and a span from
/// 0 to the last chunk returns the whole document.
///
/// A `span.last()` past the document's last chunk is clamped to it, so a
/// caller can read to the end without knowing the chunk count; a
/// `span.first()` past it is [`GetError::ChunkNotFound`]. Validates
/// `collection` against `config.yml` first, mirroring [`get_document`], and
/// refuses an index built without source ranges
/// ([`GetError::IndexOutdated`]).
pub async fn get_chunks(
    config_dir: &Path,
    collection: &str,
    path: &str,
    span: ChunkSpan,
) -> Result<String, GetError> {
    let cfg = load_config_for(config_dir, collection)?;
    let table = open_chunks_table(config_dir).await?;
    let schema = table.schema().await.map_err(GetError::QueryChunks)?;
    if !chunks_schema_has_source_ranges(&schema) {
        return Err(GetError::IndexOutdated {
            model: cfg.embedding.model,
        });
    }
    let rows = read_chunk_ranges(&table, collection, path, span).await?;
    let out_of_sync = || GetError::IndexOutOfSync {
        collection: collection.to_string(),
        path: path.to_string(),
    };
    let Some(first) = rows.iter().find(|r| r.chunk_sequence == span.first()) else {
        return Err(GetError::ChunkNotFound {
            collection: collection.to_string(),
            path: path.to_string(),
            chunk_sequence: span.first(),
        });
    };
    let last = rows
        .iter()
        .max_by_key(|r| r.chunk_sequence)
        .expect("rows holds at least the first chunk");
    let source = read_source(config_dir, collection, path)
        .await?
        .ok_or_else(out_of_sync)?;
    if source.source_hash != first.source_hash || source.source_hash != last.source_hash {
        return Err(out_of_sync());
    }
    let start = usize::try_from(first.source_start).map_err(|_| out_of_sync())?;
    let end = usize::try_from(last.source_end).map_err(|_| out_of_sync())?;
    source
        .content
        .get(start..end)
        .map(str::to_string)
        .ok_or_else(out_of_sync)
}

/// Load `config.yml` and reject a collection it does not declare.
fn load_config_for(config_dir: &Path, collection: &str) -> Result<config::Config, GetError> {
    let cfg = config::load(&config_dir.join("config.yml"))?;
    if !cfg.collections.contains_key(collection) {
        return Err(GetError::UnknownCollection {
            name: collection.to_string(),
        });
    }
    Ok(cfg)
}

/// One document's stored text and the hash it was ingested under.
struct StoredSource {
    content: String,
    source_hash: String,
}

/// Point lookup of one `sources` row, `None` when the document is absent.
async fn read_source(
    config_dir: &Path,
    collection: &str,
    path: &str,
) -> Result<Option<StoredSource>, GetError> {
    let table = open_sources_table(config_dir).await?;
    let stream = table
        .query()
        .only_if_expr(
            col(COL_COLLECTION)
                .eq(lit(collection))
                .and(col(COL_PATH).eq(lit(path))),
        )
        .select(Select::Columns(vec![
            COL_CONTENT.to_string(),
            COL_SOURCE_HASH.to_string(),
        ]))
        .limit(1)
        .execute()
        .await
        .map_err(GetError::QuerySources)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(GetError::QuerySources)?;
    Ok(first_source_row(&batches))
}

/// Pull the first row out of a `sources` query result, or `None` when no
/// row matched. The lookup hits the unique `(collection, path)` key with
/// `limit(1)`, so there is at most one row. The caller selects both columns
/// explicitly, so their absence in a non-empty batch would be a structural
/// invariant violation, hence `expect` rather than folding it into the
/// `None` (not-found) path.
fn first_source_row(batches: &[RecordBatch]) -> Option<StoredSource> {
    let batch = batches.iter().find(|b| b.num_rows() > 0)?;
    let content: &StringArray = batch
        .column_by_name(COL_CONTENT)
        .expect("query selected the content column")
        .as_string();
    let hash: &StringArray = batch
        .column_by_name(COL_SOURCE_HASH)
        .expect("query selected the source_hash column")
        .as_string();
    if !content.is_valid(0) || !hash.is_valid(0) {
        return None;
    }
    Some(StoredSource {
        content: content.value(0).to_string(),
        source_hash: hash.value(0).to_string(),
    })
}

/// The stored range of one chunk.
struct ChunkRangeRow {
    chunk_sequence: u32,
    source_start: u64,
    source_end: u64,
    source_hash: String,
}

/// Read the range rows of the chunks in `span` for one document. A plain
/// (non-vector) query has no implicit row limit, so every chunk in the span
/// comes back.
async fn read_chunk_ranges(
    table: &Table,
    collection: &str,
    path: &str,
    span: ChunkSpan,
) -> Result<Vec<ChunkRangeRow>, GetError> {
    let stream = table
        .query()
        .only_if_expr(
            col(COL_COLLECTION)
                .eq(lit(collection))
                .and(col(COL_PATH).eq(lit(path)))
                .and(col(COL_CHUNK_SEQUENCE).gt_eq(lit(span.first())))
                .and(col(COL_CHUNK_SEQUENCE).lt_eq(lit(span.last()))),
        )
        .select(Select::Columns(vec![
            COL_CHUNK_SEQUENCE.to_string(),
            COL_SOURCE_START.to_string(),
            COL_SOURCE_END.to_string(),
            COL_SOURCE_HASH.to_string(),
        ]))
        .execute()
        .await
        .map_err(GetError::QueryChunks)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(GetError::QueryChunks)?;
    Ok(batches.iter().flat_map(chunk_range_rows).collect())
}

fn chunk_range_rows(batch: &RecordBatch) -> Vec<ChunkRangeRow> {
    let column = |name: &str| {
        batch
            .column_by_name(name)
            .unwrap_or_else(|| panic!("query selected the {name} column"))
    };
    let sequences = column(COL_CHUNK_SEQUENCE).as_primitive::<UInt32Type>();
    let starts = column(COL_SOURCE_START).as_primitive::<UInt64Type>();
    let ends = column(COL_SOURCE_END).as_primitive::<UInt64Type>();
    let hashes: &StringArray = column(COL_SOURCE_HASH).as_string();
    (0..batch.num_rows())
        .filter(|&i| {
            sequences.is_valid(i) && starts.is_valid(i) && ends.is_valid(i) && hashes.is_valid(i)
        })
        .map(|i| ChunkRangeRow {
            chunk_sequence: sequences.value(i),
            source_start: starts.value(i),
            source_end: ends.value(i),
            source_hash: hashes.value(i).to_string(),
        })
        .collect()
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
    fn chunk_span_without_last_reads_a_single_chunk() {
        let span = ChunkSpan::new(3, None).expect("valid");
        assert_eq!((span.first(), span.last()), (3, 3));
    }

    #[test]
    fn chunk_span_accepts_last_equal_to_first() {
        assert_eq!(
            ChunkSpan::new(2, Some(2)).unwrap(),
            ChunkSpan::new(2, None).unwrap()
        );
    }

    #[test]
    fn chunk_span_rejects_last_before_first() {
        let err = ChunkSpan::new(5, Some(4)).expect_err("4 < 5");
        assert!(
            matches!(
                err,
                GetError::ChunkEndBeforeChunk {
                    chunk: 5,
                    chunk_end: 4
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn chunk_span_from_request_maps_each_combination() {
        assert_eq!(ChunkSpan::from_request(None, None).unwrap(), None);
        assert_eq!(
            ChunkSpan::from_request(Some(1), Some(4)).unwrap(),
            Some(ChunkSpan::new(1, Some(4)).unwrap())
        );
        let err = ChunkSpan::from_request(None, Some(4)).expect_err("end without start");
        assert!(matches!(err, GetError::ChunkEndWithoutChunk), "got {err:?}");
    }

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
            GetError::ContentTooLarge {
                size_bytes,
                limit_bytes,
            } => {
                assert_eq!(size_bytes, 4);
                assert_eq!(limit_bytes, 3);
            }
            other => panic!("expected ContentTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn check_size_limit_counts_utf8_bytes_not_chars() {
        // "あ" is 3 UTF-8 bytes but 1 char; the cap is a byte budget, so a
        // 2-byte cap must reject it (a char count would wrongly admit it).
        let err = check_size_limit("あ", Some(2)).expect_err("3 bytes over a 2-byte cap");
        assert!(
            matches!(err, GetError::ContentTooLarge { size_bytes: 3, .. }),
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
