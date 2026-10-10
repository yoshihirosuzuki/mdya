//! Integration smoke tests for archive collections: a `.zip` / `.tar` /
//! `.tar.gz` registered as a collection is ingested through
//! `ingest::update_all_collections` without being extracted.
//!
//! Setup mirrors `smoke_ingest.rs`: a fresh `~/.mdya/`-style tempdir per
//! test, a deterministic mock `Embedder`, and `LANCE_LANGUAGE_MODEL_HOME`
//! pinned through the shared `tests/common/mod.rs` helper under
//! `LANCE_ENV_LOCK`.

mod common;

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use arrow_array::cast::AsArray;
use arrow_array::{ArrayRef, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use tempfile::TempDir;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{
    FileOutcome, IngestProgress, NullProgress, UpdateSummary, update_all_collections,
};
use mdya::store::lance_lm::lance_models_dir;
use mdya::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const VECTOR_DIM: usize = 256;

/// Deterministic stand-in for the real embedder, as in `smoke_ingest.rs`.
struct MockEmbedder;

impl Embedder for MockEmbedder {
    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        VECTOR_DIM
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| vec![0.1_f32; VECTOR_DIM]).collect())
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|_| vec![0.2_f32; VECTOR_DIM]).collect())
    }
}

async fn fresh_config_dir(tmp: &TempDir) -> Result<PathBuf> {
    let base = tmp.path().join("mdya");
    let index_dir = base.join("index");
    std::fs::create_dir_all(&index_dir)?;
    let db = mdya::store::connect(&index_dir).await?;
    let schema = chunks_schema(i32::try_from(VECTOR_DIM)?, MODEL_ID);
    db.create_empty_table(CHUNKS_TABLE_NAME, Arc::new(schema))
        .execute()
        .await?;
    db.create_empty_table(SOURCES_TABLE_NAME, Arc::new(sources_schema()))
        .execute()
        .await?;
    Ok(base)
}

/// Distinct `(collection, path)` pairs in `table`, sorted.
async fn indexed(base: &Path, table: &str) -> Result<Vec<(String, String)>> {
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(table).execute().await?;
    let batches = table
        .query()
        .execute()
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    let mut pairs = std::collections::BTreeSet::new();
    for batch in &batches {
        let collections: &StringArray =
            batch.column_by_name("collection").expect("col").as_string();
        let paths: &StringArray = batch.column_by_name("path").expect("path").as_string();
        for i in 0..batch.num_rows() {
            pairs.insert((collections.value(i).to_string(), paths.value(i).to_string()));
        }
    }
    Ok(pairs.into_iter().collect())
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(c, p)| (c.to_string(), p.to_string()))
        .collect()
}

async fn ingest(
    base: &Path,
    collections: &[(&str, &Path)],
    parallelism: usize,
) -> Result<UpdateSummary> {
    let collections: BTreeMap<String, PathBuf> = collections
        .iter()
        .map(|(name, path)| (name.to_string(), path.to_path_buf()))
        .collect();
    Ok(update_all_collections(
        &collections,
        base,
        Arc::new(MockEmbedder),
        Arc::new(NullProgress),
        parallelism,
    )
    .await?)
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in entries {
        writer.start_file(*name, options).expect("start");
        writer.write_all(data).expect("data");
    }
    writer.finish().expect("finish").into_inner()
}

fn tar_gz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, name, *data)
            .expect("append");
    }
    let tar = builder.into_inner().expect("finish tar");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).expect("gzip");
    gz.finish().expect("finish gzip")
}

/// Write `bytes` to `path` and give it an mtime `seconds` past a fixed
/// point, so a replaced archive is guaranteed to look changed.
fn write_archive(path: &Path, bytes: &[u8], seconds: u64) {
    std::fs::write(path, bytes).expect("write archive");
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(mtime))
        .expect("set mtime");
}

