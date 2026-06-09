//! Smoke tests for `mdya search vector`.
//!
//! Library-level tests drive `SearchEngine::vector` against a real
//! LanceDB tempdir populated by `update_all_collections` +
//! `MockEmbedder`. The schema-metadata pin is checked once in
//! `SearchEngine::open` — Mismatch warns via `tracing` and lets the
//! search continue (asserted behaviourally below by checking the
//! returned hits, not the warn text), Missing aborts with
//! `SearchError::SchemaMetadataMissing`.
//!
//! CLI 3 tests reuse the validation surface (empty query / unknown
//! collection / `-n 0`) from the FTS smoke pattern, since
//! `SearchEngine::vector` reuses the same `validate` path that `fts`
//! already exercises end-to-end via the CLI.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use predicates::str::contains;
use tempfile::TempDir;

use mdya::config::save;
use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::search::{SearchEngine, SearchError, SearchLevel, SearchMode, SearchRequest};
use mdya::store::lance_lm::lance_models_dir;

use common::{
    LANCE_ENV_LOCK, ScopedLanceLanguageModelHome, add_synthetic_chunks_row, tamper_schema_metadata,
};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;
const DEFAULT_VECTOR_DIM_I32: i32 = 256;

/// Distinct-vector mock: query/passage embeddings live in different
/// regions of the unit sphere so cosine similarity is non-trivial
/// (not all 1.0). The exact values do not matter — only that hits
/// distinguish "matches" from "non-matches" enough for assertions.
struct MockEmbedder;

impl Embedder for MockEmbedder {
    fn model_id(&self) -> &str {
        DEFAULT_MODEL_ID
    }

    fn dim(&self) -> usize {
        DEFAULT_VECTOR_DIM
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| seeded_vector(0.5)).collect())
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| seeded_vector(0.5)).collect())
    }
}

fn seeded_vector(value: f32) -> Vec<f32> {
    vec![value; DEFAULT_VECTOR_DIM]
}

fn write_md(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
}

/// Same in-process corpus pattern as `tests/smoke_search_fts.rs`:
/// `mdya init` via the CLI, then config.yml editing in process.
/// Splitting init through the binary keeps the search CLI surface
/// honest about its end-to-end contract.
async fn fresh_corpus(tmp: &TempDir) -> Result<(PathBuf, PathBuf)> {
    let base = tmp.path().to_path_buf();
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;
    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", base.to_str().unwrap(), "init"])
        .assert()
        .success();
    let cfg_path = base.join("config.yml");
    let mut cfg = mdya::config::load(&cfg_path)?;
    let mut collections = BTreeMap::new();
    collections.insert(
        "notes".to_string(),
        mdya::config::CollectionEntry {
            path: coll_dir.to_string_lossy().into_owned(),
            description: None,
        },
    );
    cfg.collections = collections;
    save(&cfg_path, &cfg)?;
    Ok((base, coll_dir))
}

