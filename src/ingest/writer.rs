//! Ingest writer — orchestrates chunking → embedding → LanceDB upsert.
//! Lives between the leaf modules (walker / incremental / orphan) and
//! the public entry point (`super::mod`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::TimestampMicrosecondType;
use arrow_array::{
    Array, FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
    TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::Schema;
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use futures::stream::{self, StreamExt};
use lancedb::Table;
use lancedb::expr::{col, lit};
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::vector::IvfFlatIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::table::OptimizeAction;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::chunking::Chunk;
use crate::embedding::Embedder;
use crate::format::FileFormat;
use crate::store::lance_lm::ensure_lindera_ipadic_config;
use crate::store::metadata_check::{self, Outcome as MetadataOutcome};
use crate::store::{
    CHUNKS_TABLE_NAME, COL_BODY, COL_CHUNK_SEQUENCE, COL_COLLECTION, COL_EMBEDDING, COL_PATH,
    COL_SOURCE_HASH, SOURCES_TABLE_NAME,
};

/// The two LanceDB tables the ingest writer keeps in step: per-chunk
/// rows (`chunks`) and the per-file faithful source text (`sources`).
/// Lance has no cross-table atomic commit, so the two are written
/// in separate commits; consistency is structural, not ordering-based —
/// `update-all` only skips re-ingesting a file when its `source_hash`
/// agrees across both tables, so a crash that updates one but not the
/// other self-heals on the next run.
#[derive(Clone)]
struct Tables {
    chunks: Table,
    sources: Table,
}

use super::error::IngestError;
use super::incremental::{Action, DbRow};
use super::orphan::compute_orphans;
use super::progress::{FileOutcome, IngestProgress};
use super::walker::collect_ingestable_files;

/// Max chunks passed to `Embedder::embed_passages` in one call (grill
/// Q6). Worst-case activation ≈ 1.5 GB which fits well inside the
/// 8192 MB default `runtime.memory_limit_mb`.
///
/// `pub(crate)` keeps this an implementation detail — the snapshot
/// pinned "embed batch size の config 化 — 内部 const のまま" and
/// nothing outside the crate has reason to read it.
pub(crate) const EMBED_BATCH_SIZE: usize = 32;

/// Counters returned by `update_all_collections`. `new + updated +
/// skipped` covers every file the walker visited; `removed` counts
/// orphan deletes; `failed` counts per-file errors (file-level
/// failures do not abort the whole run).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSummary {
    pub new: u64,
    pub updated: u64,
    pub skipped: u64,
    pub removed: u64,
    pub failed: u64,
}

/// Public entry point used by the `mdya update-all` CLI subcommand and
/// (future) by the MCP server.
///
/// Takes `&BTreeMap<String, PathBuf>` rather than `&Config` so the
/// caller owns path expansion and validation, keeping the writer
/// testable from a thin fixture.
///
/// **Caller contract for `LANCE_LANGUAGE_MODEL_HOME`**: this function
/// writes `<config_dir>/lance-models/lindera/ipadic/config.yml` and
/// then drives Lance's FTS `create_index`. Lance reads its config from
/// `$LANCE_LANGUAGE_MODEL_HOME/lindera/ipadic/config.yml`.
/// The binary entry point (`src/main.rs`) sets the env var to
/// `<config_dir>/lance-models/` before the tokio runtime starts, so
/// the path the helper writes and the path Lance reads coincide.
/// Library callers (e.g. integration tests, future embedders) **must**
/// arrange the same redirect — point `LANCE_LANGUAGE_MODEL_HOME` at
/// `mdya::store::lance_lm::lance_models_dir(config_dir)` before
/// invoking this function. Otherwise Lance falls back to
/// `dirs::data_local_dir()/lance/language_models/` and the FTS index
/// build fails with "Invalid directory path".
pub async fn update_all_collections(
    collections: &BTreeMap<String, PathBuf>,
    config_dir: &Path,
    embedder: Arc<dyn Embedder>,
    progress: Arc<dyn IngestProgress>,
    parallelism: usize,
) -> Result<UpdateSummary, IngestError> {
    let tables = Tables {
        chunks: open_table(config_dir, CHUNKS_TABLE_NAME).await?,
        sources: open_table(config_dir, SOURCES_TABLE_NAME).await?,
    };
    // Write the tiny `lindera/ipadic/config.yml` Lance reads at FTS
    // `create_index` time. Idempotent — preserves mtime when
    // bytes already match — so calling this on every `update-all` is
    // cheap and keeps `mdya update-all` self-bootstrapping when the
    // user skipped `mdya init`.
    ensure_lindera_ipadic_config(config_dir)?;
    let declared_dim =
        i32::try_from(embedder.dim()).expect("embedder dim fits i32 (ruri-v3-30m = 256)");
    enforce_schema_metadata(
        metadata_check::check(&tables.chunks, embedder.model_id(), declared_dim)
            .await
            .map_err(IngestError::QueryChunks)?,
        embedder.model_id(),
        declared_dim,
    )?;
    let mut summary = UpdateSummary::default();
    for (name, root) in collections {
        update_one_collection(
            name,
            root,
            &tables,
            Arc::clone(&embedder),
            Arc::clone(&progress),
            &mut summary,
            parallelism,
        )
        .await?;
    }
    maintain_indices(&tables.chunks).await?;
    progress.finish();
    Ok(summary)
}