/// Mark the entry named `name` as compressed with method 12 (bzip2, which
/// this build cannot read), in both its local and central headers.
fn mark_unsupported(bytes: &mut [u8], name: &[u8]) {
    // (signature, name length offset, name offset, method offset)
    let headers: [(&[u8; 4], usize, usize, usize); 2] =
        [(b"PK\x03\x04", 26, 30, 8), (b"PK\x01\x02", 28, 46, 10)];
    for (signature, len_at, name_at, method_at) in headers {
        let starts: Vec<usize> = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, w)| w == signature)
            .map(|(i, _)| i)
            .collect();
        for start in starts {
            let len = u16::from_le_bytes([bytes[start + len_at], bytes[start + len_at + 1]]);
            let found = &bytes[start + name_at..start + name_at + usize::from(len)];
            if found == name {
                bytes[start + method_at..start + method_at + 2]
                    .copy_from_slice(&12u16.to_le_bytes());
            }
        }
    }
}

#[tokio::test]
async fn zip_documents_are_indexed_under_their_entry_paths() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("export.zip");
    write_archive(
        &archive,
        &zip_bytes(&[
            ("export/docs/a.md", b"# A\n\nAlpha.\n"),
            ("export/b.md", b"# B\n\nBeta.\n"),
            ("export/img.png", b"\x89PNG"),
            ("__MACOSX/export/._b.md", b"\x00\x05\x16\x07"),
        ]),
        0,
    );

    let summary = ingest(&base, &[("export", &archive)], 0).await?;

    assert_eq!((summary.new, summary.failed), (2, 0), "{summary:?}");
    let expected = pairs(&[("export", "export/b.md"), ("export", "export/docs/a.md")]);
    assert_eq!(indexed(&base, CHUNKS_TABLE_NAME).await?, expected);
    assert_eq!(indexed(&base, SOURCES_TABLE_NAME).await?, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tar_gz_documents_are_indexed_in_parallel() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("repo.tgz");
    write_archive(
        &archive,
        &tar_gz_bytes(&[
            ("./a.md", b"# A\n"),
            ("./b.md", b"# B\n"),
            ("./c.md", b"# C\n"),
            ("./d/e.md", b"# E\n"),
        ]),
        0,
    );

    let summary = ingest(&base, &[("repo", &archive)], 2).await?;

    assert_eq!((summary.new, summary.failed), (4, 0), "{summary:?}");
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[
            ("repo", "a.md"),
            ("repo", "b.md"),
            ("repo", "c.md"),
            ("repo", "d/e.md")
        ])
    );
    Ok(())
}

#[tokio::test]
async fn replacing_the_archive_updates_changed_documents_and_removes_missing_ones() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    write_archive(
        &archive,
        &zip_bytes(&[
            ("keep.md", b"same"),
            ("edit.md", b"before"),
            ("gone.md", b"bye"),
        ]),
        0,
    );
    ingest(&base, &[("notes", &archive)], 0).await?;

    let again = ingest(&base, &[("notes", &archive)], 0).await?;
    assert_eq!(
        (again.skipped, again.new, again.updated),
        (3, 0, 0),
        "{again:?}"
    );

    write_archive(
        &archive,
        &zip_bytes(&[("keep.md", b"same"), ("edit.md", b"after")]),
        60,
    );
    let changed = ingest(&base, &[("notes", &archive)], 0).await?;

    assert_eq!(
        (
            changed.skipped,
            changed.updated,
            changed.removed,
            changed.failed
        ),
        (1, 1, 1, 0),
        "{changed:?}"
    );
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("notes", "edit.md"), ("notes", "keep.md")])
    );
    Ok(())
}

