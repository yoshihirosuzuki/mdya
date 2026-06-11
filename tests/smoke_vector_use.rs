//! Integration smoke for `mdya vector use <model>`.
//!
//! Drives the destructive switch core (`cli::vector::switch_model`) with
//! a stand-in `Embedder` so the test never downloads a real model. The
//! switch changes both the model id and the vector dim (256 -> 128) to
//! prove the uniform drop + recreate + re-embed path:
//!
//! 1. `config.yml::embedding.model` is rewritten to the new model.
//! 2. The `chunks` table is recreated with the new `Schema::metadata`
//!    pin and the new `FixedSizeList` width.
//! 3. Every collection file is re-embedded into the fresh table, and the
//!    FTS / IVF_Flat indices are rebuilt.
//!
//! Like the other ingest smokes it pins `LANCE_LANGUAGE_MODEL_HOME` to
//! its own tempdir through the shared guard (Lance reads the FTS
//! tokenizer config from there at `create_index` time) and serialises
//! that env-critical section with `LANCE_ENV_LOCK`.

mod common;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arrow_schema::DataType;
use lancedb::index::IndexType;
use tempfile::TempDir;

use mdya::cli::vector::switch_model;
use mdya::config::{self, CollectionEntry, Config};
use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::NullProgress;
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const OLD_MODEL: &str = "cl-nagoya/ruri-v3-30m";
const OLD_DIM: usize = 256;
const NEW_MODEL: &str = "fake-model-b";
const NEW_DIM: usize = 128;

/// Constant-vector stand-in for the production embedder, parameterised on
/// model id + dim so the test can switch to a different-dimension model.
struct MockEmbedder {
    model_id: String,
    dim: usize,
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

/// Materialize `~/.mdya/` with a `config.yml` pinning `OLD_MODEL`, a
/// `notes` collection, and the `chunks` (old dim) + `sources` tables —
/// the state a user reaches after `mdya init` + `mdya update-all`.
async fn seed_config_dir(tmp: &TempDir, coll_dir: &Path) -> Result<std::path::PathBuf> {
    let base = tmp.path().to_path_buf();
    let index_dir = base.join("index");
    std::fs::create_dir_all(&index_dir)?;

    let mut cfg = Config::init_template();
    cfg.embedding.model = OLD_MODEL.to_string();
    cfg.collections.insert(
        "notes".to_string(),
        CollectionEntry {
            path: coll_dir.to_string_lossy().into_owned(),
            description: None,
        },
    );
    config::save(&base.join("config.yml"), &cfg)?;

    let db = mdya::store::connect(&index_dir).await?;
    let schema = chunks_schema(i32::try_from(OLD_DIM)?, OLD_MODEL);
    db.create_empty_table(CHUNKS_TABLE_NAME, Arc::new(schema))
        .execute()
        .await?;
    db.create_empty_table(SOURCES_TABLE_NAME, Arc::new(sources_schema()))
        .execute()
        .await?;
    Ok(base)
}

/// Seed `count` tiny markdown files so the re-embed produces enough
/// non-null vectors for IVF_Flat's K-means auto-fallback to train
/// (mirrors `smoke_update_all_index.rs`).
fn seed_md_files(coll_dir: &Path, count: usize) {
    for i in 0..count {
        let path = coll_dir.join(format!("doc-{i:03}.md"));
        std::fs::write(&path, format!("# Doc {i}\n\nbody {i}.\n")).expect("write md");
    }
}

#[tokio::test]
async fn vector_use_switches_model_recreates_table_and_re_embeds() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    seed_md_files(&coll_dir, 50);
    let base = seed_config_dir(&tmp, &coll_dir).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));

    let cfg = config::load(&base.join("config.yml"))?;
    let embedder = Arc::new(MockEmbedder {
        model_id: NEW_MODEL.to_string(),
        dim: NEW_DIM,
    });
    // The smoke test only re-embeds two tiny files; the sequential path
    // exercises the same code paths without spawning extra task workers.
    let summary = switch_model(&base, cfg, embedder, Arc::new(NullProgress), 0).await?;
    assert_eq!(summary.failed, 0, "no file should fail to re-embed");

    // 1. config.yml now pins the new model.
    let cfg = config::load(&base.join("config.yml"))?;
    assert_eq!(cfg.embedding.model, NEW_MODEL);

    // 2. chunks table recreated with the new metadata pin + dim.
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(CHUNKS_TABLE_NAME).execute().await?;
    let schema = table.schema().await?;
    assert_eq!(
        schema.metadata().get("embedding_model").map(String::as_str),
        Some(NEW_MODEL),
        "embedding_model pin must be the new model"
    );
    assert_eq!(
        schema.metadata().get("vector_dim").map(String::as_str),
        Some(NEW_DIM.to_string().as_str()),
        "vector_dim pin must be the new dim"
    );
    let embedding = schema
        .field_with_name("embedding")
        .expect("embedding column exists");
    match embedding.data_type() {
        DataType::FixedSizeList(_, width) => assert_eq!(
            *width, NEW_DIM as i32,
            "embedding column width must follow the new dim"
        ),
        other => panic!("embedding must be FixedSizeList, got {other:?}"),
    }

    // 3. every file was re-embedded into the fresh table, and indices rebuilt.
    // Each of the 50 seeded files yields at least one chunk, so the row
    // count is >= 50 (a single small section is one chunk; the bound stays
    // robust if chunking ever splits a file).
    let rows = table.count_rows(None).await?;
    assert!(rows >= 50, "expected >=50 re-embedded chunks, got {rows}");
    let indices = table.list_indices().await?;
    let has_body_fts = indices.iter().any(|idx| {
        matches!(idx.index_type, IndexType::FTS) && idx.columns.iter().any(|c| c == "body")
    });
    let has_ivf = indices.iter().any(|idx| {
        matches!(idx.index_type, IndexType::IvfFlat) && idx.columns.iter().any(|c| c == "embedding")
    });
    assert!(
        has_body_fts,
        "FTS index on `body` missing after switch: {indices:?}"
    );
    assert!(
        has_ivf,
        "IvfFlat index on `embedding` missing after switch: {indices:?}"
    );
    Ok(())
}