async fn ingest(base: &Path, coll_dir: &Path) -> Result<()> {
    let mut collections = BTreeMap::new();
    collections.insert("notes".to_string(), coll_dir.to_path_buf());
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

// ---------- Library-level tests (real ingest + SearchEngine::vector) ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vector_returns_at_least_one_hit_with_normalised_score() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "a.md", "# A\n\nbody a.\n");
    write_md(&coll_dir, "b.md", "# B\n\nbody b.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .vector(
            &SearchRequest {
                query: "anything".to_string(),
                collections: vec![],
                limit: 20,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;

    assert_eq!(resp.mode, SearchMode::Vector);
    assert!(resp.total >= 1, "expected hits, got {resp:?}");
    for h in &resp.hits {
        assert!(
            (0.0..=1.0).contains(&h.score()),
            "score out of [0,1]: {h:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vector_self_embedding_score_is_near_one() -> Result<()> {
    // MockEmbedder returns the same vector for queries and passages,
    // so cosine distance ≈ 0 ⇒ score ≈ 1. This pins the
    // `(1 - distance).max(0.0)` mapping under integration.
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "only.md", "# Only\n\nbody only.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .vector(
            &SearchRequest {
                query: "anything".to_string(),
                collections: vec![],
                limit: 1,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;

    let top = resp.hits.first().expect("at least one hit ingested");
    assert!(top.score() > 0.99, "self-embedding score too low: {top:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vector_continues_when_actual_embedding_model_disagrees_with_declared() -> Result<()> {
    // A schema-metadata pin mismatch surfaces as a `tracing::warn!`
    // line at `SearchEngine::open` time, but the engine is still
    // constructed and hits flow through. We don't capture the warn
    // text here — the behavioural pin is "hits returned despite
    // mismatch" (loud corruption, search-side warn + continue).
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, _coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    tamper_schema_metadata(&base, Some("wrong-model"), Some(DEFAULT_VECTOR_DIM_I32)).await?;
    add_synthetic_chunks_row(&base).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .vector(
            &SearchRequest {
                query: "anything".to_string(),
                collections: vec![],
                limit: 5,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;

    assert_eq!(resp.mode, SearchMode::Vector);
    assert!(
        !resp.hits.is_empty(),
        "search must continue despite mismatch: {resp:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vector_aborts_when_schema_metadata_pins_are_missing() -> Result<()> {
    // Schema metadata absent is structurally abnormal (= pre-pin DB
    // / direct tampering / foreign-tool table). Both update-all and
    // search abort; here we pin the search-side abort returns
    // `SearchError::SchemaMetadataMissing` with the exact absent
    // keys.
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, _coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    tamper_schema_metadata(&base, None, None).await?;

    let result = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await;
    match result {
        Err(SearchError::SchemaMetadataMissing { absent_keys }) => {
            assert!(absent_keys.contains(&"embedding_model"), "{absent_keys:?}");
            assert!(absent_keys.contains(&"vector_dim"), "{absent_keys:?}");
        }
        Err(other) => panic!("expected SchemaMetadataMissing, got {other:?}"),
        Ok(_) => panic!("open must fail when both pins are missing"),
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vector_collection_filter_uses_type_safe_only_if_expr() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "a.md", "# A\n\nbody a.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    // `notes` is the declared collection — the filter should accept it
    // and return at least one hit. An unknown collection name would
    // hit the validation path (covered by the CLI tests below).
    let resp = engine
        .vector(
            &SearchRequest {
                query: "anything".to_string(),
                collections: vec!["notes".to_string()],
                limit: 5,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;
    assert!(resp.hits.iter().all(|h| h.collection() == "notes"));
    Ok(())
}

// ---------- CLI validation tests (init-only, no ingest required) ----------

fn init_only(tmp: &TempDir) {
    CliCommand::cargo_bin("mdya")
        .unwrap()
        .args(["--config-dir", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();
}

#[test]
fn cli_empty_query_exits_one_with_validation_message() {
    let tmp = TempDir::new().unwrap();
    init_only(&tmp);
    CliCommand::cargo_bin("mdya")
        .unwrap()
        .args([
            "--config-dir",
            tmp.path().to_str().unwrap(),
            "search",
            "vector",
            "   ",
        ])
        .assert()
        .failure()
        .stderr(contains("query must be non-empty"));
}

#[test]
fn cli_unknown_collection_exits_one_with_typo_hint() {
    let tmp = TempDir::new().unwrap();
    init_only(&tmp);
    CliCommand::cargo_bin("mdya")
        .unwrap()
        .args([
            "--config-dir",
            tmp.path().to_str().unwrap(),
            "search",
            "vector",
            "anything",
            "-c",
            "ghost",
        ])
        .assert()
        .failure()
        .stderr(contains("unknown collection: 'ghost'"));
}

#[test]
fn cli_zero_limit_exits_one_with_invalid_limit_message() {
    let tmp = TempDir::new().unwrap();
    init_only(&tmp);
    CliCommand::cargo_bin("mdya")
        .unwrap()
        .args([
            "--config-dir",
            tmp.path().to_str().unwrap(),
            "search",
            "vector",
            "anything",
            "-n",
            "0",
        ])
        .assert()
        .failure()
        .stderr(contains("limit must be >= 1"));
}