/// Build or refresh the FTS (body) and IVF_Flat (embedding) indices the
/// `mdya search fts` / `vector` / `hybrid` subcommands rely on. Runs at
/// the **end** of `update-all`:
///
/// 1. Short-circuit on a zero-row table — LanceDB's `create_index`
///    fails on an empty table.
/// 2. Skip existing indices rather than blindly re-creating, since the
///    skip path is roughly 400× faster on a 10K-row table.
/// 3. Always `optimize(All)` — OSS Lance requires manual reindexing of
///    new rows.
async fn maintain_indices(table: &Table) -> Result<(), IngestError> {
    if table
        .count_rows(None)
        .await
        .map_err(IngestError::CountRows)?
        == 0
    {
        return Ok(());
    }
    let existing = table
        .list_indices()
        .await
        .map_err(IngestError::ListIndices)?;
    // `IndexConfig.columns` is `Vec<String>` (lancedb-0.29.0/src/index.rs:357)
    // — currently always length 1 in practice but documented as
    // forward-compatible with composite indices, so iterate.
    let has_body_index = existing
        .iter()
        .any(|idx| idx.columns.iter().any(|c| c == COL_BODY));
    let has_embedding_index = existing
        .iter()
        .any(|idx| idx.columns.iter().any(|c| c == COL_EMBEDDING));
    if !has_body_index {
        create_fts_index(table, COL_BODY).await?;
    }
    // Build the IVF_Flat index only once the table holds at least one
    // real (non-null) embedding. A corpus made up solely of placeholder
    // chunks (zero-body files) has rows but 0 vectors, and IVF
    // K-means training fails with "cannot train K centroids with 0
    // vectors". The index is created on a later `update-all` once real
    // content lands; until then vector search falls back to brute force
    // over the (empty) vector set.
    if !has_embedding_index && has_any_embedding(table).await? {
        table
            .create_index(
                &[COL_EMBEDDING],
                Index::IvfFlat(IvfFlatIndexBuilder::default()),
            )
            .execute()
            .await
            .map_err(IngestError::CreateVectorIndex)?;
    }
    table
        .optimize(OptimizeAction::All)
        .await
        .map_err(IngestError::OptimizeIndex)?;
    Ok(())
}

/// Whether the `chunks` table holds at least one non-null embedding.
/// Placeholder chunks carry null embeddings, so a table can have
/// rows yet no vectors — IVF index training needs >=1.
async fn has_any_embedding(table: &Table) -> Result<bool, IngestError> {
    // `count_rows` takes a SQL predicate string (lancedb 0.29.0 has no
    // typed-expr overload). The interpolated value is a compile-time
    // column constant, never user input, so there is no injection surface.
    let n = table
        .count_rows(Some(format!("{COL_EMBEDDING} IS NOT NULL")))
        .await
        .map_err(IngestError::CountRows)?;
    Ok(n > 0)
}