#[tokio::test]
async fn an_unreadable_archive_keeps_its_index_and_other_collections_still_update() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    let dir = tmp.path().join("dir");
    std::fs::create_dir(&dir)?;
    write_archive(&archive, &zip_bytes(&[("a.md", b"alpha")]), 0);
    ingest(&base, &[("notes", &archive), ("dir", &dir)], 0).await?;

    write_archive(&archive, b"not a zip any more", 60);
    std::fs::write(dir.join("new.md"), "# New\n")?;
    let broken = ingest(&base, &[("notes", &archive), ("dir", &dir)], 0).await?;

    std::fs::remove_file(&archive)?;
    let missing = ingest(&base, &[("notes", &archive), ("dir", &dir)], 0).await?;

    assert_eq!(broken.unreadable_collections, ["notes"]);
    assert_eq!((broken.new, broken.removed), (1, 0), "{broken:?}");
    assert_eq!(missing.unreadable_collections, ["notes"]);
    assert_eq!(missing.removed, 0, "{missing:?}");
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("dir", "new.md"), ("notes", "a.md")])
    );
    Ok(())
}

#[tokio::test]
async fn unreadable_documents_fail_without_losing_their_previous_rows() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha"), ("b.md", b"beta")]),
        0,
    );
    ingest(&base, &[("notes", &archive)], 0).await?;

    let mut bytes = zip_bytes(&[
        ("a.md", b"alpha 2"),
        ("b.md", b"beta 2"),
        ("\u{3042}.md", b"x"),
    ]);
    mark_unsupported(&mut bytes, b"b.md");
    write_archive(&archive, &bytes, 60);
    let summary = ingest(&base, &[("notes", &archive)], 0).await?;

    assert_eq!(
        (
            summary.new,
            summary.updated,
            summary.failed,
            summary.removed
        ),
        (1, 1, 1, 0),
        "{summary:?}"
    );
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[
            ("notes", "a.md"),
            ("notes", "b.md"),
            ("notes", "\u{3042}.md")
        ])
    );
    Ok(())
}

#[tokio::test]
async fn a_directory_named_like_an_archive_is_walked_as_a_directory() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let dir = tmp.path().join("notes.zip");
    std::fs::create_dir(&dir)?;
    std::fs::write(dir.join("a.md"), "# A\n")?;

    let summary = ingest(&base, &[("notes", &dir)], 0).await?;

    assert_eq!((summary.new, summary.failed), (1, 0), "{summary:?}");
    assert!(summary.unreadable_collections.is_empty());
    Ok(())
}

#[tokio::test]
async fn archive_documents_are_searchable_and_return_their_original_text() -> Result<()> {
    use mdya::config::{self, CollectionEntry, Config};
    use mdya::get::{ChunkSpan, get_chunks, get_document};
    use mdya::search::{SearchEngine, SearchLevel, SearchRequest};

    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("export.tar.gz");
    let original =
        "---\ntitle: Release\n---\n\n# Release notes\n\nThe zebracorn feature ships today.\n";
    write_archive(
        &archive,
        &tar_gz_bytes(&[("export/release.md", original.as_bytes())]),
        0,
    );
    let mut cfg = Config::init_template();
    cfg.collections.insert(
        "export".to_string(),
        CollectionEntry {
            path: archive.display().to_string(),
            description: None,
        },
    );
    config::save(&base.join("config.yml"), &cfg)?;
    ingest(&base, &[("export", &archive)], 0).await?;

    let engine = SearchEngine::open(&base, MODEL_ID, i32::try_from(VECTOR_DIM)?).await?;
    let response = engine
        .fts(&SearchRequest {
            query: "zebracorn".to_string(),
            collections: vec!["export".to_string()],
            limit: 10,
            level: SearchLevel::Doc,
        })
        .await?;

    assert!(
        response
            .hits
            .iter()
            .any(|h| h.path() == "export/release.md"),
        "{response:?}"
    );
    assert_eq!(
        get_document(&base, "export", "export/release.md").await?,
        original
    );
    let whole = get_chunks(
        &base,
        "export",
        "export/release.md",
        ChunkSpan::new(0, Some(u32::MAX))?,
    )
    .await?;
    assert_eq!(whole, original);
    Ok(())
}

