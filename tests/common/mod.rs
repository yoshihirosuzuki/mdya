//! Shared helpers for integration tests that touch Lance's FTS tokenizer
//! or the chunks table's Arrow `Schema::metadata` pin.
//!
//! Lance reads `<LANCE_LANGUAGE_MODEL_HOME>/lindera/ipadic/config.yml`
//! at `create_index` time. Tests that drive the ingest write
//! path therefore (a) have to point Lance at their own tempdir and
//! (b) have to serialise the process-global env var across parallel
//! test threads. `cargo test` runs tests within one binary in parallel
//! by default, so two tests without the lock would race the env var
//! and observe each other's tempdir paths.
//!
//! Centralising the guard + lock here keeps the three test files that
//! need them (`verify_lindera_ipadic`, `smoke_ingest`,
//! `smoke_update_all_index`) in lock-step — a future tweak to the
//! drop / restore order lands in one place.
//!
//! `#[allow(dead_code)]` is on every item because Rust's integration
//! test harness compiles `tests/common/mod.rs` once per test binary
//! and warns on items the importing binary does not happen to call.

#![allow(dead_code)]

use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{
    Array, FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
    TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::Utc;
use tokio::sync::Mutex;

const LANCE_LANGUAGE_MODEL_HOME_ENV_KEY: &str = "LANCE_LANGUAGE_MODEL_HOME";

/// Serialises every `ScopedLanceLanguageModelHome::set(...)` critical
/// section across the test binary. Callers `.await` the lock first,
/// then construct the guard; the guard's `Drop` impl restores the
/// previous env value before the lock is released.
///
/// `tokio::sync::Mutex` rather than `std::sync::Mutex` because the
/// guard is held across `.await` points (the whole test body needs the
/// env redirect to stay pinned to its own tempdir). `const_new` lets
/// it live in a `static`.
pub static LANCE_ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Scoped redirect of `LANCE_LANGUAGE_MODEL_HOME` for the lifetime of
/// the guard. The previous env value is captured on construction and
/// restored on drop, so tests never bleed env state into one another.
///
/// SAFETY: callers must hold [`LANCE_ENV_LOCK`] for the full lifetime
/// of this guard. `std::env::set_var` is `unsafe` in the Rust 2024
/// edition because env state is process-global; the mutex provides
/// the single-threaded access guarantee the unsafety requires.
pub struct ScopedLanceLanguageModelHome {
    previous: Option<String>,
}

impl ScopedLanceLanguageModelHome {
    pub fn set(value: &Path) -> Self {
        let previous = env::var(LANCE_LANGUAGE_MODEL_HOME_ENV_KEY).ok();
        // SAFETY: justified at the struct level — caller holds LANCE_ENV_LOCK.
        unsafe {
            env::set_var(LANCE_LANGUAGE_MODEL_HOME_ENV_KEY, value);
        }
        Self { previous }
    }
}

impl Drop for ScopedLanceLanguageModelHome {
    fn drop(&mut self) {
        // SAFETY: same justification — caller's LANCE_ENV_LOCK guard
        // outlives this guard, so no other test thread can touch the
        // env var while we restore it.
        unsafe {
            match &self.previous {
                Some(v) => env::set_var(LANCE_LANGUAGE_MODEL_HOME_ENV_KEY, v),
                None => env::remove_var(LANCE_LANGUAGE_MODEL_HOME_ENV_KEY),
            }
        }
    }
}

/// Drop and recreate the `chunks` table with the requested
/// `Schema::metadata` pins. Pass `None` for a key to leave
/// it absent (exercises `SearchError::SchemaMetadataMissing`); pass
/// `Some(...)` with a value different from `config.yml::embedding.model`
/// or the real vector dim to exercise `SchemaMetadataMismatch`.
///
/// Drops every existing row. Used by `tests/smoke_search_vector.rs` to
/// stage the loud-corruption paths without invoking `mdya init` a
/// second time. Callers can re-add synthetic rows via
/// [`add_synthetic_chunks_row`] afterwards.
pub async fn tamper_schema_metadata(
    base: &Path,
    embedding_model: Option<&str>,
    vector_dim_for_metadata: Option<i32>,
) -> Result<()> {
    let index_dir = base.join("index");
    let db = lancedb::connect(index_dir.to_str().expect("UTF-8 path"))
        .execute()
        .await?;
    if db
        .table_names()
        .execute()
        .await?
        .iter()
        .any(|n| n == "chunks")
    {
        // lancedb 0.29: `drop_table` takes a namespace path; the
        // default (root) namespace is the empty slice.
        db.drop_table("chunks", &[]).await?;
    }
    let mut metadata = HashMap::new();
    if let Some(model) = embedding_model {
        metadata.insert("embedding_model".to_string(), model.to_string());
    }
    if let Some(dim) = vector_dim_for_metadata {
        metadata.insert("vector_dim".to_string(), dim.to_string());
    }
    // FixedSizeList width is independent of the `vector_dim` metadata
    // value — keep the structural dim at 256 so the synthetic rows
    // below match the production schema and the test's `MockEmbedder`
    // can still emit 256-wide query vectors.
    let schema = chunks_schema_with_explicit_metadata(256, metadata);
    db.create_empty_table("chunks", Arc::new(schema))
        .execute()
        .await?;
    Ok(())
}

/// Append one synthetic chunks row to the existing table. Used after
/// [`tamper_schema_metadata`] to give vector / FTS search something to
/// match — the row's `body` deliberately reads as plain text so FTS
/// snippets are stable.
pub async fn add_synthetic_chunks_row(base: &Path) -> Result<()> {
    let index_dir = base.join("index");
    let db = lancedb::connect(index_dir.to_str().expect("UTF-8 path"))
        .execute()
        .await?;
    let table = db.open_table("chunks").execute().await?;
    let schema: Arc<Schema> = table.schema().await?;
    let dim = vector_dim_from_schema(&schema);
    let batch = synthetic_chunks_batch(schema.clone(), 1, dim);
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    table.add(reader).execute().await?;
    Ok(())
}

fn vector_dim_from_schema(schema: &Schema) -> usize {
    let field = schema
        .field_with_name("embedding")
        .expect("chunks schema has embedding column");
    match field.data_type() {
        DataType::FixedSizeList(_, dim) => *dim as usize,
        other => panic!("embedding column should be FixedSizeList, got {other:?}"),
    }
}

fn synthetic_chunks_batch(schema: Arc<Schema>, n: usize, dim: usize) -> RecordBatch {
    let collection = StringArray::from(vec!["__corruption_inject__"; n]);
    let path: StringArray = (0..n)
        .map(|i| format!("__inject_{i}.md"))
        .collect::<Vec<_>>()
        .into();
    let chunk_sequence = UInt32Array::from(vec![0_u32; n]);
    let body = StringArray::from(vec!["injected for corruption test"; n]);
    let embedding = synthetic_embedding_array(n, dim);
    let micros = Utc::now().timestamp_micros();
    let modified_at = TimestampMicrosecondArray::from(vec![micros; n]).with_timezone("UTC");
    let source_hash = StringArray::from(vec!["0".repeat(64); n]);
    let columns: Vec<Arc<dyn Array>> = vec![
        Arc::new(collection),
        Arc::new(path),
        Arc::new(chunk_sequence),
        Arc::new(body),
        Arc::new(embedding),
        Arc::new(modified_at),
        Arc::new(source_hash),
    ];
    RecordBatch::try_new(schema, columns).expect("schema matches synthetic columns")
}

fn synthetic_embedding_array(n: usize, dim: usize) -> FixedSizeListArray {
    let values = Float32Builder::with_capacity(n * dim);
    let mut builder = FixedSizeListBuilder::new(values, dim as i32);
    // A zero-vector embedding has undefined cosine similarity and
    // LanceDB returns NaN distance, which scores to 0 after our
    // `cosine_distance_to_score` clamp and effectively hides the row
    // from `nearest_to` results. Use a non-zero constant so synthetic
    // rows are reachable by `MockEmbedder`'s `[0.5; dim]` query vector.
    for _ in 0..n {
        builder.values().append_slice(&vec![0.5_f32; dim]);
        builder.append(true);
    }
    builder.finish()
}

/// Mirror of `mdya::store::chunks_schema` but with caller-supplied
/// metadata (rather than the production `{embedding_model, vector_dim}`).
/// Lets [`tamper_schema_metadata`] write metadata with arbitrary
/// `embedding_model` values or with keys deliberately absent.
fn chunks_schema_with_explicit_metadata(
    vector_dim: i32,
    metadata: HashMap<String, String>,
) -> Schema {
    Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_sequence", DataType::UInt32, false),
        Field::new("body", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                vector_dim,
            ),
            true,
        ),
        Field::new(
            "modified_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("source_hash", DataType::Utf8, false),
    ])
    .with_metadata(metadata)
}
