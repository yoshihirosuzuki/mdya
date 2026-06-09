//! Empirical verification of Arrow Schema metadata behaviour under
//! LanceDB / Lance. These axes back the schema metadata pin design
//! (used instead of an `embedding_model` column).
//!
//! Each test isolates one behavioural axis of the `lancedb = "=0.29.0"`
//! API around `Schema::with_metadata`, `Field::with_metadata`, and
//! `FixedSizeList<Float32, N>` dim enforcement. Findings are printed via
//! `println!` so they end up in the `--nocapture` output.
//!
//! Tests are gated behind `#[ignore]` and run via
//! `just schema-metadata-verify`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
use arrow_array::{
    FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use tempfile::TempDir;

const VECTOR_DIM: i32 = 256;
const WRONG_DIM: i32 = 128;
const TABLE_NAME: &str = "verify_chunks";
const MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";

fn finding(name: &str, msg: impl std::fmt::Display) {
    println!("[metadata finding] {name}: {msg}");
}

fn schema_with_metadata(dim: i32, kv: &[(&str, &str)]) -> Schema {
    let mut meta = HashMap::new();
    for (k, v) in kv {
        meta.insert((*k).to_string(), (*v).to_string());
    }
    Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            false,
        ),
    ])
    .with_metadata(meta)
}

fn schema_with_field_metadata(dim: i32, field_meta: &[(&str, &str)]) -> Schema {
    let mut meta = HashMap::new();
    for (k, v) in field_meta {
        meta.insert((*k).to_string(), (*v).to_string());
    }
    Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            false,
        )
        .with_metadata(meta),
    ])
}

fn build_row_batch(schema: Arc<Schema>, dim: i32, seed: u64) -> RecordBatch {
    let collection = StringArray::from(vec!["notes"]);
    let embedding = deterministic_embedding(dim, seed);
    RecordBatch::try_new(schema, vec![Arc::new(collection), Arc::new(embedding)])
        .expect("RecordBatch::try_new (verify-only fixture)")
}

fn deterministic_embedding(dim: i32, seed: u64) -> FixedSizeListArray {
    let values = Float32Builder::with_capacity(dim as usize);
    let mut builder = FixedSizeListBuilder::new(values, dim);
    let v: Vec<f32> = (0..dim)
        .map(|i| ((seed as f32) + (i as f32)).sin())
        .collect();
    builder.values().append_slice(&v);
    builder.append(true);
    builder.finish()
}

async fn fresh_db(tmp: &TempDir) -> Result<Connection> {
    let path = tmp.path().join("index");
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_str().context("UTF-8 tempdir path")?;
    Ok(lancedb::connect(path_str).execute().await?)
}

fn reader_for(batch: RecordBatch, schema: Arc<Schema>) -> Box<dyn RecordBatchReader + Send> {
    Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema))
}

// ───────────────────────────── axis 1 ──────────────────────────────────────

