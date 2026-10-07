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
use mdya::get::{ChunkSpan, GetError, get_chunks, get_document};
use mdya::ingest::{IngestError, NullProgress, update_all_collections};
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{
    CHUNKS_TABLE_NAME, COL_COLLECTION, COL_PATH, SOURCES_TABLE_NAME, chunks_schema, sources_schema,
};

use common::{
    LANCE_ENV_LOCK, ScopedLanceLanguageModelHome, downgrade_chunks_table_to_pre_range_layout,
};

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

/// Read the single chunk `chunk_sequence` (a span of one).
async fn get_chunk(
    base: &Path,
    collection: &str,
    path: &str,
    chunk_sequence: u32,
) -> Result<String, GetError> {
    get_chunks(
        base,
        collection,
        path,
        ChunkSpan::new(chunk_sequence, None)?,
    )
    .await
}

/// Overwrite the `sources` row for `(collection, path)` with a bogus hash
/// and content — simulating a `sources` table left diverged from
/// `chunks` by an interrupted previous run.
async fn corrupt_sources_row(base: &Path, collection: &str, path: &str) -> Result<()> {
    write_sources_row(
        base,
        collection,
        path,
        &format!("{:0>64}", "dead"),
        "TAMPERED — stale sources content",
    )
    .await
}

/// Overwrite the `sources` row for `(collection, path)` with `source_hash`
/// and `content`.
async fn write_sources_row(
    base: &Path,
    collection: &str,
    path: &str,
    source_hash: &str,
    content: &str,
) -> Result<()> {
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(SOURCES_TABLE_NAME).execute().await?;
    let schema = std::sync::Arc::new(sources_schema());
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec![collection])),
            Arc::new(StringArray::from(vec![path])),
            Arc::new(StringArray::from(vec![source_hash])),
            Arc::new(StringArray::from(vec![content])),
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
async fn get_chunk_returns_the_original_text_of_a_valid_sequence() -> Result<()> {
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

    // The only chunk covers the whole document, heading markup included —
    // the original text, not the plain-text body.
    let text = get_chunk(&base, "notes", "release.md", 0).await?;
    assert_eq!(text, "# Release\n\nrelease checklist body.\n");
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
async fn cli_get_chunk_path_enforces_the_cap_and_honors_the_bypass_flag() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;

    let big = format!("# Big\n\n{}\n", "lorem ipsum dolor ".repeat(8));
    write_md(&coll_dir, "big.md", &big);
    ingest(&base, &coll_dir).await?;
    // A chunk range can span a whole document, so chunk reads are capped
    // like full-document reads.
    set_cli_max_bytes(&base, 1).await?;

    mdya_get(&base, &["--chunk", "0"])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(contains("chunk too large"))
        .stderr(contains("use --no-size-limit to override"));
    mdya_get(&base, &["--chunk", "0", "--chunk-end", "5"])
        .assert()
        .failure()
        .stderr(contains("chunk range too large"));

    mdya_get(&base, &["--chunk", "0", "-f"])
        .assert()
        .success()
        .stdout(big.clone());

    Ok(())
}

