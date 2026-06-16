//! Smoke for `mdya get` / `get_document`: ingest a corpus with the
//! mock embedder, then fetch faithful originals from the `sources` table.
//!
//! The headline assertion is that `get` returns the **original** bytes —
//! front matter and inline formatting that the lossy `chunks.body` drops —
//! proving retrieval reads `sources`, not a chunk re-assembly. Zero-chunk
//! files (front-matter-only) are still retrievable, and unknown collection
//! / missing path map to typed errors.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray};
use assert_cmd::Command as CliCommand;
use predicates::str::contains;
use tempfile::TempDir;

use mdya::config::{self, CollectionEntry, Config};
use mdya::embedding::{EmbedError, Embedder};
use mdya::get::{GetError, get_chunk, get_document};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{
    CHUNKS_TABLE_NAME, COL_COLLECTION, COL_PATH, SOURCES_TABLE_NAME, chunks_schema, sources_schema,
};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;

/// Constant-vector stand-in so the smoke never downloads the real model.
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

/// Write `config.yml` declaring the `notes` collection and create the
/// `chunks` + `sources` tables. Returns `(base_config_dir, collection_dir)`.
async fn setup(tmp: &TempDir) -> Result<(PathBuf, PathBuf)> {
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

fn write_md(coll_dir: &Path, name: &str, body: &str) {
    std::fs::write(coll_dir.join(name), body).expect("write md");
}

/// Overwrite the `sources` row for `(collection, path)` with a bogus hash
/// and content — simulating a `sources` table left diverged from
/// `chunks` by an interrupted previous run.
async fn corrupt_sources_row(base: &Path, collection: &str, path: &str) -> Result<()> {
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(SOURCES_TABLE_NAME).execute().await?;
    let schema = std::sync::Arc::new(sources_schema());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec![collection])),
            Arc::new(StringArray::from(vec![path])),
            Arc::new(StringArray::from(vec![format!("{:0>64}", "dead")])),
            Arc::new(StringArray::from(vec!["TAMPERED — stale sources content"])),
        ],
    )?;
    let reader: Box<dyn RecordBatchReader + Send> =
        Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    let mut builder = table.merge_insert(&[COL_COLLECTION, COL_PATH]);
    builder.when_matched_update_all(None);
    builder.when_not_matched_insert_all();
    builder.execute(reader).await?;
    Ok(())
}