/// Build a `lindera/ipadic` FTS index on the given column. Only the
/// `body` column is indexed; the `&'static str` column parameter keeps
/// the FTS builder defaults in one place should another text column
/// ever need the same index.
async fn create_fts_index(table: &Table, column: &'static str) -> Result<(), IngestError> {
    table
        .create_index(
            &[column],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await
        .map_err(|source| IngestError::CreateFtsIndex { column, source })?;
    Ok(())
}

async fn open_table(config_dir: &Path, table_name: &str) -> Result<Table, IngestError> {
    let index_dir = config_dir.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| IngestError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    let db = lancedb::connect(index_str)
        .execute()
        .await
        .map_err(|source| IngestError::LancedbConnect {
            path: index_dir.clone(),
            source,
        })?;
    db.open_table(table_name)
        .execute()
        .await
        .map_err(|e| match table_name {
            SOURCES_TABLE_NAME => IngestError::OpenSourcesTable(e),
            _ => {
                debug_assert_eq!(
                    table_name, CHUNKS_TABLE_NAME,
                    "open_table only opens the chunks / sources tables"
                );
                IngestError::OpenChunksTable(e)
            }
        })
}

/// Process one collection's `.md` files end-to-end.
///
/// **File-level** failures (read / chunk / embed / DB
/// write for a single file) are caught inside the loop, bumped into
/// `summary.failed`, and the caller continues with the next file.
/// **Infra-level** failures (DB connect, FS walk, pin check) propagate
/// with `?` and abort the whole `update-all` — that propagation lives
/// here at the `await?` boundaries deliberately.
async fn update_one_collection(
    name: &str,
    root: &Path,
    tables: &Tables,
    embedder: Arc<dyn Embedder>,
    progress: Arc<dyn IngestProgress>,
    summary: &mut UpdateSummary,
    parallelism: usize,
) -> Result<(), IngestError> {
    info!(collection = %name, root = %root.display(), "ingest start");
    let fs_paths_vec = collect_ingestable_files(root);
    let fs_paths: BTreeSet<PathBuf> = fs_paths_vec.iter().cloned().collect();
    let db_index = load_existing_rows(&tables.chunks, name).await?;
    // `sources.source_hash` per path: the comparator the skip gate checks
    // against `chunks.source_hash` so a crash-stale `sources` row forces
    // a re-ingest instead of being silently skipped.
    let source_hashes = load_existing_source_hashes(&tables.sources, name).await?;
    remove_orphans(tables, name, &fs_paths, &db_index, &source_hashes, summary).await?;
    progress.set_total_files(fs_paths_vec.len());
    // `runtime.embed_parallelism` lets users bound candle's forward peak,
    // which saturates memory in practice. `parallelism == 0` keeps the
    // sequential path as a disable sentinel.
    if parallelism == 0 {
        run_files_sequentially(
            name,
            root,
            tables,
            &embedder,
            &progress,
            summary,
            &fs_paths_vec,
            &db_index,
            &source_hashes,
        )
        .await;
    } else {
        run_files_in_parallel(
            name,
            root,
            tables,
            embedder,
            progress,
            summary,
            // `Vec` move (not borrow) so `stream::iter(...).into_iter()`
            // can consume the paths inside the parallel closure chain.
            fs_paths_vec,
            &db_index,
            &source_hashes,
            parallelism,
        )
        .await;
    }
    Ok(())
}

/// Sequential file processing — runs when `runtime.embed_parallelism = 0`.
/// Kept as a separate path (rather than `buffer_unordered(1)`) so users
/// who explicitly disable parallelism get a literal `for` loop with no
/// `spawn_blocking` overhead.
///
/// Signature note: takes `&Arc<_>` for `embedder` / `progress` because
/// the sequential loop only needs a per-iteration `Arc::clone` (no
/// task spawn that would require a `'static` move). The parallel side
/// (`run_files_in_parallel`) takes `Arc<_>` by value because each
/// closure spawned through `buffer_unordered` must own its handle.
#[allow(clippy::too_many_arguments)]
async fn run_files_sequentially(
    name: &str,
    root: &Path,
    tables: &Tables,
    embedder: &Arc<dyn Embedder>,
    progress: &Arc<dyn IngestProgress>,
    summary: &mut UpdateSummary,
    fs_paths_vec: &[PathBuf],
    db_index: &BTreeMap<PathBuf, DbRow>,
    source_hashes: &BTreeMap<PathBuf, String>,
) {
    for rel_path in fs_paths_vec {
        progress.start_file(rel_path);
        let outcome = process_file(
            name,
            root,
            rel_path,
            db_index.get(rel_path),
            source_hashes.get(rel_path).map(String::as_str),
            tables,
            Arc::clone(embedder),
        )
        .await
        .unwrap_or_else(|e| {
            warn!(
                collection = %name,
                path = %rel_path.display(),
                error = %e,
                "ingest file-level failure (continuing)",
            );
            FileOutcome::Failed
        });
        record_outcome(summary, outcome);
        progress.finish_file(rel_path, outcome);
    }
}

/// Parallel file processing — `runtime.embed_parallelism > 0`. Each
/// file's `process_file` future is buffered through
/// `stream::buffer_unordered(parallelism)`; the embed forward inside
/// `chunk_embed_and_replace` is itself offloaded to
/// `tokio::task::spawn_blocking` so the async runtime workers are not
/// stalled by candle's CPU pass. `summary` is accumulated by the
/// caller after all tasks complete so concurrent `record_outcome`
/// calls cannot race on it.
#[allow(clippy::too_many_arguments)]
async fn run_files_in_parallel(
    name: &str,
    root: &Path,
    tables: &Tables,
    embedder: Arc<dyn Embedder>,
    progress: Arc<dyn IngestProgress>,
    summary: &mut UpdateSummary,
    fs_paths_vec: Vec<PathBuf>,
    db_index: &BTreeMap<PathBuf, DbRow>,
    source_hashes: &BTreeMap<PathBuf, String>,
    parallelism: usize,
) {
    // The closure passed to `.map(...)` below cannot borrow `name` /
    // `root` / `tables` because each generated future must outlive
    // `run_files_in_parallel`'s stack frame. We own them once here so
    // the per-file closure can cheaply `clone()` an owned handle
    // instead of having to borrow up the call chain. `Tables::clone`
    // is two internal `Arc` clones (see `lancedb::Table { inner:
    // Arc<dyn BaseTable> }`) so the per-file clones are cheap.
    let name_owned = name.to_string();
    let root_owned = root.to_path_buf();
    let tables_owned = tables.clone();
    let outcomes: Vec<FileOutcome> = stream::iter(fs_paths_vec.into_iter().map(|rel_path| {
        let name = name_owned.clone();
        let root = root_owned.clone();
        let tables = tables_owned.clone();
        let embedder = Arc::clone(&embedder);
        let progress = Arc::clone(&progress);
        let existing = db_index.get(&rel_path).cloned();
        let source_hash = source_hashes.get(&rel_path).cloned();
        async move {
            progress.start_file(&rel_path);
            let outcome = process_file(
                &name,
                &root,
                &rel_path,
                existing.as_ref(),
                source_hash.as_deref(),
                &tables,
                embedder,
            )
            .await
            .unwrap_or_else(|e| {
                warn!(
                    collection = %name,
                    path = %rel_path.display(),
                    error = %e,
                    "ingest file-level failure (continuing)",
                );
                FileOutcome::Failed
            });
            progress.finish_file(&rel_path, outcome);
            outcome
        }
    }))
    .buffer_unordered(parallelism)
    .collect()
    .await;
    for outcome in outcomes {
        record_outcome(summary, outcome);
    }
}

fn record_outcome(summary: &mut UpdateSummary, outcome: FileOutcome) {
    match outcome {
        FileOutcome::New => summary.new += 1,
        FileOutcome::Updated => summary.updated += 1,
        FileOutcome::Skipped => summary.skipped += 1,
        FileOutcome::Failed => summary.failed += 1,
    }
}

async fn load_existing_rows(
    table: &Table,
    collection: &str,
) -> Result<BTreeMap<PathBuf, DbRow>, IngestError> {
    // `only_if_expr` builds the predicate from typed `col`/`lit` so the
    // collection name flows through DataFusion's expression layer and
    // never needs `sql_escape`. `Table::delete` and `UpdateBuilder::only_if`
    // below still take SQL strings (lancedb v0.29 has no type-safe write
    // API), so `sql_escape` survives for those sites only.
    let stream = table
        .query()
        .only_if_expr(col(COL_COLLECTION).eq(lit(collection)))
        .select(Select::Columns(vec![
            "path".to_string(),
            "modified_at".to_string(),
            "source_hash".to_string(),
        ]))
        .execute()
        .await
        .map_err(IngestError::QueryChunks)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(IngestError::QueryChunks)?;
    Ok(extract_existing_rows(&batches))
}

fn extract_existing_rows(batches: &[RecordBatch]) -> BTreeMap<PathBuf, DbRow> {
    let mut by_path: BTreeMap<PathBuf, DbRow> = BTreeMap::new();
    for batch in batches {
        let paths: &StringArray = batch
            .column_by_name("path")
            .expect("path column requested")
            .as_string();
        let mtimes = batch
            .column_by_name("modified_at")
            .expect("modified_at column requested")
            .as_primitive::<TimestampMicrosecondType>();
        let hashes: &StringArray = batch
            .column_by_name("source_hash")
            .expect("source_hash column requested")
            .as_string();
        for i in 0..paths.len() {
            // The schema declares all three columns `nullable = false`, but
            // `Array::value()` is unspecified on null indices, so guard
            // defensively in case a tampered DB violates that invariant.
            if !paths.is_valid(i) || !mtimes.is_valid(i) || !hashes.is_valid(i) {
                tracing::warn!(row = i, "non-nullable column carried null, skipping row");
                continue;
            }
            let path_str = paths.value(i);
            let Some(path) = validate_relative_path(path_str) else {
                tracing::warn!(
                    row = i,
                    path = path_str,
                    "ignoring row with non-relative or traversal path",
                );
                continue;
            };
            let micros = mtimes.value(i);
            let Some(mtime) = DateTime::<Utc>::from_timestamp_micros(micros) else {
                tracing::warn!(
                    row = i,
                    micros,
                    "ignoring row with out-of-range modified_at timestamp",
                );
                continue;
            };
            let row = DbRow {
                modified_at: mtime,
                source_hash: hashes.value(i).to_string(),
            };
            by_path.insert(path, row);
        }
    }
    by_path
}

async fn remove_orphans(
    tables: &Tables,
    collection: &str,
    fs_paths: &BTreeSet<PathBuf>,
    db_index: &BTreeMap<PathBuf, DbRow>,
    source_hashes: &BTreeMap<PathBuf, String>,
    summary: &mut UpdateSummary,
) -> Result<(), IngestError> {
    // Orphans = any `(collection, path)` indexed in chunks OR sources
    // that no longer exists on disk. Union the two key sets so an
    // interrupted previous run that left one table's row behind still
    // gets cleaned.
    let mut db_paths: BTreeSet<PathBuf> = db_index.keys().cloned().collect();
    db_paths.extend(source_hashes.keys().cloned());
    let orphans = compute_orphans(fs_paths, &db_paths);
    for path in &orphans {
        let path_str = path.to_string_lossy();
        let predicate = format!(
            "collection = '{}' AND path = '{}'",
            sql_escape(collection),
            sql_escape(&path_str),
        );
        tables
            .chunks
            .delete(&predicate)
            .await
            .map_err(IngestError::DeleteChunks)?;
        tables
            .sources
            .delete(&predicate)
            .await
            .map_err(IngestError::DeleteSources)?;
        // Counts orphaned `(collection, path)` pairs (present in chunks
        // and/or sources, including a half-written row from an interrupted
        // run), not necessarily distinct deleted files.
        summary.removed += 1;
    }
    Ok(())
}

async fn process_file(
    collection: &str,
    root: &Path,
    rel_path: &Path,
    existing: Option<&DbRow>,
    sources_hash: Option<&str>,
    tables: &Tables,
    embedder: Arc<dyn Embedder>,
) -> Result<FileOutcome, IngestError> {
    // The walker (`collect_ingestable_files`) only emits paths that
    // `FileFormat::from_path` recognises, so this `expect` is unreachable
    // unless a caller bypasses the walker — in which case we want a
    // panic at the boundary rather than a silent skip.
    let format = FileFormat::from_path(rel_path)
        .expect("walker only emits paths recognised by FileFormat::from_path");
    // Defense-in-depth: refuse traversal or absolute components before
    // forming `absolute`, even when the caller already supplies a
    // canonical relative path. `extract_existing_rows` also pre-filters
    // DB-derived paths via `validate_relative_path`, so this guard is
    // mainly insurance against future callers.
    ensure_under_root(rel_path)?;
    let absolute = root.join(rel_path);
    let fs_mtime = read_mtime(&absolute)?;
    // True only when this path has a `chunks` row AND the `sources` row
    // agrees on `source_hash`. False for a new file (`existing == None`),
    // where no comparison applies — those always take the write path
    // below. A `false` for an *existing* file means the two tables
    // diverged because a previous run was interrupted between their
    // (non-atomic) commits, and must be repaired.
    let sources_mirror_chunks =
        existing.is_some_and(|row| sources_hash == Some(row.source_hash.as_str()));
    // Fast path: mtime unchanged AND sources mirrors chunks → skip
    // without reading the file.
    if existing.is_some_and(|row| row.modified_at == fs_mtime) && sources_mirror_chunks {
        return Ok(FileOutcome::Skipped);
    }
    let (bytes, source_hash) = read_bytes_with_hash(&absolute)?;
    // `source_hash` is taken over the raw file bytes so the hash semantic
    // stays uniform across formats and an extractor version bump does not
    // silently trigger re-ingest of every PDF — that case is handled by
    // the changelog + a manual-recovery convention. Extraction (UTF-8
    // strict for Markdown, pdf-extract for PDF) happens *after* the hash,
    // and any extractor failure surfaces through `IngestError::Extract` to
    // the per-file `unwrap_or_else` in the writer loop (warn +
    // `FileOutcome::Failed`, file is retried on the next `update-all`).
    let content = format.extract(&bytes)?;
    let action = Action::decide(fs_mtime, &source_hash, existing);
    let content_changed = matches!(action, Action::New | Action::Reingest);
    // Keep `sources` mirroring the file: after any content change, and
    // otherwise only to repair a stale/missing row left by an interrupted
    // run. This is a separate Lance commit from the chunks write below
    // (no cross-table atomicity), so the source_hash gate above is what
    // makes a half-written pair self-heal on the next run.
    if content_changed || !sources_mirror_chunks {
        upsert_source(
            &tables.sources,
            collection,
            rel_path,
            &source_hash,
            &content,
        )
        .await?;
    }
    match action {
        Action::Skip => Ok(FileOutcome::Skipped),
        Action::TouchMtime => {
            update_mtime(&tables.chunks, collection, rel_path, fs_mtime).await?;
            Ok(FileOutcome::Skipped)
        }
        Action::Reingest => {
            chunk_embed_and_upsert(
                format,
                collection,
                rel_path,
                &content,
                fs_mtime,
                &source_hash,
                &tables.chunks,
                embedder,
            )
            .await?;
            Ok(FileOutcome::Updated)
        }
        Action::New => {
            chunk_embed_and_upsert(
                format,
                collection,
                rel_path,
                &content,
                fs_mtime,
                &source_hash,
                &tables.chunks,
                embedder,
            )
            .await?;
            Ok(FileOutcome::New)
        }
    }
}

fn read_mtime(absolute: &Path) -> Result<DateTime<Utc>, IngestError> {
    let metadata = std::fs::metadata(absolute)?;
    let modified = metadata.modified()?;
    Ok(DateTime::<Utc>::from(modified))
}

/// Read a file as raw bytes and return both the bytes and their SHA-256
/// hex digest. Hashing happens over raw bytes (not the extracted text)
/// so the comparator semantic stays uniform across formats.
///
/// UTF-8 decoding is no longer this function's concern: the per-format
/// `FileFormat::extract` step decides whether the bytes are Markdown
/// (strict UTF-8) or PDF (delegated to `pdf-extract`). Format-specific
/// errors surface via `IngestError::Extract`.
fn read_bytes_with_hash(absolute: &Path) -> Result<(Vec<u8>, String), IngestError> {
    let bytes = std::fs::read(absolute)?;
    let digest = Sha256::digest(&bytes);
    let hex = format!("{digest:x}");
    debug_assert_eq!(hex.len(), 64, "sha256 hex must be 64 chars");
    Ok((bytes, hex))
}

/// UPSERT the `modified_at` column for one `(collection, path)` row
/// without re-chunking the file.
///
/// The `micros` value spliced into the SQL `column` expression is
/// `mtime.timestamp_micros()` — sourced from a `DateTime<Utc>` built
/// by `read_mtime` from `std::fs::Metadata::modified()`. chrono and
/// Arrow's `TimestampMicrosecond` share the same i64 representation,
/// so the literal cannot land outside Arrow's range and DataFusion
/// accepts it for any filesystem mtime (including pre-1970 negatives
/// that APFS / ext4 preserve). The `format!` substitution is therefore
/// a literal whose value cannot escape DataFusion's literal grammar.
async fn update_mtime(
    table: &Table,
    collection: &str,
    rel_path: &Path,
    mtime: DateTime<Utc>,
) -> Result<(), IngestError> {
    let path_str = rel_path.to_string_lossy();
    let micros = mtime.timestamp_micros();
    let predicate = format!(
        "collection = '{}' AND path = '{}'",
        sql_escape(collection),
        sql_escape(&path_str),
    );
    table
        .update()
        .only_if(predicate)
        .column(
            "modified_at",
            format!("arrow_cast({micros}, 'Timestamp(Microsecond, Some(\"UTC\"))')"),
        )
        .execute()
        .await
        .map_err(IngestError::UpdateChunks)?;
    Ok(())
}

/// Re-chunk one file and UPSERT via `merge_insert` so the replacement
/// is atomic (one Lance commit). The delete clause is scoped to a
/// single `(collection, path)` — without scoping the join would touch
/// sibling files, and without the delete entirely a shrinking file
/// (5 chunks → 3) would leave stale rows behind.
///
/// `chunk_markdown` always returns at least one chunk: empty bodies
/// produce a null-embedded placeholder, so every row has a `sources`
/// mirror while empty bodies stay out of the vector index.
#[allow(clippy::too_many_arguments)]
async fn chunk_embed_and_upsert(
    format: FileFormat,
    collection: &str,
    rel_path: &Path,
    content: &str,
    mtime: DateTime<Utc>,
    source_hash: &str,
    table: &Table,
    embedder: Arc<dyn Embedder>,
) -> Result<(), IngestError> {
    let chunks = format.chunk(content)?;
    debug_assert!(
        !chunks.is_empty(),
        "FileFormat::chunk guarantees >=1 chunk via the placeholder \
         (Markdown: `placeholder_chunk`; PDF: empty-text fallback)"
    );
    let path_str = rel_path.to_string_lossy();
    let scope_filter = format!(
        "collection = '{}' AND path = '{}'",
        sql_escape(collection),
        sql_escape(&path_str),
    );
    // Offload the candle forward pass to tokio's blocking worker pool
    // so the async runtime threads stay free for `Table::add` / other
    // files' I/O. `embedder` (Arc) and `chunks` (Vec clone) move into
    // the closure; the original `chunks` is reused below for
    // `build_record_batch`.
    let vectors = {
        let embedder_for_embed = Arc::clone(&embedder);
        let chunks_for_embed = chunks.clone();
        tokio::task::spawn_blocking(move || {
            embed_chunks_in_batches(&*embedder_for_embed, &chunks_for_embed)
        })
        .await
        .map_err(IngestError::EmbedJoin)??
    };
    let batch = build_record_batch(
        table.schema().await.map_err(IngestError::OpenChunksTable)?,
        collection,
        &path_str,
        mtime,
        source_hash,
        embedder.dim(),
        &chunks,
        &vectors,
    )?;
    let schema = batch.schema();
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    let mut builder = table.merge_insert(&[COL_COLLECTION, COL_PATH, COL_CHUNK_SEQUENCE]);
    builder.when_matched_update_all(None);
    builder.when_not_matched_insert_all();
    builder.when_not_matched_by_source_delete(Some(scope_filter));
    let result = builder
        .execute(reader)
        .await
        .map_err(IngestError::WriteChunks)?;
    debug!(
        collection = %collection,
        path = %path_str,
        inserted = result.num_inserted_rows,
        updated = result.num_updated_rows,
        deleted = result.num_deleted_rows,
        attempts = result.num_attempts,
        version = result.version,
        "merge_insert upsert",
    );
    Ok(())
}

fn embed_chunks_in_batches(
    embedder: &dyn Embedder,
    chunks: &[Chunk],
) -> Result<Vec<Vec<f32>>, IngestError> {
    let mut all = Vec::with_capacity(chunks.len());
    for batch in chunks.chunks(EMBED_BATCH_SIZE) {
        // Skip placeholder chunks (empty body) — they get a null
        // embedding, not an embedded empty string, so they stay out of
        // the vector index. Real sections never flush an empty body.
        let texts: Vec<&str> = batch
            .iter()
            .filter(|c| !c.body.is_empty())
            .map(|c| c.body.as_str())
            .collect();
        if texts.is_empty() {
            continue;
        }
        let vectors = embedder.embed_passages(&texts)?;
        all.extend(vectors);
    }
    Ok(all)
}

#[allow(clippy::too_many_arguments)]
fn build_record_batch(
    schema: Arc<Schema>,
    collection: &str,
    path: &str,
    mtime: DateTime<Utc>,
    source_hash: &str,
    vector_dim: usize,
    chunks: &[Chunk],
    vectors: &[Vec<f32>],
) -> Result<RecordBatch, IngestError> {
    let n = chunks.len();
    // One vector per non-empty-body chunk; the placeholder (empty body)
    // carries no vector and is stored as a null embedding.
    debug_assert_eq!(
        vectors.len(),
        chunks.iter().filter(|c| !c.body.is_empty()).count()
    );
    let collection_col = StringArray::from(vec![collection; n]);
    let path_col = StringArray::from(vec![path; n]);
    let n_u32 = u32::try_from(n).map_err(|_| IngestError::TooManyChunks { count: n })?;
    let chunk_sequence_col = UInt32Array::from((0..n_u32).collect::<Vec<u32>>());
    let body_col = StringArray::from(chunks.iter().map(|c| c.body.as_str()).collect::<Vec<_>>());
    let embedding_col = build_embedding_array(chunks, vectors, vector_dim)?;
    let micros = mtime.timestamp_micros();
    let modified_at_col =
        TimestampMicrosecondArray::from(vec![micros; n]).with_timezone("UTC".to_string());
    let source_hash_col = StringArray::from(vec![source_hash; n]);
    let columns: Vec<Arc<dyn Array>> = vec![
        Arc::new(collection_col),
        Arc::new(path_col),
        Arc::new(chunk_sequence_col),
        Arc::new(body_col),
        Arc::new(embedding_col),
        Arc::new(modified_at_col),
        Arc::new(source_hash_col),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Lift `metadata_check::Outcome` to the matching `IngestError`. `Pass`
/// returns `Ok(())` so the caller can chain it on `?`.
fn enforce_schema_metadata(
    outcome: MetadataOutcome,
    declared_model: &str,
    declared_dim: i32,
) -> Result<(), IngestError> {
    match outcome {
        MetadataOutcome::Pass => Ok(()),
        MetadataOutcome::Mismatch {
            actual_embedding_model,
            actual_vector_dim,
        } => Err(IngestError::SchemaMetadataMismatch {
            declared_model: declared_model.to_string(),
            declared_dim,
            actual_embedding_model,
            actual_vector_dim,
        }),
        MetadataOutcome::Missing { absent_keys } => {
            Err(IngestError::SchemaMetadataMissing { absent_keys })
        }
    }
}

/// Build the `embedding` column for `chunks`. Non-empty-body chunks
/// consume `vectors` in order; the placeholder (empty body) gets a
/// null entry so it is excluded from the IVF_Flat index (empirically
/// verified, lancedb 0.29.0). The `FixedSizeList` still needs `dim`
/// child values written before a null `append(false)`, so we pad with
/// zeros that the null bitmap then masks.
fn build_embedding_array(
    chunks: &[Chunk],
    vectors: &[Vec<f32>],
    dim: usize,
) -> Result<FixedSizeListArray, IngestError> {
    use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
    let dim_i32 = i32::try_from(dim).map_err(|_| IngestError::DimTooLarge { dim })?;
    let values_builder = Float32Builder::with_capacity(vectors.len() * dim);
    let mut builder = FixedSizeListBuilder::new(values_builder, dim_i32);
    let mut next_vector = vectors.iter();
    for chunk in chunks {
        if chunk.body.is_empty() {
            builder.values().append_slice(&vec![0.0_f32; dim]);
            builder.append(false);
            continue;
        }
        let v = next_vector
            .next()
            .expect("embed_chunks_in_batches yields one vector per non-empty-body chunk");
        debug_assert_eq!(v.len(), dim);
        builder.values().append_slice(v);
        builder.append(true);
    }
    Ok(builder.finish())
}

/// UPSERT the faithful original `content` into the `sources` table for
/// one `(collection, path)`. `merge_insert` on the composite key
/// updates the row in place when the file is re-ingested and inserts it
/// when new — a single Lance commit, independent of the `chunks` write.
async fn upsert_source(
    sources: &Table,
    collection: &str,
    rel_path: &Path,
    source_hash: &str,
    content: &str,
) -> Result<(), IngestError> {
    let path_str = rel_path.to_string_lossy();
    // The sources schema is static (no embedding-model / vector_dim
    // pin), so build it locally instead of paying a `sources.schema()`
    // round-trip per file during parallel ingest.
    let schema = Arc::new(crate::store::sources_schema());
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from(vec![collection])),
            Arc::new(StringArray::from(vec![path_str.as_ref()])),
            Arc::new(StringArray::from(vec![source_hash])),
            Arc::new(StringArray::from(vec![content])),
        ],
    )?;
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    let mut builder = sources.merge_insert(&[COL_COLLECTION, COL_PATH]);
    builder.when_matched_update_all(None);
    builder.when_not_matched_insert_all();
    builder
        .execute(reader)
        .await
        .map_err(IngestError::WriteSources)?;
    Ok(())
}

