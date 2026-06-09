//! Regression: `tracing_init::init` must skip `IndicatifLayer`
//! attach in non-TTY subprocesses.
//!
//! Without the fix, `IndicatifLayer::on_new_span` hijacks every
//! dependency-crate `#[tracing::instrument]` span (the layer inserts
//! an `IndicatifSpanContext` unconditionally, not opt-in via
//! `IndicatifSpanExt`). With a busy lance table the scan spawns >7
//! concurrent active spans (default `max_progress_bars`), pushing the
//! pending counter above zero. As the queue drains against a
//! footer that `indicatif` auto-hides in non-TTY, tracing-indicatif's
//! `pb_manager.rs:160`
//! `debug_assert!(!footer_pb.is_hidden(), ...)` fires → panic.
//!
//! Setup: library API + `MockEmbedder` (no model download) ingest
//! 50 markdown docs. Verify: `mdya get` spawned as a non-TTY
//! subprocess via `assert_cmd` does NOT emit the custom panic hook
//! line ("this is a bug in mdya").

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use predicates::prelude::*;
use tempfile::TempDir;

use mdya::config::{self, CollectionEntry, Config};
use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;

/// Doc count chosen to exceed tracing-indicatif's default
/// `max_progress_bars=7`. Lance's scan during `mdya get` spawns
/// enough concurrent `#[instrument]` spans (`FilteredReadStream::
/// read_fragment` and friends) to push the pending queue above zero,
/// reproducing the panic precondition in non-TTY subprocesses.
const DOC_COUNT: usize = 50;

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

fn write_md(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write md");
}

/// Materialise `<base>` with a `config.yml` + empty `chunks` / `sources`
/// tables, then ingest `DOC_COUNT` markdown files into the `notes`
/// collection via the library API. Returns `(base, coll_dir)`.
async fn setup_busy_db(tmp: &TempDir) -> Result<(PathBuf, PathBuf)> {
    let base = tmp.path().to_path_buf();
    let coll_dir = base.join("notes");
    std::fs::create_dir_all(&coll_dir)?;

    let mut cfg = Config::init_template();
    cfg.collections.insert(
        "notes".to_string(),
        CollectionEntry {
            path: coll_dir.to_string_lossy().into_owned(),
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

    for i in 0..DOC_COUNT {
        write_md(
            &coll_dir,
            &format!("doc-{i:03}.md"),
            &format!("# Doc {i}\n\nbody {i}.\n"),
        );
    }

    let mut collections = BTreeMap::new();
    collections.insert("notes".to_string(), coll_dir.clone());
    update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder),
        Arc::new(NullProgress),
        0,
    )
    .await?;

    Ok((base, coll_dir))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mdya_get_against_busy_db_does_not_panic_in_non_tty_subprocess() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _lance_home_guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, _coll_dir) = setup_busy_db(&tmp).await?;

    // `assert_cmd`'s default stdio is `Stdio::piped()` on all three
    // streams, so the child binary sees a non-TTY stderr — exactly the
    // environment that triggered the panic before the fix.
    CliCommand::cargo_bin("mdya")?
        .args([
            "--config-dir",
            base.to_str().expect("utf-8 path"),
            "get",
            "notes",
            "doc-001.md",
        ])
        .assert()
        // `.success()` catches panic exit codes (101 from `process::abort`
        // after the panic hook) even if the panic hook string ever
        // changes. The stderr assertion below remains as the primary
        // signal that the specific `tracing-indicatif` debug_assert
        // path is no longer reached.
        .success()
        .stderr(predicate::str::contains("this is a bug in mdya").not());

    Ok(())
}