/// `Schema::with_metadata({k:v})` で create_empty_table → drop → 同 path で
/// 再 connect + open_table すると、`Table::schema()` の metadata に同じ k/v
/// が読めることを確認する。本 PR の design 全体の前提。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_01_schema_metadata_preserved_across_create_and_reopen() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_metadata(
        VECTOR_DIM,
        &[("embedding_model", MODEL_ID), ("vector_dim", "256")],
    ));
    {
        let db = fresh_db(&tmp).await?;
        db.create_empty_table(TABLE_NAME, schema.clone())
            .execute()
            .await?;
    }
    let db = fresh_db(&tmp).await?;
    let tbl = db.open_table(TABLE_NAME).execute().await?;
    let read_back = tbl.schema().await?;
    let meta = read_back.metadata();
    finding(
        "axis_01_metadata_after_reopen",
        format!(
            "embedding_model = {:?}, vector_dim = {:?}, full = {:?}",
            meta.get("embedding_model"),
            meta.get("vector_dim"),
            meta
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 2 ──────────────────────────────────────

/// `add` で row insert した後、schema metadata が同 process / 再 open
/// 双方で保持されることを確認する。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_02_schema_metadata_preserved_after_add() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_metadata(
        VECTOR_DIM,
        &[("embedding_model", MODEL_ID), ("vector_dim", "256")],
    ));
    let db = fresh_db(&tmp).await?;
    let tbl = db
        .create_empty_table(TABLE_NAME, schema.clone())
        .execute()
        .await?;
    let batch = build_row_batch(schema.clone(), VECTOR_DIM, 1);
    tbl.add(reader_for(batch, schema.clone())).execute().await?;
    let read_back = tbl.schema().await?;
    let meta = read_back.metadata();
    finding(
        "axis_02_metadata_after_add",
        format!(
            "embedding_model = {:?}, vector_dim = {:?}",
            meta.get("embedding_model"),
            meta.get("vector_dim")
        ),
    );
    // Re-open path to be sure metadata also survives a reload.
    drop(tbl);
    drop(db);
    let db2 = fresh_db(&tmp).await?;
    let tbl2 = db2.open_table(TABLE_NAME).execute().await?;
    let reread = tbl2.schema().await?;
    finding(
        "axis_02_metadata_after_add_then_reopen",
        format!(
            "embedding_model = {:?}, vector_dim = {:?}",
            reread.metadata().get("embedding_model"),
            reread.metadata().get("vector_dim")
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 3 ──────────────────────────────────────

/// Verify that schema metadata is preserved after `merge_insert`.
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_03_schema_metadata_preserved_after_merge_insert() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_metadata(
        VECTOR_DIM,
        &[("embedding_model", MODEL_ID), ("vector_dim", "256")],
    ));
    let db = fresh_db(&tmp).await?;
    let tbl = db
        .create_empty_table(TABLE_NAME, schema.clone())
        .execute()
        .await?;
    let batch1 = build_row_batch(schema.clone(), VECTOR_DIM, 1);
    tbl.add(reader_for(batch1, schema.clone()))
        .execute()
        .await?;
    let batch2 = build_row_batch(schema.clone(), VECTOR_DIM, 1);
    let mut builder = tbl.merge_insert(&["collection"]);
    builder
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    builder.execute(reader_for(batch2, schema.clone())).await?;
    let read_back = tbl.schema().await?;
    finding(
        "axis_03_metadata_after_merge_insert",
        format!(
            "embedding_model = {:?}, vector_dim = {:?}",
            read_back.metadata().get("embedding_model"),
            read_back.metadata().get("vector_dim")
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 4 ──────────────────────────────────────

/// `FixedSizeList<Float32, 256>` の table に dim ≠ 256 vector を含む
/// batch を `add` したときの error 種類を確定する。declared/actual の
/// schema metadata 比較で先に弾く設計のため到達しない code path だが、
/// fail-safe 動作を literal verify する。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_04_dim_mismatch_at_insert_returns_error() -> Result<()> {
    let tmp = TempDir::new()?;
    let table_schema = Arc::new(schema_with_metadata(VECTOR_DIM, &[]));
    let db = fresh_db(&tmp).await?;
    let tbl = db
        .create_empty_table(TABLE_NAME, table_schema.clone())
        .execute()
        .await?;
    let wrong_schema = Arc::new(schema_with_metadata(WRONG_DIM, &[]));
    let batch = build_row_batch(wrong_schema.clone(), WRONG_DIM, 1);
    let result = tbl
        .add(reader_for(batch, wrong_schema.clone()))
        .execute()
        .await;
    finding(
        "axis_04_dim_mismatch_at_insert",
        format!(
            "add with FixedSizeList<{}> into table FixedSizeList<{}>: {:?}",
            WRONG_DIM,
            VECTOR_DIM,
            result.err().map(|e| format!("{e}"))
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 5 ──────────────────────────────────────

/// `nearest_to` に dim ≠ 256 query vector を渡したときの error 種類を
/// 確定する。declared/actual 比較で SearchEngine 入口で弾く前提だが、
/// 構造検知が effective に動くことを literal verify する。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_05_dim_mismatch_at_search_returns_error() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_metadata(VECTOR_DIM, &[]));
    let db = fresh_db(&tmp).await?;
    let tbl = db
        .create_empty_table(TABLE_NAME, schema.clone())
        .execute()
        .await?;
    let batch = build_row_batch(schema.clone(), VECTOR_DIM, 1);
    tbl.add(reader_for(batch, schema.clone())).execute().await?;
    let wrong_query: Vec<f32> = (0..WRONG_DIM).map(|i| (i as f32).sin()).collect();
    let nearest_attempt = tbl.query().nearest_to(wrong_query);
    match nearest_attempt {
        Ok(builder) => {
            let exec = builder.limit(5).execute().await;
            match exec {
                Ok(stream) => {
                    let batches: Result<Vec<RecordBatch>, _> = stream.try_collect().await;
                    finding(
                        "axis_05_dim_mismatch_at_search_executed",
                        format!(
                            "nearest_to executed; collect = {:?}",
                            batches.map(|b| b.iter().map(|r| r.num_rows()).sum::<usize>())
                        ),
                    );
                }
                Err(e) => finding(
                    "axis_05_dim_mismatch_at_search_execute_err",
                    format!("nearest_to(...).execute() = {e}"),
                ),
            }
        }
        Err(e) => finding(
            "axis_05_dim_mismatch_at_search_builder_err",
            format!("nearest_to() builder error = {e}"),
        ),
    }
    Ok(())
}

// ───────────────────────────── axis 6 ──────────────────────────────────────

/// `Field::metadata` (列ごとの metadata) も Schema metadata と同じく
/// 透過保持されるかを literal verify する。本 PR は schema-level
/// 統一を採用しているが、将来の field-level 採用に備えて挙動を凍結。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_06_field_metadata_preserved() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_field_metadata(
        VECTOR_DIM,
        &[("embedding_model", MODEL_ID)],
    ));
    {
        let db = fresh_db(&tmp).await?;
        db.create_empty_table(TABLE_NAME, schema.clone())
            .execute()
            .await?;
    }
    let db = fresh_db(&tmp).await?;
    let tbl = db.open_table(TABLE_NAME).execute().await?;
    let read_back = tbl.schema().await?;
    let embedding_field = read_back
        .field_with_name("embedding")
        .context("embedding field must exist in read-back schema")?;
    finding(
        "axis_06_field_metadata_after_reopen",
        format!(
            "embedding.metadata().get(\"embedding_model\") = {:?}, full = {:?}",
            embedding_field.metadata().get("embedding_model"),
            embedding_field.metadata()
        ),
    );
    Ok(())
}

// ───────────────────────────── axis 7 ──────────────────────────────────────

/// `HashMap::get(&key)` の lookup integrity が reopen 後も保たれること
/// を確認する。Rust HashMap は順序を保証しない仕様のため、本 PR の
/// 設計 (= `metadata.get("embedding_model")` ベースの比較) が順序非依存
/// であることを literal verify する。
#[tokio::test]
#[ignore = "real LanceDB; run via `just schema-metadata-verify`"]
async fn axis_07_metadata_key_lookup_after_reopen() -> Result<()> {
    let tmp = TempDir::new()?;
    let schema = Arc::new(schema_with_metadata(
        VECTOR_DIM,
        &[
            ("zzz_last_alphabetically", "z"),
            ("aaa_first_alphabetically", "a"),
            ("embedding_model", MODEL_ID),
            ("vector_dim", "256"),
        ],
    ));
    {
        let db = fresh_db(&tmp).await?;
        db.create_empty_table(TABLE_NAME, schema.clone())
            .execute()
            .await?;
    }
    let db = fresh_db(&tmp).await?;
    let tbl = db.open_table(TABLE_NAME).execute().await?;
    let read_back = tbl.schema().await?;
    let meta = read_back.metadata();
    let observed_order: Vec<&String> = meta.keys().collect();
    finding(
        "axis_07_key_lookup",
        format!(
            "get(\"embedding_model\") = {:?}, get(\"vector_dim\") = {:?}, get(\"aaa_first_alphabetically\") = {:?}, get(\"zzz_last_alphabetically\") = {:?}, get(\"absent\") = {:?}, observed key iteration order (HashMap, no guarantee) = {:?}",
            meta.get("embedding_model"),
            meta.get("vector_dim"),
            meta.get("aaa_first_alphabetically"),
            meta.get("zzz_last_alphabetically"),
            meta.get("absent"),
            observed_order
        ),
    );
    Ok(())
}
