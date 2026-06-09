//! Empirical verification of LanceDB index behaviour.
//!
//! These tests are gated behind `#[ignore]` and run only via
//! `just lancedb-verify`. Each test isolates one behavioural axis of
//! the `lancedb = "=0.29.0"` API we depend on and prints findings via
//! `println!` so they end up in the `--nocapture` output.
//!
//! Do not relax the `#[ignore]` attribute — these tests are slow and
//! the findings need a maintainer pass before they get pinned as
//! invariants.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::cast::AsArray;
use arrow_array::{
    Array, FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
    TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::Schema;
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::vector::IvfFlatIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::table::OptimizeAction;
use lancedb::{Connection, Table};
use tempfile::TempDir;

use mdya::store::{CHUNKS_TABLE_NAME, chunks_schema};

const VECTOR_DIM: i32 = 256;
const MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const BENCH_REPEATS: usize = 3;

// ───────────────────────────── helpers ─────────────────────────────────────

fn schema_arc() -> Arc<Schema> {
    Arc::new(chunks_schema(VECTOR_DIM, MODEL_ID))
}

async fn fresh_connection(tmp: &TempDir) -> Result<Connection> {
    let path = tmp.path().join("index");
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_str().context("UTF-8 tempdir path")?;
    Ok(lancedb::connect(path_str).execute().await?)
}

async fn fresh_empty_table(tmp: &TempDir) -> Result<Table> {
    let db = fresh_connection(tmp).await?;
    let tbl = db
        .create_empty_table(CHUNKS_TABLE_NAME, schema_arc())
        .execute()
        .await?;
    Ok(tbl)
}

/// Build a one-row `RecordBatch` for the 7 columns of the `chunks`
/// table: `(collection, path, chunk_sequence, body, embedding,
/// modified_at, source_hash)`. The `embedding` is a pseudo-vector of
/// `dim` values derived from `seed`.
fn build_one_row_batch(seed: u64) -> RecordBatch {
    let schema = schema_arc();
    let collection = StringArray::from(vec!["notes"]);
    let path = StringArray::from(vec![format!("doc-{seed}.md")]);
    let chunk_sequence = UInt32Array::from(vec![0_u32]);
    let body = StringArray::from(vec![format!(
        "hello world from row {seed} indexed body content"
    )]);
    let embedding = deterministic_embedding(seed);
    let modified_at = TimestampMicrosecondArray::from(vec![1_700_000_000_000_000_i64])
        .with_timezone("UTC".to_string());
    let source_hash = StringArray::from(vec![format!("{:0>64}", seed)]);
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(collection),
            Arc::new(path),
            Arc::new(chunk_sequence),
            Arc::new(body),
            Arc::new(embedding),
            Arc::new(modified_at),
            Arc::new(source_hash),
        ],
    )
    .unwrap_or_else(|e| panic!("RecordBatch::try_new failed (schema / columns mismatch?): {e}"))
}

fn deterministic_embedding(seed: u64) -> FixedSizeListArray {
    let values = Float32Builder::with_capacity(VECTOR_DIM as usize);
    let mut builder = FixedSizeListBuilder::new(values, VECTOR_DIM);
    let v: Vec<f32> = (0..VECTOR_DIM)
        .map(|i| ((seed as f32) + (i as f32)).sin())
        .collect();
    builder.values().append_slice(&v);
    builder.append(true);
    builder.finish()
}

async fn insert_rows(tbl: &Table, seeds: impl IntoIterator<Item = u64>) -> Result<()> {
    let schema = schema_arc();
    let batches: Vec<_> = seeds
        .into_iter()
        .map(|s| Ok(build_one_row_batch(s)))
        .collect();
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(batches, schema));
    tbl.add(reader).execute().await?;
    Ok(())
}

