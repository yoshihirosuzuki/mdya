//! Smoke for `mdya collection list` / `mdya status`. The library
//! layer (`introspect::collection_list` / `introspect::status`) is driven
//! against a real LanceDB tempdir ingested with the mock embedder, so
//! per-collection document counts and the schema-pin status are exercised
//! without downloading the model. The binary layer is driven through the
//! built `mdya` binary to prove the `--format` plumbing, `--description`
//! flag, and JSON shape that the MCP tools will mirror.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use tempfile::TempDir;

use mdya::config::{self, CollectionEntry, Config};
use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::introspect;
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;

struct MockEmbedder;

impl Embedder for MockEmbedder {
    fn model_id(&self) -> &str {
        DEFAULT_MODEL_ID
    }
    fn dim(&self) -> usize {
        DEFAULT_VECTOR_DIM
    }
    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|_| vec![0.1_f32; DEFAULT_VECTOR_DIM])
            .collect())
    }
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts
            .iter()
            .map(|_| vec![0.2_f32; DEFAULT_VECTOR_DIM])
            .collect())
    }
}

/// Write `config.yml` with two collections (`notes` carries a description,
/// `work` does not) and create the empty `chunks` + `sources` tables.
async fn setup(tmp: &TempDir) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let base = tmp.path().to_path_buf();
    let notes_dir = base.join("notes");
    let work_dir = base.join("work");
    std::fs::create_dir_all(&notes_dir)?;
    std::fs::create_dir_all(&work_dir)?;

    let mut cfg = Config::init_template();
    cfg.collections.insert(
        "notes".to_string(),
        CollectionEntry {
            path: notes_dir.to_string_lossy().into_owned(),
            description: Some("個人メモ".to_string()),
        },
    );
    cfg.collections.insert(
        "work".to_string(),
        CollectionEntry {
            path: work_dir.to_string_lossy().into_owned(),
            description: None,
        },
    );
    config::save(&base.join("config.yml"), &cfg)?;

    let index_dir = base.join("index");
    std::fs::create_dir_all(&index_dir)?;
    let db = mdya::store::connect(&index_dir).await?;
    db.create_empty_table(
        CHUNKS_TABLE_NAME,
        Arc::new(chunks_schema(DEFAULT_VECTOR_DIM as i32, DEFAULT_MODEL_ID)),
    )
    .execute()
    .await?;
    db.create_empty_table(SOURCES_TABLE_NAME, Arc::new(sources_schema()))
        .execute()
        .await?;
    Ok((base, notes_dir, work_dir))
}

async fn ingest(base: &Path, dirs: &[(&str, &Path)]) -> Result<()> {
    let mut collections = BTreeMap::new();
    for (name, dir) in dirs {
        collections.insert((*name).to_string(), dir.to_path_buf());
    }
    update_all_collections(
        &collections,
        base,
        Arc::new(MockEmbedder),
        Arc::new(NullProgress),
        0,
    )
    .await?;
    Ok(())
}

fn write_md(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write md");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_list_reports_per_collection_counts_and_descriptions() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, notes_dir, work_dir) = setup(&tmp).await?;
    write_md(&notes_dir, "a.md", "# A\n\nalpha.\n");
    write_md(&notes_dir, "b.md", "# B\n\nbeta.\n");
    write_md(&work_dir, "c.md", "# C\n\ngamma.\n");
    ingest(&base, &[("notes", &notes_dir), ("work", &work_dir)]).await?;

    let report = introspect::collection_list(Some(&base)).await?;
    let notes = report
        .collections
        .iter()
        .find(|c| c.name == "notes")
        .expect("notes present");
    assert_eq!(notes.document_count, 2);
    assert_eq!(notes.description.as_deref(), Some("個人メモ"));

    let work = report
        .collections
        .iter()
        .find(|c| c.name == "work")
        .expect("work present");
    assert_eq!(work.document_count, 1);
    assert_eq!(work.description, None);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_schema_pins_and_row_counts() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, notes_dir, work_dir) = setup(&tmp).await?;
    write_md(&notes_dir, "a.md", "# A\n\nalpha.\n");
    write_md(&work_dir, "c.md", "# C\n\ngamma.\n");
    ingest(&base, &[("notes", &notes_dir), ("work", &work_dir)]).await?;

    let report = introspect::status(Some(&base)).await?;
    assert_eq!(report.embedding_model, DEFAULT_MODEL_ID);
    assert_eq!(report.vector_dim, DEFAULT_VECTOR_DIM as u32);
    assert_eq!(report.collections, 2);
    assert_eq!(report.sources, 2);
    assert!(
        report.chunks >= 2,
        "expected >=2 chunks, got {}",
        report.chunks
    );
    Ok(())
}

/// Through the real binary: `init` → `collection add --description` →
/// `collection list --format json`. Stays download-free (no ingest, so the
/// `sources` table is empty and document_count is 0).
#[test]
fn cli_collection_list_json_surfaces_description() -> Result<()> {
    let tmp = TempDir::new()?;
    let config_dir = tmp.path().to_str().expect("utf8");
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;

    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", config_dir, "init"])
        .assert()
        .success();
    CliCommand::cargo_bin("mdya")?
        .args([
            "--config-dir",
            config_dir,
            "collection",
            "add",
            coll_dir.to_str().expect("utf8"),
            "--description",
            "個人メモ",
        ])
        .assert()
        .success();

    let output = CliCommand::cargo_bin("mdya")?
        .args([
            "--config-dir",
            config_dir,
            "collection",
            "list",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    let collections = value["collections"].as_array().expect("collections array");
    assert_eq!(collections.len(), 1, "exactly one collection was added");
    let only = &collections[0];
    assert_eq!(only["name"], "notes");
    assert_eq!(only["description"], "個人メモ");
    assert_eq!(only["document_count"], 0);
    Ok(())
}

/// Through the real binary: `mdya status --format json` on a fresh index
/// reports the init-written pin and zero counts.
#[test]
fn cli_status_json_on_fresh_index() -> Result<()> {
    let tmp = TempDir::new()?;
    let config_dir = tmp.path().to_str().expect("utf8");

    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", config_dir, "init"])
        .assert()
        .success();

    let output = CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", config_dir, "status", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output)?;
    assert!(
        value["version"].as_str().is_some_and(|v| !v.is_empty()),
        "version should be a non-empty string, got {}",
        value["version"]
    );
    assert_eq!(value["embedding_model"], DEFAULT_MODEL_ID);
    assert_eq!(value["vector_dim"], DEFAULT_VECTOR_DIM);
    assert_eq!(value["collections"], 0);
    assert_eq!(value["chunks"], 0);
    assert_eq!(value["sources"], 0);
    Ok(())
}
