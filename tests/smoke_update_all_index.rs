//! Integration smokes for the `update-all` end-of-run index maintenance
//! step.
//!
//! `update_all_collections` now (i) writes `<base>/lance-models/lindera/
//! ipadic/config.yml` and (ii) creates the FTS / IVF_Flat indices on the
//! `chunks` table at the end of every run. Each test pins
//! `LANCE_LANGUAGE_MODEL_HOME` to its own tempdir through the shared
//! guard so Lance's FTS tokenizer finds the freshly-written config; the
//! `LANCE_ENV_LOCK` mutex serialises the env-critical section across
//! parallel `cargo test` threads.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use lancedb::index::IndexType;
use tempfile::TempDir;

use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::store::lance_lm::{lance_models_dir, lindera_ipadic_config_path};
use mdya::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;

/// Constant-vector stand-in for the production embedder. The first
/// test in this file needs enough rows for IVF_Flat's K-means
/// training; the vector content does not matter for `list_indices()`
/// shape assertions.
struct MockEmbedder {
    model_id: String,
    dim: usize,
}

impl MockEmbedder {
    fn pinned() -> Self {
        Self {
            model_id: DEFAULT_MODEL_ID.to_string(),
            dim: DEFAULT_VECTOR_DIM,
        }
    }
}

impl Embedder for MockEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| vec![0.1_f32; self.dim]).collect())
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| vec![0.2_f32; self.dim]).collect())
    }
}

async fn fresh_config_dir(tmp: &TempDir) -> Result<PathBuf> {
    let base = tmp.path().to_path_buf();
    let index_dir = base.join("index");
    std::fs::create_dir_all(&index_dir)?;
    let db = mdya::store::connect(&index_dir).await?;
    let schema = chunks_schema(i32::try_from(DEFAULT_VECTOR_DIM)?, DEFAULT_MODEL_ID);
    db.create_empty_table(CHUNKS_TABLE_NAME, Arc::new(schema))
        .execute()
        .await?;
    db.create_empty_table(SOURCES_TABLE_NAME, Arc::new(sources_schema()))
        .execute()
        .await?;
    Ok(base)
}

fn write_md(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
}

fn collections_with(name: &str, root: PathBuf) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();
    map.insert(name.to_string(), root);
    map
}

/// Generate `count` deterministic markdown files under `coll_dir` so
/// IVF_Flat has enough rows for stable K-means training. 50 rows is
/// well below `num_partitions * sample_rate` (`sqrt(50) * 256 ≈ 1810`),
/// so Lance's auto-fallback to "train on the entire dataset" kicks in
/// when train data is insufficient.
fn seed_md_files(coll_dir: &Path, count: usize) {
    for i in 0..count {
        write_md(
            coll_dir,
            &format!("doc-{i:03}.md"),
            &format!("# Doc {i}\n\nbody {i}.\n"),
        );
    }
}

#[tokio::test]
async fn update_all_creates_fts_and_vector_indices_after_first_ingest() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    seed_md_files(&coll_dir, 50);

    update_all_collections(
        &collections_with("notes", coll_dir.clone()),
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;

    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(CHUNKS_TABLE_NAME).execute().await?;
    let indices = table.list_indices().await?;
    let has_body_fts = indices.iter().any(|idx| {
        matches!(idx.index_type, IndexType::FTS) && idx.columns.iter().any(|c| c == "body")
    });
    let has_ivf = indices.iter().any(|idx| {
        matches!(idx.index_type, IndexType::IvfFlat) && idx.columns.iter().any(|c| c == "embedding")
    });
    assert!(has_body_fts, "FTS index on `body` missing: {indices:?}");
    assert!(has_ivf, "IvfFlat index on `embedding` missing: {indices:?}");
    Ok(())
}

#[tokio::test]
async fn update_all_is_idempotent_for_indices() -> Result<()> {
    // Second run must not error on `create_index` even though the
    // indices already exist — `replace(false)` would, but `maintain_indices`
    // skips create when `list_indices()` already covers the column.
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    seed_md_files(&coll_dir, 50);

    let collections = collections_with("notes", coll_dir.clone());
    update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(CHUNKS_TABLE_NAME).execute().await?;
    let after_first = table.list_indices().await?.len();

    update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;
    let after_second = table.list_indices().await?.len();
    assert_eq!(
        after_first, after_second,
        "indices count must be stable across runs"
    );
    assert!(
        after_second >= 2,
        "expected FTS (body) + IvfFlat, got {after_second}"
    );
    Ok(())
}

#[tokio::test]
async fn update_all_with_zero_collections_does_not_attempt_index_creation() -> Result<()> {
    // IVF_Flat fails on an empty table. The short-circuit in
    // `maintain_indices` must catch the "no rows ever ingested" case
    // so `update_all_collections` does not crash when every
    // collection has been removed from config.yml.
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let empty_collections: BTreeMap<String, PathBuf> = BTreeMap::new();

    update_all_collections(
        &empty_collections,
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;

    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(CHUNKS_TABLE_NAME).execute().await?;
    let indices = table.list_indices().await?;
    assert!(
        indices.is_empty(),
        "zero-row table must not have triggered create_index, got {indices:?}"
    );
    Ok(())
}

#[tokio::test]
async fn update_all_writes_lindera_ipadic_config_yml() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    write_md(&coll_dir, "a.md", "# A\n\nbody.\n");

    update_all_collections(
        &collections_with("notes", coll_dir.clone()),
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;

    let config_path = lindera_ipadic_config_path(&base);
    assert!(
        config_path.is_file(),
        "lindera/ipadic config.yml missing at {}",
        config_path.display()
    );
    let yaml = std::fs::read_to_string(&config_path)?;
    assert!(
        yaml.contains("kind: ipadic"),
        "config.yml must select the embedded dictionary, got:\n{yaml}"
    );
    Ok(())
}

#[tokio::test]
async fn update_all_does_not_rewrite_existing_lindera_ipadic_config() -> Result<()> {
    // Byte-equal idempotency at the `update_all` level — second run
    // must leave the config.yml mtime untouched so file watchers /
    // incremental builds do not re-fire on every ingest.
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    write_md(&coll_dir, "a.md", "# A\n\nbody.\n");

    let collections = collections_with("notes", coll_dir.clone());
    update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;
    let config_path = lindera_ipadic_config_path(&base);
    let first_mtime = std::fs::metadata(&config_path)?.modified()?;

    // Cross the 1 s filesystem-mtime boundary so HFS+ would have to
    // bump mtime if we touched the file.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder::pinned()),
        Arc::new(NullProgress),
        0,
    )
    .await?;
    let second_mtime = std::fs::metadata(&config_path)?.modified()?;
    assert_eq!(
        first_mtime, second_mtime,
        "byte-equal path must preserve mtime"
    );
    Ok(())
}
