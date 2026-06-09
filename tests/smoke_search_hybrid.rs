//! Smoke tests for `mdya search hybrid`.
//!
//! Library-level tests drive `SearchEngine::hybrid` against a real
//! LanceDB tempdir populated by `update_all_collections` +
//! `MockEmbedder`. The hybrid path goes through
//! `VectorQuery::execute_with_options` (lancedb 0.29 `src/query.rs:1304-1316`)
//! which auto-dispatches to `execute_hybrid` because both
//! `full_text_search` and `nearest_to` are set on the builder. The
//! reranker default is `RRFReranker` (k=60), so `_relevance_score`
//! ranges over `[0, 2/(k+1)] ≈ [0, 0.033]` — bounded loosely as
//! `[0, 1]` in assertions to leave room for the LanceDB default reranker
//! to evolve without rewriting the test.
//!
//! CLI 3 tests reuse the validation surface (empty query / unknown
//! collection / `-n 0`) from the FTS smoke pattern, since
//! `SearchEngine::hybrid` reuses the same `validate` path that `fts`
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
use mdya::search::{SearchEngine, SearchLevel, SearchMode, SearchRequest};
use mdya::store::lance_lm::lance_models_dir;

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;
const DEFAULT_VECTOR_DIM_I32: i32 = 256;

/// Same-vector mock for both query and passage so cosine distance ≈ 0
/// ⇒ similarity ≈ 1; mirrors `smoke_search_vector.rs::MockEmbedder` so
/// the vector contribution to hybrid is non-trivial but deterministic.
/// The exact value does not matter — only that hits are reachable.
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

// ---------- Library-level tests (real ingest + SearchEngine::hybrid) ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hybrid_returns_at_least_one_hit_with_relevance_score() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(
        &coll_dir,
        "release.md",
        "# Release\n\nrelease checklist here.\n",
    );
    write_md(&coll_dir, "other.md", "# Other\n\nsomething unrelated.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .hybrid(
            &SearchRequest {
                query: "release".to_string(),
                collections: vec![],
                limit: 20,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;

    assert_eq!(resp.mode, SearchMode::Hybrid);
    assert!(resp.total >= 1, "expected hits, got {resp:?}");
    // `_relevance_score` from `RRFReranker::default()` (k=60) is the sum
    // of `1.0 / (k + rank)` contributions across at most 2 sources, so
    // the upper bound is `2/(k+1) ≈ 0.0328`. We bound loosely by 1.0
    // to stay robust against the LanceDB default reranker evolving.
    for h in &resp.hits {
        assert!(
            h.score() >= 0.0 && h.score() <= 1.0,
            "score out of [0, 1]: {h:?}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hybrid_hits_are_sorted_by_score_descending() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(
        &coll_dir,
        "a.md",
        "# A\n\nrelease checklist primary content.\n",
    );
    write_md(&coll_dir, "b.md", "# B\n\nrelease body.\n");
    write_md(&coll_dir, "c.md", "# C\n\nunrelated content.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .hybrid(
            &SearchRequest {
                query: "release".to_string(),
                collections: vec![],
                limit: 20,
                level: SearchLevel::Chunk,
            },
            &MockEmbedder,
        )
        .await?;

    // Pin the deterministic tie-break sort that `sort_hits_stable`
    // applies — same direction (DESC) across all 3 modes, see
    // `engine.rs::sort_hits_stable` doc.
    for w in resp.hits.windows(2) {
        assert!(
            w[0].score() >= w[1].score(),
            "hits not sorted DESC by score: {:?}",
            resp.hits
        );
    }
    Ok(())
}

// `SearchEngine::open`-time corruption paths (schema metadata Missing
// abort + Mismatch warn-and-continue) are exhaustively covered by
// `smoke_search_vector.rs`, which exercises the same `open` code path.
// Adding hybrid-side variants would be duplication, and the
// `tamper_schema_metadata` helper recreates the chunks table without
// rebuilding the FTS inverted index — hybrid `full_text_search` would
// fail with `LanceError(Invalid input)` before reaching the corruption
// check. The Mismatch contract is mode-agnostic by design, so
// vector-side coverage suffices.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hybrid_collection_filter_uses_type_safe_only_if_expr() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "a.md", "# A\n\nrelease body.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let resp = engine
        .hybrid(
            &SearchRequest {
                query: "release".to_string(),
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
//
// `run_hybrid` calls `engine.validate_request(...)` BEFORE loading the
// `cl-nagoya/ruri-v3-30m` weights (~140 MB), so obvious typos exit `1`
// without ever touching the model cache. These tests pin that ordering
// — if validate were moved past the embedder load, every CLI typo would
// trigger a first-run download.

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
            "hybrid",
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
            "hybrid",
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
            "hybrid",
            "anything",
            "-n",
            "0",
        ])
        .assert()
        .failure()
        .stderr(contains("limit must be >= 1"));
}