#[tokio::test]
async fn vector_use_reports_an_unreadable_archive_and_update_all_restores_it() -> Result<()> {
    use mdya::cli::vector::switch_model;
    use mdya::config::{self, CollectionEntry, Config};

    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    let bytes = zip_bytes(&[("a.md", b"alpha")]);
    write_archive(&archive, &bytes, 0);
    let mut cfg = Config::init_template();
    cfg.collections.insert(
        "notes".to_string(),
        CollectionEntry {
            path: archive.display().to_string(),
            description: None,
        },
    );
    config::save(&base.join("config.yml"), &cfg)?;
    ingest(&base, &[("notes", &archive)], 0).await?;

    write_archive(&archive, b"truncated download", 60);
    let rebuilt = switch_model(
        &base,
        cfg,
        Arc::new(MockEmbedder),
        Arc::new(NullProgress),
        0,
    )
    .await?;

    assert_eq!(rebuilt.unreadable_collections, ["notes"]);
    assert!(indexed(&base, CHUNKS_TABLE_NAME).await?.is_empty());
    assert_eq!(
        indexed(&base, SOURCES_TABLE_NAME).await?,
        pairs(&[("notes", "a.md")])
    );

    write_archive(&archive, &bytes, 120);
    let restored = ingest(&base, &[("notes", &archive)], 0).await?;

    assert_eq!((restored.new, restored.failed), (1, 0), "{restored:?}");
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("notes", "a.md")])
    );
    Ok(())
}

#[tokio::test]
async fn a_truncated_tar_gz_keeps_its_index_while_a_missing_directory_empties() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("repo.tar.gz");
    let dir = tmp.path().join("dir");
    std::fs::create_dir(&dir)?;
    std::fs::write(dir.join("d.md"), "# D\n")?;
    let filler = "x".repeat(4096);
    let bytes = tar_gz_bytes(&[("a.md", filler.as_bytes()), ("b.md", filler.as_bytes())]);
    write_archive(&archive, &bytes, 0);
    ingest(&base, &[("repo", &archive), ("dir", &dir)], 0).await?;

    write_archive(&archive, &bytes[..bytes.len() / 2], 60);
    std::fs::remove_dir_all(&dir)?;
    let summary = ingest(&base, &[("repo", &archive), ("dir", &dir)], 0).await?;

    assert_eq!(summary.unreadable_collections, ["repo"]);
    assert_eq!(summary.removed, 1, "{summary:?}");
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("repo", "a.md"), ("repo", "b.md")])
    );
    Ok(())
}

/// Copy the rows of `(collection, from)` in `table` to `(collection, to)`,
/// as an earlier mdya that indexed `to` would have left them.
async fn copy_rows(base: &Path, table: &str, collection: &str, from: &str, to: &str) -> Result<()> {
    let db = mdya::store::connect(base.join("index")).await?;
    let table = db.open_table(table).execute().await?;
    let batches = table
        .query()
        .only_if(format!("collection = '{collection}' AND path = '{from}'"))
        .execute()
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    let mut copies = Vec::new();
    for batch in batches {
        let schema = batch.schema();
        let path_at = schema.index_of("path")?;
        let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
        columns[path_at] = Arc::new(StringArray::from(vec![to; batch.num_rows()]));
        copies.push(RecordBatch::try_new(schema, columns)?);
    }
    table.add(copies).execute().await?;
    Ok(())
}

/// A document already in the index under a name with a control character,
/// as an earlier mdya indexed it, is removed once the archive is read again:
/// its name is rejected, so it is not among the paths the archive lists.
#[tokio::test]
async fn an_indexed_document_whose_name_holds_a_control_character_is_removed() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha"), ("b.md", b"beta")]),
        0,
    );
    ingest(&base, &[("notes", &archive)], 0).await?;
    let odd = "c\u{1b}[31m.md";
    for table in [CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME] {
        copy_rows(&base, table, "notes", "b.md", odd).await?;
    }
    assert_eq!(indexed(&base, CHUNKS_TABLE_NAME).await?.len(), 3);

    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha"), ("b.md", b"beta"), (odd, b"gamma")]),
        60,
    );
    let summary = ingest(&base, &[("notes", &archive)], 0).await?;

    assert_eq!((summary.removed, summary.failed), (1, 1), "{summary:?}");
    let expected = pairs(&[("notes", "a.md"), ("notes", "b.md")]);
    assert_eq!(indexed(&base, CHUNKS_TABLE_NAME).await?, expected);
    assert_eq!(indexed(&base, SOURCES_TABLE_NAME).await?, expected);
    Ok(())
}