async fn count_rows(tbl: &Table) -> Result<usize> {
    let stream = tbl.query().execute().await?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    Ok(batches.iter().map(|b| b.num_rows()).sum())
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn finding(name: &str, msg: impl std::fmt::Display) {
    println!("[finding] {name}: {msg}");
}

// ───────────────────────────── axis 1 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_01_create_index_on_empty_table() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    let fts_result = tbl
        .create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await;
    finding(
        "axis_01_fts_on_empty",
        format!(
            "create_index FTS on 0-row table = {:?}",
            fts_result.as_ref().map(|_| "Ok")
        ),
    );
    let vector_result = tbl
        .create_index(
            &["embedding"],
            Index::IvfFlat(IvfFlatIndexBuilder::default()),
        )
        .execute()
        .await;
    finding(
        "axis_01_ivf_flat_on_empty",
        format!(
            "create_index IVF_Flat on 0-row table = {:?}",
            vector_result.as_ref().map(|_| "Ok")
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 2 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_02_full_text_search_without_index() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1]).await?;
    let search_result = tbl
        .query()
        .full_text_search(FullTextSearchQuery::new("hello".to_string()))
        .limit(10)
        .execute()
        .await;
    match search_result {
        Ok(stream) => {
            let batches: Vec<RecordBatch> = stream.try_collect().await?;
            let n: usize = batches.iter().map(|b| b.num_rows()).sum();
            finding(
                "axis_02_fts_no_index",
                format!("full_text_search succeeded without index, hits = {n}"),
            );
        }
        Err(e) => finding(
            "axis_02_fts_no_index",
            format!("full_text_search FAILED without index: {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis 3 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_03_vector_search_without_index() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1]).await?;
    let query_vec: Vec<f32> = (0..VECTOR_DIM)
        .map(|i| (1.0_f32 + i as f32).sin())
        .collect();
    let search_result = tbl.query().nearest_to(query_vec)?.limit(10).execute().await;
    match search_result {
        Ok(stream) => {
            let batches: Vec<RecordBatch> = stream.try_collect().await?;
            let n: usize = batches.iter().map(|b| b.num_rows()).sum();
            finding(
                "axis_03_vector_no_index",
                format!("vector_search succeeded without index, hits = {n}"),
            );
        }
        Err(e) => finding(
            "axis_03_vector_no_index",
            format!("vector_search FAILED without index: {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis 4 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_04_create_index_replace_semantics() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1, 2, 3]).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    let second = tbl
        .create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .replace(false)
        .execute()
        .await;
    finding(
        "axis_04_replace_false_on_existing",
        format!(
            "create_index(replace=false) on existing = {:?}",
            second.as_ref().err()
        ),
    );
    let third = tbl
        .create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .replace(true)
        .execute()
        .await;
    finding(
        "axis_04_replace_true_on_existing",
        format!(
            "create_index(replace=true) on existing = {:?}",
            third.as_ref().map(|_| "Ok")
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 5 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_05_insert_visibility_without_optimize() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1]).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    insert_rows(&tbl, [2, 3]).await?;
    let stream = tbl
        .query()
        .full_text_search(FullTextSearchQuery::new("hello".to_string()))
        .limit(10)
        .execute()
        .await?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    let hits: usize = batches.iter().map(|b| b.num_rows()).sum();
    finding(
        "axis_05_post_insert_no_optimize",
        format!("FTS hits after insert without optimize = {hits} (expected 3 if visible)"),
    );
    Ok(())
}

// ───────────────────────────── axis 6 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_06_optimize_integrates_new_rows() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1]).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    insert_rows(&tbl, [2, 3]).await?;
    let opt_started = Instant::now();
    tbl.optimize(lancedb::table::OptimizeAction::All).await?;
    let opt_elapsed = opt_started.elapsed();
    let stream = tbl
        .query()
        .full_text_search(FullTextSearchQuery::new("hello".to_string()))
        .limit(10)
        .execute()
        .await?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    let hits: usize = batches.iter().map(|b| b.num_rows()).sum();
    finding(
        "axis_06_after_optimize",
        format!(
            "FTS hits after optimize = {hits} (expected 3); optimize() elapsed = {:?}",
            opt_elapsed
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 7 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_07_query_stability_immediately_after_optimize() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, (1..=50).collect::<Vec<_>>()).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    insert_rows(&tbl, (51..=100).collect::<Vec<_>>()).await?;
    tbl.optimize(lancedb::table::OptimizeAction::All).await?;
    let mut runs = Vec::new();
    for _ in 0..3 {
        let stream = tbl
            .query()
            .full_text_search(FullTextSearchQuery::new("hello".to_string()))
            .limit(1000)
            .execute()
            .await?;
        let batches: Vec<RecordBatch> = stream.try_collect().await?;
        let n: usize = batches.iter().map(|b| b.num_rows()).sum();
        runs.push(n);
    }
    finding(
        "axis_07_query_stability_after_optimize",
        format!("3 successive FTS counts after optimize = {runs:?} (expected all 100)"),
    );
    Ok(())
}

// ───────────────────────────── axis 8 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_08_optimize_cost_at_10k_rows() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, 1..=10_000_u64).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    // Warm-up
    tbl.optimize(lancedb::table::OptimizeAction::All).await?;
    let mut samples = Vec::with_capacity(BENCH_REPEATS);
    for _ in 0..BENCH_REPEATS {
        insert_rows(&tbl, [99_999_u64]).await?;
        let t = Instant::now();
        tbl.optimize(lancedb::table::OptimizeAction::All).await?;
        samples.push(t.elapsed());
    }
    finding(
        "axis_08_optimize_cost_10k",
        format!(
            "optimize() median (3 runs on ~10K rows) = {:?}",
            median(samples)
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 9 ──────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_09_brute_force_latency_at_10k_rows() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, 1..=10_000_u64).await?;
    let mut fts_samples = Vec::with_capacity(BENCH_REPEATS);
    let mut vec_samples = Vec::with_capacity(BENCH_REPEATS);
    let query_vec: Vec<f32> = (0..VECTOR_DIM)
        .map(|i| (1.0_f32 + i as f32).sin())
        .collect();
    // Warm-up
    let _ = tbl
        .query()
        .full_text_search(FullTextSearchQuery::new("hello".to_string()))
        .limit(20)
        .execute()
        .await;
    let _ = tbl
        .query()
        .nearest_to(query_vec.clone())?
        .limit(20)
        .execute()
        .await;
    for _ in 0..BENCH_REPEATS {
        let t = Instant::now();
        let stream_result = tbl
            .query()
            .full_text_search(FullTextSearchQuery::new("hello".to_string()))
            .limit(20)
            .execute()
            .await;
        if let Ok(stream) = stream_result {
            let _: Vec<RecordBatch> = stream.try_collect().await?;
        }
        fts_samples.push(t.elapsed());
        let t = Instant::now();
        let stream = tbl
            .query()
            .nearest_to(query_vec.clone())?
            .limit(20)
            .execute()
            .await?;
        let _: Vec<RecordBatch> = stream.try_collect().await?;
        vec_samples.push(t.elapsed());
    }
    finding(
        "axis_09_fts_no_index_error_latency_10k",
        format!(
            "FTS no-index error-return median (3 runs, 10K rows) = {:?} [NOT brute force; index absent = Err path per axis 02]",
            median(fts_samples)
        ),
    );
    finding(
        "axis_09_brute_force_vector_10k",
        format!(
            "vector brute force median (3 runs, 10K rows) = {:?}",
            median(vec_samples)
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 10 ─────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_10_upsert_consistency_via_delete_then_insert() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1, 2, 3]).await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    tbl.optimize(lancedb::table::OptimizeAction::All).await?;
    let before = count_rows(&tbl).await?;
    // Simulate the writer's UPSERT for seed 1: delete-then-insert with the
    // same (collection, path).
    tbl.delete("collection = 'notes' AND path = 'doc-1.md'")
        .await?;
    insert_rows(&tbl, [1]).await?;
    tbl.optimize(lancedb::table::OptimizeAction::All).await?;
    let after = count_rows(&tbl).await?;
    let stream = tbl
        .query()
        .only_if("path = 'doc-1.md'")
        .select(Select::Columns(vec![
            "path".to_string(),
            "body".to_string(),
        ]))
        .execute()
        .await?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    let row_count_for_path: usize = batches.iter().map(|b| b.num_rows()).sum();
    let body_observed: String = batches
        .iter()
        .flat_map(|b| {
            let col: &StringArray = b
                .column_by_name("body")
                .expect("body column requested")
                .as_string();
            (0..col.len())
                .map(|i| col.value(i).to_string())
                .collect::<Vec<_>>()
        })
        .next()
        .unwrap_or_default();
    finding(
        "axis_10_upsert_row_counts",
        format!(
            "rows before = {before}, after delete+insert+optimize = {after}, path 'doc-1.md' row count = {row_count_for_path}, body = {body_observed:?}"
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 11 ─────────────────────────────────────

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_11_fts_with_lindera_ipadic_tokenizer() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, [1, 2, 3]).await?;
    let attempt = tbl
        .create_index(
            &["body"],
            Index::FTS(FtsIndexBuilder::default().base_tokenizer("lindera/ipadic".to_string())),
        )
        .execute()
        .await;
    match attempt {
        Ok(()) => finding(
            "axis_11_lindera_ipadic_create",
            "create_index FTS(lindera/ipadic) succeeded",
        ),
        Err(e) => finding(
            "axis_11_lindera_ipadic_create",
            format!("create_index FTS(lindera/ipadic) FAILED: {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis 12 ─────────────────────────────────────

/// Measure the cost of `IndexBuilder.replace(true)` on indices that
/// already exist, so `maintain_indices` can decide whether the
/// `list_indices()` skip check is worth keeping.
///
/// The 10K-row seed matches axis 08/09 so the K-means input
/// distribution is comparable. The two paths run in separate phases
/// (not interleaved) so the expensive path B re-train does not
/// perturb path A's cache state. Path A measures only the
/// `list_indices()` + skip branch because that is the path
/// `maintain_indices` actually takes when both indices exist. The
/// `create_index` arm of `maintain_indices` is not exercised because
/// the seed step above already guarantees both indices are present —
/// by design the cost we want to measure is the steady-state cost on
/// every `update-all` invocation when nothing has changed.
#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_12_replace_true_cost_at_10k_rows() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, 1..=10_000_u64).await?;
    // Seed the indices once so both paths observe the "indices already
    // present" baseline that `maintain_indices` runs against on every
    // second `update-all` invocation.
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    tbl.create_index(
        &["embedding"],
        Index::IvfFlat(IvfFlatIndexBuilder::default()),
    )
    .execute()
    .await?;

    // Phase A: list_indices + skip check, BENCH_REPEATS in a row so
    // path B's re-trains do not interleave with cache state.
    let mut path_a: Vec<Duration> = Vec::with_capacity(BENCH_REPEATS);
    for _ in 0..BENCH_REPEATS {
        let start = Instant::now();
        let existing = tbl.list_indices().await?;
        let _has_body = existing
            .iter()
            .any(|i| i.columns.iter().any(|c| c == "body"));
        let _has_embedding = existing
            .iter()
            .any(|i| i.columns.iter().any(|c| c == "embedding"));
        path_a.push(start.elapsed());
    }

    // Phase B: replace(true) direct create_index, BENCH_REPEATS in a
    // row. If `replace(true)` short-circuits when nothing changed
    // this is comparable to path A; if it actually re-trains K-means
    // + retokenizes FTS, the cost surfaces here.
    let mut path_b: Vec<Duration> = Vec::with_capacity(BENCH_REPEATS);
    for _ in 0..BENCH_REPEATS {
        let start = Instant::now();
        tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
            .execute()
            .await?;
        tbl.create_index(
            &["embedding"],
            Index::IvfFlat(IvfFlatIndexBuilder::default()),
        )
        .execute()
        .await?;
        path_b.push(start.elapsed());
    }

    let median_a = median(path_a);
    let median_b = median(path_b);
    // `.max(f64::EPSILON)` guards against a sub-microsecond path A
    // median (= empty `list_indices` skip is essentially free) from
    // producing an infinite ratio.
    let ratio = median_b.as_secs_f64() / median_a.as_secs_f64().max(f64::EPSILON);
    finding(
        "axis_12_replace_true_cost",
        format!(
            "path_a (list_indices skip) median = {:?}, \
             path_b (replace(true) direct) median = {:?}, \
             ratio = {:.2}",
            median_a, median_b, ratio
        ),
    );
    Ok(())
}

// ─────────────────────── nullable embedding ────────────────────────────────
//
// Pins the empirical basis for the placeholder-chunk design: a
// `chunks` row with a null `embedding` (a zero-body file's placeholder)
// must (a) not break IVF_Flat index creation and (b) never surface in a
// `nearest_to` vector search. Verified once during the design phase;
// kept here so a lancedb bump that regresses either property is caught.

/// Like [`build_one_row_batch`] but with a null `embedding`, i.e. the
/// placeholder-chunk shape.
fn build_placeholder_row_batch(seed: u64) -> RecordBatch {
    let schema = schema_arc();
    let collection = StringArray::from(vec!["notes"]);
    let path = StringArray::from(vec![format!("placeholder-{seed}.md")]);
    let chunk_sequence = UInt32Array::from(vec![0_u32]);
    let body = StringArray::from(vec![""]);
    let embedding = null_embedding();
    let modified_at = TimestampMicrosecondArray::from(vec![1_700_000_000_000_000_i64])
        .with_timezone("UTC".to_string());
    let source_hash = StringArray::from(vec![format!("{:0>64}", seed)]);
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(collection),
            Arc::new(path),
            Arc::new(chunk_sequence),
            Arc::new(body),
            Arc::new(embedding),
            Arc::new(modified_at),
            Arc::new(source_hash),
        ],
    )
    .unwrap_or_else(|e| panic!("RecordBatch::try_new failed: {e}"))
}

fn null_embedding() -> FixedSizeListArray {
    let values = Float32Builder::with_capacity(VECTOR_DIM as usize);
    let mut builder = FixedSizeListBuilder::new(values, VECTOR_DIM);
    // FixedSizeListBuilder still wants VECTOR_DIM child values before a
    // null `append(false)`; the null bitmap then masks them.
    builder
        .values()
        .append_slice(&vec![0.0_f32; VECTOR_DIM as usize]);
    builder.append(false);
    builder.finish()
}

async fn insert_placeholder_rows(tbl: &Table, seeds: impl IntoIterator<Item = u64>) -> Result<()> {
    let schema = schema_arc();
    let batches: Vec<_> = seeds
        .into_iter()
        .map(|s| Ok(build_placeholder_row_batch(s)))
        .collect();
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(batches, schema));
    tbl.add(reader).execute().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_null_embedding_build_index_and_exclude_from_search() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    // 600 real rows so IVF K-means has enough vectors to train, plus 16
    // null-embedding placeholders mixed in.
    insert_rows(&tbl, 0..600).await?;
    insert_placeholder_rows(&tbl, 0..16).await?;

    let idx = tbl
        .create_index(
            &["embedding"],
            Index::IvfFlat(IvfFlatIndexBuilder::default()),
        )
        .execute()
        .await;
    finding(
        "axis_null_embedding_ivf_build",
        format!(
            "create_index IVF_Flat with 16/616 null embeddings = {:?}",
            idx.as_ref().map(|_| "Ok")
        ),
    );
    idx?;

    let query_vec: Vec<f32> = (0..VECTOR_DIM)
        .map(|i| (1.0_f32 + i as f32).sin())
        .collect();
    let stream = tbl
        .query()
        .nearest_to(query_vec)?
        .limit(700)
        .select(Select::Columns(vec!["path".to_string()]))
        .execute()
        .await?;
    let batches: Vec<RecordBatch> = stream.try_collect().await?;
    let null_hits: usize = batches
        .iter()
        .map(|b| {
            let paths = b
                .column_by_name("path")
                .expect("path column")
                .as_string::<i32>();
            (0..b.num_rows())
                .filter(|&r| paths.value(r).starts_with("placeholder-"))
                .count()
        })
        .sum();
    finding(
        "axis_null_embedding_search_excludes",
        format!("null-embedding rows in nearest_to results = {null_hits} (expect 0)"),
    );
    assert_eq!(
        null_hits, 0,
        "null-embedding placeholder leaked into vector search"
    );
    Ok(())
}

/// Edge of the placeholder design: a table that
/// once held real vectors (IVF_Flat index built) loses all of them — every
/// remaining row is a null-embedding placeholder — and `update-all` then
/// calls `optimize(All)` on the now-vector-less index. The writer's
/// `maintain_indices` guard only blocks *building* a fresh index on 0
/// vectors; it does not skip `optimize` on an existing one, so confirm
/// that path does not panic / error.
#[tokio::test]
#[ignore = "real LanceDB; run via `just lancedb-verify`"]
async fn axis_optimize_existing_index_after_all_vectors_become_null() -> Result<()> {
    let tmp = TempDir::new()?;
    let tbl = fresh_empty_table(&tmp).await?;
    insert_rows(&tbl, 0..600).await?;
    tbl.create_index(
        &["embedding"],
        Index::IvfFlat(IvfFlatIndexBuilder::default()),
    )
    .execute()
    .await?;

    // All real content disappears; only null-embedding placeholders remain.
    insert_placeholder_rows(&tbl, 0..16).await?;
    tbl.delete("path LIKE 'doc-%'").await?;

    let optimized = tbl.optimize(OptimizeAction::All).await;
    finding(
        "axis_optimize_all_nulls",
        format!(
            "optimize(All) on IVF index with 0 non-null vectors = {:?}",
            optimized.as_ref().map(|_| "Ok").map_err(|e| e.to_string())
        ),
    );
    optimized?;
    Ok(())
}