/// Load `path -> source_hash` for one collection's `sources` rows. The
/// skip gate compares each value against the matching `chunks`
/// `source_hash` so a crash-stale `sources` row forces re-ingest.
async fn load_existing_source_hashes(
    sources: &Table,
    collection: &str,
) -> Result<BTreeMap<PathBuf, String>, IngestError> {
    let stream = sources
        .query()
        .only_if_expr(col(COL_COLLECTION).eq(lit(collection)))
        .select(Select::Columns(vec![
            COL_PATH.to_string(),
            COL_SOURCE_HASH.to_string(),
        ]))
        .execute()
        .await
        .map_err(IngestError::QuerySources)?;
    let batches = stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(IngestError::QuerySources)?;
    Ok(extract_source_hashes(&batches))
}

fn extract_source_hashes(batches: &[RecordBatch]) -> BTreeMap<PathBuf, String> {
    let mut by_path = BTreeMap::new();
    for batch in batches {
        let paths: &StringArray = batch
            .column_by_name(COL_PATH)
            .expect("path column requested")
            .as_string();
        let hashes: &StringArray = batch
            .column_by_name(COL_SOURCE_HASH)
            .expect("source_hash column requested")
            .as_string();
        for i in 0..paths.len() {
            if !paths.is_valid(i) || !hashes.is_valid(i) {
                tracing::warn!(row = i, "sources row carried null, skipping");
                continue;
            }
            let path_str = paths.value(i);
            let Some(path) = validate_relative_path(path_str) else {
                tracing::warn!(
                    row = i,
                    path = path_str,
                    "ignoring sources row with non-relative or traversal path",
                );
                continue;
            };
            by_path.insert(path, hashes.value(i).to_string());
        }
    }
    by_path
}