/// Runs a step when the archive's collection starts, which comes after the
/// archive is listed and before its documents are read.
struct OnCollectionStart<F>(F);

impl<F: Fn() + Send + Sync> IngestProgress for OnCollectionStart<F> {
    fn start_collection(&self, _name: &str, _total_files: usize) {
        (self.0)();
    }
    fn start_file(&self, _path: &Path) {}
    fn chunks_embedded(&self, _path: &Path, _done: usize, _total: usize) {}
    fn finish_file(&self, _path: &Path, _outcome: FileOutcome) {}
    fn start_index_maintenance(&self) {}
    fn finish(&self) {}
}

/// An archive that can no longer be read once its documents are listed:
/// the documents left unread fail and keep their previous rows.
#[tokio::test]
async fn documents_left_unread_when_the_archive_breaks_after_listing_keep_their_rows() -> Result<()>
{
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha"), ("b.md", b"beta")]),
        0,
    );
    ingest(&base, &[("notes", &archive)], 0).await?;

    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha 2"), ("b.md", b"beta 2")]),
        60,
    );
    let collections = BTreeMap::from([("notes".to_string(), archive.clone())]);
    let removed = archive.clone();
    let summary = update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder),
        Arc::new(OnCollectionStart(move || {
            std::fs::remove_file(&removed).expect("remove archive")
        })),
        0,
    )
    .await?;

    assert!(summary.unreadable_collections.is_empty(), "{summary:?}");
    assert_eq!(
        (
            summary.new,
            summary.updated,
            summary.failed,
            summary.removed
        ),
        (0, 0, 2, 0),
        "{summary:?}"
    );
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("notes", "a.md"), ("notes", "b.md")])
    );
    Ok(())
}

/// An archive rewritten once its documents are listed, still a zip with
/// the same names: nothing is read from it this run, the documents left
/// unread fail and keep their previous rows, and the next run reads it.
#[tokio::test]
async fn documents_left_unread_when_the_archive_is_rewritten_after_listing_keep_their_rows()
-> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let base = fresh_config_dir(&tmp).await?;
    let _lance_home = ScopedLanceLanguageModelHome::set(&lance_models_dir(&base));
    let archive = tmp.path().join("notes.zip");
    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha"), ("b.md", b"beta")]),
        0,
    );
    ingest(&base, &[("notes", &archive)], 0).await?;

    write_archive(
        &archive,
        &zip_bytes(&[("a.md", b"alpha 2"), ("b.md", b"beta 2")]),
        60,
    );
    let collections = BTreeMap::from([("notes".to_string(), archive.clone())]);
    let rewritten = archive.clone();
    let summary = update_all_collections(
        &collections,
        &base,
        Arc::new(MockEmbedder),
        Arc::new(OnCollectionStart(move || {
            write_archive(
                &rewritten,
                &zip_bytes(&[("a.md", b"alpha 3"), ("b.md", b"beta 3")]),
                120,
            )
        })),
        0,
    )
    .await?;

    assert!(summary.unreadable_collections.is_empty(), "{summary:?}");
    assert_eq!(
        (
            summary.new,
            summary.updated,
            summary.failed,
            summary.removed
        ),
        (0, 0, 2, 0),
        "{summary:?}"
    );
    assert_eq!(
        indexed(&base, CHUNKS_TABLE_NAME).await?,
        pairs(&[("notes", "a.md"), ("notes", "b.md")])
    );

    let summary = ingest(&base, &[("notes", &archive)], 0).await?;

    assert_eq!(
        (
            summary.new,
            summary.updated,
            summary.failed,
            summary.removed
        ),
        (0, 2, 0, 0),
        "{summary:?}"
    );
    Ok(())
}