/// A document with `n` sections of short bodies — one chunk per section.
fn sectioned_document(n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "## Section {i}\n\nbody of section {i} with [a link](https://example.com/{i}).\n\n"
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_reads_tile_back_to_the_original_document() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    // 15 chunks: more than LanceDB's default top-k of 10, so a silently
    // limited range query would truncate the result.
    let doc = format!("---\ntitle: t\n---\n{}", sectioned_document(15));
    write_md(&coll_dir, "doc.md", &doc);
    ingest(&base, &coll_dir).await?;

    // Every chunk read in turn reassembles the document exactly; each one
    // carries its original Markdown (link targets included).
    let mut rebuilt = String::new();
    for i in 0..15 {
        let text = get_chunk(&base, "notes", "doc.md", i).await?;
        assert!(
            text.contains(&format!("](https://example.com/{i})")),
            "chunk {i} should return original Markdown: {text:?}"
        );
        rebuilt.push_str(&text);
    }
    assert_eq!(rebuilt, doc);

    // The whole span is the whole document, in one contiguous piece.
    let all = get_chunks(&base, "notes", "doc.md", ChunkSpan::new(0, Some(14))?).await?;
    assert_eq!(all, doc);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_span_reads_neighbours_and_clamps_an_end_past_the_last_chunk() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    let doc = sectioned_document(4);
    write_md(&coll_dir, "doc.md", &doc);
    ingest(&base, &coll_dir).await?;

    let one = get_chunk(&base, "notes", "doc.md", 1).await?;
    let two = get_chunk(&base, "notes", "doc.md", 2).await?;
    let three = get_chunk(&base, "notes", "doc.md", 3).await?;

    // N..M is the concatenation of N..=M, with no repeated text.
    let span = get_chunks(&base, "notes", "doc.md", ChunkSpan::new(1, Some(2))?).await?;
    assert_eq!(span, format!("{one}{two}"));
    // An end past the last chunk reads to the end of the document.
    let tail = get_chunks(&base, "notes", "doc.md", ChunkSpan::new(1, Some(99))?).await?;
    assert_eq!(tail, format!("{one}{two}{three}"));
    assert!(doc.ends_with(&tail));

    // A start past the last chunk is still "chunk not found".
    let err = get_chunks(&base, "notes", "doc.md", ChunkSpan::new(4, Some(9))?)
        .await
        .expect_err("start past the last chunk");
    assert!(
        matches!(
            err,
            GetError::ChunkNotFound {
                chunk_sequence: 4,
                ..
            }
        ),
        "got {err:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_read_refuses_a_chunks_row_out_of_sync_with_sources() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "doc.md", "# Doc\n\nthe real body.\n");
    ingest(&base, &coll_dir).await?;

    // `sources` now holds other text under another hash: slicing it with
    // the `chunks` ranges would return the wrong bytes.
    corrupt_sources_row(&base, "notes", "doc.md").await?;
    let err = get_chunk(&base, "notes", "doc.md", 0)
        .await
        .expect_err("diverged tables are refused");
    assert!(
        matches!(err, GetError::IndexOutOfSync { .. }),
        "got {err:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outdated_index_refuses_chunk_reads_and_ingest_but_serves_full_documents() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    let original = "# Doc\n\nthe real body.\n";
    write_md(&coll_dir, "doc.md", original);
    ingest(&base, &coll_dir).await?;
    downgrade_chunks_table_to_pre_range_layout(&base).await?;

    let err = get_chunk(&base, "notes", "doc.md", 0)
        .await
        .expect_err("chunk read on an old index");
    match err {
        GetError::IndexOutdated { model } => assert_eq!(model, DEFAULT_MODEL_ID),
        other => panic!("expected IndexOutdated, got {other:?}"),
    }
    // Full-document reads do not touch `chunks` and keep working.
    assert_eq!(get_document(&base, "notes", "doc.md").await?, original);

    let mut collections = BTreeMap::new();
    collections.insert("notes".to_string(), coll_dir.clone());
    let err = update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder),
        Arc::new(NullProgress),
        0,
    )
    .await
    .expect_err("update-all on an old index");
    assert!(
        matches!(err, IngestError::IndexOutdated { .. }),
        "got {err:?}"
    );
    assert!(
        err.to_string()
            .contains("mdya vector use cl-nagoya/ruri-v3-30m")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_get_rejects_invalid_chunk_ranges() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "big.md", &sectioned_document(3));
    ingest(&base, &coll_dir).await?;

    // `--chunk-end` alone is an argument error (clap `requires`).
    mdya_get(&base, &["--chunk-end", "1"])
        .assert()
        .failure()
        .stderr(contains("--chunk <N>"));
    // An end before the start is refused by the library.
    mdya_get(&base, &["--chunk", "2", "--chunk-end", "1"])
        .assert()
        .failure()
        .stderr(contains("--chunk-end (1) must be >= --chunk (2)"));
    // An end equal to the start reads that one chunk.
    let single = get_chunk(&base, "notes", "big.md", 1).await?;
    mdya_get(&base, &["--chunk", "1", "--chunk-end", "1"])
        .assert()
        .success()
        .stdout(single);
    Ok(())
}

/// Lowercase hex SHA-256 of `bytes`, the `source_hash` ingest records.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunk_read_refuses_a_range_past_the_stored_text() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    let original = "# Doc\n\nthe real body.\n";
    write_md(&coll_dir, "doc.md", original);
    ingest(&base, &coll_dir).await?;

    // Same hash, shorter text: the hashes agree but the chunk's range no
    // longer fits, which must not panic or return a partial slice.
    write_sources_row(
        &base,
        "notes",
        "doc.md",
        &sha256_hex(original.as_bytes()),
        "short",
    )
    .await?;
    let err = get_chunk(&base, "notes", "doc.md", 0)
        .await
        .expect_err("range past the stored text");
    assert!(
        matches!(err, GetError::IndexOutOfSync { .. }),
        "got {err:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_block_chunks_overlap_singly_but_a_range_read_does_not() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    // One paragraph over the window: split into overlapping sub-chunks.
    let doc = format!("{}\n", "word ".repeat(400));
    write_md(&coll_dir, "long.md", &doc);
    ingest(&base, &coll_dir).await?;

    let all = get_chunks(&base, "notes", "long.md", ChunkSpan::new(0, Some(99))?).await?;
    assert_eq!(all, doc);

    let mut singles = String::new();
    let mut count = 0;
    while let Ok(text) = get_chunk(&base, "notes", "long.md", count).await {
        singles.push_str(&text);
        count += 1;
    }
    assert!(
        count >= 2,
        "the paragraph must be split, got {count} chunk(s)"
    );
    assert!(
        singles.len() > doc.len(),
        "single reads of a split block repeat text at the seams"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_search_human_output_shows_the_document_chunk_count() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (base, coll_dir) = setup(&tmp).await?;
    write_md(&coll_dir, "doc.md", &sectioned_document(3));
    ingest(&base, &coll_dir).await?;

    CliCommand::cargo_bin("mdya")?
        .args([
            "--config-dir",
            base.to_str().unwrap(),
            "search",
            "fts",
            "section",
        ])
        .assert()
        .success()
        .stdout(contains("notes/doc.md  score="))
        .stdout(contains("  chunks=3\n"));
    Ok(())
}