/// Validate that a path string read from the `chunks` or `sources`
/// table is a safe relative path: no parent-dir traversal (`..`),
/// no absolute root, no Windows prefix. Used by
/// `extract_existing_rows` and `extract_source_hashes` to drop rows
/// whose stored `path` would otherwise escape the collection root
/// once later joined with `root.join(...)`. Returns `None` on any
/// rejected component so the caller can log and continue rather
/// than abort the surrounding loop.
fn validate_relative_path(s: &str) -> Option<PathBuf> {
    let p = PathBuf::from(s);
    if p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        None
    } else {
        Some(p)
    }
}

/// Defense-in-depth invariant for `process_file`: refuse a relative
/// path that would escape the collection root once joined. Called
/// before `root.join(rel_path)` so a future caller that bypasses
/// `extract_existing_rows`'s pre-filter still cannot reach a sibling
/// directory via `..` / absolute components.
fn ensure_under_root(rel_path: &Path) -> Result<(), IngestError> {
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        Err(IngestError::PathTraversal {
            rel_path: rel_path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

/// Double single quotes inside a string literal so it can be safely
/// concatenated into a LanceDB SQL predicate. Callers must wrap the
/// returned value in `'…'` themselves.
fn sql_escape(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_outcome_increments_the_matching_counter() {
        let mut s = UpdateSummary::default();
        record_outcome(&mut s, FileOutcome::New);
        record_outcome(&mut s, FileOutcome::New);
        record_outcome(&mut s, FileOutcome::Updated);
        record_outcome(&mut s, FileOutcome::Skipped);
        record_outcome(&mut s, FileOutcome::Failed);
        assert_eq!(s.new, 2);
        assert_eq!(s.updated, 1);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.removed, 0);
    }

    #[test]
    fn sql_escape_doubles_single_quotes() {
        assert_eq!(sql_escape("foo"), "foo");
        assert_eq!(sql_escape("foo'bar"), "foo''bar");
        assert_eq!(sql_escape("'leading"), "''leading");
        assert_eq!(sql_escape("trailing'"), "trailing''");
    }

    #[test]
    fn validate_relative_path_accepts_simple_relative_paths() {
        assert!(validate_relative_path("foo.md").is_some());
        assert!(validate_relative_path("dir/sub/file.md").is_some());
    }

    #[test]
    fn validate_relative_path_rejects_parent_dir() {
        assert!(validate_relative_path("../foo").is_none());
        assert!(validate_relative_path("foo/../bar").is_none());
        assert!(validate_relative_path("..").is_none());
    }

    #[test]
    fn validate_relative_path_rejects_absolute_root() {
        assert!(validate_relative_path("/etc/shadow").is_none());
    }

    #[test]
    fn ensure_under_root_accepts_relative_path() {
        let p = Path::new("foo/bar.md");
        assert!(ensure_under_root(p).is_ok());
    }

    #[test]
    fn ensure_under_root_rejects_parent_dir() {
        let p = Path::new("../foo");
        let err = ensure_under_root(p).unwrap_err();
        assert!(matches!(err, IngestError::PathTraversal { .. }));
    }

    #[test]
    fn ensure_under_root_rejects_absolute_root() {
        let p = Path::new("/etc/shadow");
        let err = ensure_under_root(p).unwrap_err();
        assert!(matches!(err, IngestError::PathTraversal { .. }));
    }

    #[test]
    fn build_embedding_array_rejects_dim_exceeding_i32_max() {
        let chunks: Vec<Chunk> = vec![];
        let vectors: Vec<Vec<f32>> = vec![];
        let dim = (i32::MAX as usize) + 1;
        let err = build_embedding_array(&chunks, &vectors, dim).unwrap_err();
        assert!(matches!(err, IngestError::DimTooLarge { dim: d } if d == dim));
    }
}
