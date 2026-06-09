//! Smoke tests for `mdya search fts`.
//!
//! Three CLI-binary tests exercise the validation path (empty query,
//! unknown collection, `--limit 0`) — these only need `mdya init` to
//! land before the engine errors out, so they avoid the real ingest
//! pipeline. Three library-level tests drive `SearchEngine::fts`
//! against a real LanceDB tempdir populated by `update_all_collections`
//! + `MockEmbedder` (same pattern as `tests/smoke_update_all_index.rs`).

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
use mdya::search::{SearchEngine, SearchLevel, SearchMode, SearchRequest, SearchResponse};
use mdya::store::lance_lm::lance_models_dir;

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;

/// Constant-vector stand-in for the production embedder. Search hits
/// are decided by the FTS index, not the vector column, so a fixed
/// vector is enough.
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

fn write_md(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
}

/// Materialise `~/.mdya/`-equivalent dirs, `config.yml` declaring one
/// collection, and the `chunks` table. Mirrors what `mdya init` +
/// `mdya collection add` would do, but lets the test stay in-process.
///
/// **Discipline note**: `smoke_update_all_index.rs::fresh_config_dir`
/// builds the table via `mdya::store::connect` in-process; this file
/// spawns the `mdya init` binary instead so the search CLI surface is
/// covered end-to-end (init → search) in one binary run. The
/// divergence is intentional, not an oversight — keep it explicit so
/// later contributors do not "unify" the two patterns and lose the
/// init contract coverage.
async fn fresh_corpus(tmp: &TempDir) -> Result<(PathBuf, PathBuf)> {
    let base = tmp.path().to_path_buf();
    let coll_dir = tmp.path().join("notes");
    std::fs::create_dir(&coll_dir)?;

    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", base.to_str().unwrap(), "init"])
        .assert()
        .success();

    // Append the collection to config.yml manually; `mdya collection add`
    // also works but spawning a second binary is overhead the test does
    // not need.
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

// ---------- Library-level tests (real ingest + SearchEngine::fts) ----------

#[tokio::test]
async fn fts_returns_at_least_one_hit_for_matching_query() -> Result<()> {
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

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM as i32).await?;
    let resp = engine
        .fts(&SearchRequest {
            query: "release".to_string(),
            collections: vec![],
            limit: 20,
            level: SearchLevel::Chunk,
        })
        .await?;

    assert_eq!(resp.mode, SearchMode::Fts);
    assert!(resp.total >= 1, "expected at least one hit, got {resp:?}");
    assert!(
        resp.hits.iter().any(|h| h.path() == "release.md"),
        "expected `release.md` in hits, got {resp:?}"
    );
    Ok(())
}

#[tokio::test]
async fn fts_returns_empty_envelope_when_no_hits() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "a.md", "# A\n\nbody a.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM as i32).await?;
    let resp = engine
        .fts(&SearchRequest {
            query: "completelyabsentword".to_string(),
            collections: vec![],
            limit: 20,
            level: SearchLevel::Chunk,
        })
        .await?;

    assert_eq!(resp.total, 0);
    assert!(resp.hits.is_empty());
    Ok(())
}

#[tokio::test]
async fn fts_response_round_trips_through_json_envelope() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let (base, coll_dir) = fresh_corpus(&tmp).await?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    write_md(&coll_dir, "doc.md", "# Doc\n\nrelease body text.\n");
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM as i32).await?;
    let resp = engine
        .fts(&SearchRequest {
            query: "release".to_string(),
            collections: vec![],
            limit: 20,
            level: SearchLevel::Chunk,
        })
        .await?;
    let json = serde_json::to_string(&resp)?;
    let parsed: SearchResponse = serde_json::from_str(&json)?;
    assert_eq!(parsed.mode, SearchMode::Fts);
    assert_eq!(parsed.total, resp.total);
    assert_eq!(parsed.hits.len(), resp.hits.len());
    Ok(())
}

// ---------- CLI validation tests (no ingest required) ----------

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
            "fts",
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
            "fts",
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
            "fts",
            "anything",
            "-n",
            "0",
        ])
        .assert()
        .failure()
        .stderr(contains("limit must be >= 1"));
}