/// Delete the `sources` row for `(collection, path)` — simulating a crash
/// that wrote `chunks` but never its `sources` mirror.
async fn delete_sources_row(base: &Path, collection: &str, path: &str) -> Result<()> {
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(SOURCES_TABLE_NAME).execute().await?;
    table
        .delete(&format!(
            "{COL_COLLECTION} = '{collection}' AND {COL_PATH} = '{path}'"
        ))
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_returns_the_faithful_original_including_front_matter_and_headings() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    // Front matter (stripped from chunks) + heading line (folded into the
    // chunk body) + inline formatting (flattened in chunk bodies): all of
    // it must come back verbatim from `get`.
    let original = "---\ntitle: Release\ndate: 2024-01-01\n---\n# Release\n\nrelease checklist with **bold** text.\n";
    write_md(&coll_dir, "release.md", original);
    ingest(&base, &coll_dir).await?;

    let got = get_document(&base, "notes", "release.md").await?;
    assert_eq!(got, original);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_returns_a_zero_chunk_front_matter_only_document() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    // No chunkable body -> a placeholder chunk is stored, and `sources`
    // still holds the faithful original so `get` succeeds.
    let stub = "---\nonly: frontmatter\n---\n";
    write_md(&coll_dir, "stub.md", stub);
    ingest(&base, &coll_dir).await?;

    let got = get_document(&base, "notes", "stub.md").await?;
    assert_eq!(got, stub);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_missing_path_is_not_found() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "release.md", "# Release\n\nbody\n");
    ingest(&base, &coll_dir).await?;

    let err = get_document(&base, "notes", "nope.md")
        .await
        .expect_err("missing path is an error");
    assert!(matches!(err, GetError::NotFound { .. }), "got {err:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_unknown_collection_is_rejected_before_lookup() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, _coll_dir) = setup(&tmp).await?;

    let err = get_document(&base, "ghost", "release.md")
        .await
        .expect_err("unknown collection is an error");
    assert!(
        matches!(err, GetError::UnknownCollection { .. }),
        "got {err:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_sources_row_is_repaired_on_next_update_all() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    let original = "# Doc\n\nthe real body.\n";
    write_md(&coll_dir, "doc.md", original);
    ingest(&base, &coll_dir).await?;
    assert_eq!(get_document(&base, "notes", "doc.md").await?, original);

    // Diverge `sources` from `chunks` (wrong hash + wrong content). The
    // file on disk is unchanged, so the only thing that can force a
    // re-ingest is the source_hash consistency gate.
    corrupt_sources_row(&base, "notes", "doc.md").await?;
    // Guard: the corruption actually took (otherwise the test proves nothing).
    assert_eq!(
        get_document(&base, "notes", "doc.md").await?,
        "TAMPERED — stale sources content"
    );

    // A plain re-run must detect the divergence and repair `sources` back
    // to the faithful original — not skip it forever.
    ingest(&base, &coll_dir).await?;
    assert_eq!(get_document(&base, "notes", "doc.md").await?, original);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_chunk_returns_a_chunk_body_for_a_valid_sequence() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    // A non-empty section guarantees at least one chunk at sequence 0.
    write_md(
        &coll_dir,
        "release.md",
        "# Release\n\nrelease checklist body.\n",
    );
    ingest(&base, &coll_dir).await?;

    let body = get_chunk(&base, "notes", "release.md", 0).await?;
    // The chunk body folds in the heading and section body (lossy vs the
    // raw source); a stable substring is enough — exercising the read path,
    // not the chunker.
    assert!(
        body.contains("release checklist body"),
        "chunk body should carry the section text: {body:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_chunk_out_of_range_sequence_is_chunk_not_found() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "release.md", "# Release\n\nbody.\n");
    ingest(&base, &coll_dir).await?;

    let err = get_chunk(&base, "notes", "release.md", 9999)
        .await
        .expect_err("out-of-range chunk_sequence is an error");
    match err {
        GetError::ChunkNotFound {
            collection,
            path,
            chunk_sequence,
        } => {
            assert_eq!(collection, "notes");
            assert_eq!(path, "release.md");
            assert_eq!(chunk_sequence, 9999);
        }
        other => panic!("expected ChunkNotFound, got {other:?}"),
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_chunk_missing_path_is_chunk_not_found() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "release.md", "# Release\n\nbody.\n");
    ingest(&base, &coll_dir).await?;

    let err = get_chunk(&base, "notes", "nope.md", 0)
        .await
        .expect_err("missing path is an error");
    // Missing-path collapses into the same ChunkNotFound branch — the
    // chunks table has no rows for the locator, regardless of which leg
    // (path vs sequence) is wrong. The chunk path treats path absence and
    // sequence absence as one failure on purpose: a caller arriving here
    // with a `chunk_sequence` from a search hit has already proven the
    // path exists, so the only remaining distinction (stale sequence vs
    // recently deleted document) is too brittle a signal to encode in the
    // error type. Document-vs-chunk separation is the document-fetch
    // error's job.
    assert!(matches!(err, GetError::ChunkNotFound { .. }), "got {err:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_chunk_unknown_collection_is_rejected_before_lookup() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, _coll_dir) = setup(&tmp).await?;

    let err = get_chunk(&base, "ghost", "release.md", 0)
        .await
        .expect_err("unknown collection is an error");
    assert!(
        matches!(err, GetError::UnknownCollection { .. }),
        "got {err:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_sources_row_is_reinserted_on_next_update_all() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    let original = "# Doc\n\nthe real body.\n";
    write_md(&coll_dir, "doc.md", original);
    ingest(&base, &coll_dir).await?;

    // Drop the `sources` row (chunks-written-but-sources-not crash).
    delete_sources_row(&base, "notes", "doc.md").await?;
    let missing = get_document(&base, "notes", "doc.md")
        .await
        .expect_err("sources row is gone");
    assert!(
        matches!(missing, GetError::NotFound { .. }),
        "got {missing:?}"
    );

    // Re-running must re-insert the missing mirror, not skip the file.
    ingest(&base, &coll_dir).await?;
    assert_eq!(get_document(&base, "notes", "doc.md").await?, original);
    Ok(())
}

/// Set `get.cli_max_bytes` in an existing `config.yml` and persist it.
async fn set_cli_max_bytes(base: &Path, max_bytes: u64) -> Result<()> {
    let cfg_path = base.join("config.yml");
    let mut cfg = config::load(&cfg_path)?;
    cfg.get.cli_max_bytes = max_bytes;
    config::save(&cfg_path, &cfg)?;
    Ok(())
}

fn mdya_get(base: &Path, extra: &[&str]) -> CliCommand {
    let mut cmd = CliCommand::cargo_bin("mdya").expect("binary builds");
    cmd.args([
        "--config-dir",
        base.to_str().unwrap(),
        "get",
        "notes",
        "big.md",
    ]);
    cmd.args(extra);
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_get_enforces_cli_max_bytes_and_honors_the_bypass_flag() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    // A full document comfortably over the small cap we set below.
    let big = format!("# Big\n\n{}\n", "lorem ipsum dolor ".repeat(8));
    write_md(&coll_dir, "big.md", &big);
    ingest(&base, &coll_dir).await?;
    assert!(big.len() as u64 > 64, "fixture must exceed the test cap");
    set_cli_max_bytes(&base, 64).await?;

    // Over the cap, no flag: exit 1 with the human error + override hint on
    // stderr, and nothing on stdout (the document must not leak past the cap).
    mdya_get(&base, &[])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("document too large"))
        .stderr(contains("use --no-size-limit to override"));

    // `-f` bypasses the cap and prints the document byte-for-byte.
    mdya_get(&base, &["-f"])
        .assert()
        .success()
        .stdout(big.clone());

    // `--no-size-limit` (the long form) behaves identically.
    mdya_get(&base, &["--no-size-limit"])
        .assert()
        .success()
        .stdout(big.clone());

    // `cli_max_bytes: 0` disables the cap entirely — the same over-cap
    // document now prints without a flag.
    set_cli_max_bytes(&base, 0).await?;
    mdya_get(&base, &[]).assert().success().stdout(big.clone());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_get_chunk_path_is_never_size_checked() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    let big = format!("# Big\n\n{}\n", "lorem ipsum dolor ".repeat(8));
    write_md(&coll_dir, "big.md", &big);
    ingest(&base, &coll_dir).await?;
    // Cap below even one chunk body: the full-document path would fail, but
    // `--chunk` must still succeed (chunk reads are out of the guard's scope).
    set_cli_max_bytes(&base, 1).await?;

    mdya_get(&base, &["--chunk", "0"])
        .assert()
        .success()
        .stdout(contains("lorem ipsum dolor"));

    Ok(())
}
