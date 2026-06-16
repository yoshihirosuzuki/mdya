//! Library-level smoke tests for `mcp::Server`'s `#[tool]` methods.
//! They drive the tools directly against a real LanceDB tempdir
//! populated by `update_all_collections` + `MockEmbedder`, bypassing
//! the stdio transport. The transport / `list_tools` / on-the-wire
//! error surface lives in `smoke_mcp_stdio.rs` (process layer); this
//! file pins the tool *logic*: `search` mode dispatch + echo + reachable
//! hits + the `k` → `limit` default, the validation →
//! `Err(Json<McpToolError>)` mapping, `get_document`, and the
//! `list_collections` introspection tool.
//!
//! `Server::with_seeded_embedder` pre-fills the embedder `OnceCell` with
//! `MockEmbedder`, so these tests never download the ~140 MB
//! `cl-nagoya/ruri-v3-30m` weights — the same deterministic-vector trick
//! `smoke_search_vector.rs` / `smoke_search_hybrid.rs` use.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use rmcp::Json;
use rmcp::handler::server::wrapper::Parameters;
use tempfile::TempDir;

use mdya::config::save;
use mdya::embedding::{EmbedError, Embedder};
use mdya::ingest::{NullProgress, update_all_collections};
use mdya::mcp::{GetDocumentRequest, McpErrorCode, McpToolError, SearchRequest, Server};
use mdya::search::{SearchEngine, SearchMode};
use mdya::store::lance_lm::lance_models_dir;

use common::{LANCE_ENV_LOCK, ScopedLanceLanguageModelHome};

const DEFAULT_MODEL_ID: &str = "cl-nagoya/ruri-v3-30m";
const DEFAULT_VECTOR_DIM: usize = 256;
const DEFAULT_VECTOR_DIM_I32: i32 = 256;

/// Unwrap a tool's success payload, panicking with the structured
/// [`McpToolError`] when the call failed. `rmcp::Json<T>` does not derive
/// `Debug` (rmcp 1.7), so `Result::expect` cannot render a tool result's
/// error branch directly — this keeps a failing success-path test readable.
fn tool_ok<T>(result: Result<Json<T>, Json<McpToolError>>) -> T {
    match result {
        Ok(Json(value)) => value,
        Err(Json(err)) => panic!("expected tool success, got error: {err:?}"),
    }
}

/// Same-vector mock for both query and passage so cosine distance ≈ 0;
/// mirrors `smoke_search_hybrid.rs::MockEmbedder` so vector / hybrid hits
/// are deterministically reachable without the real model.
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

/// Open an engine on a freshly ingested corpus and wrap it in a server
/// whose embedder slot is pre-seeded with `MockEmbedder`.
async fn seeded_server(tmp: &TempDir) -> Result<Server> {
    let (base, coll_dir) = fresh_corpus(tmp).await?;
    write_md(&coll_dir, "release.md", "# Release\n\nrelease checklist.\n");
    write_md(&coll_dir, "other.md", "# Other\n\nsomething unrelated.\n");
    ingest(&base, &coll_dir).await?;
    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    // Seeded embedder means the lazy-load path never runs, so the
    // model_cache_dir argument is never read. Pass a sentinel path that
    // would not parse as a real directory if the contract ever broke —
    // makes the "no filesystem touch" expectation visible to readers.
    Ok(Server::with_seeded_embedder(
        &base,
        Path::new(":seeded-embedder-no-cache-needed:"),
        Arc::new(engine),
        Arc::new(MockEmbedder),
    ))
}

fn req(query: &str, k: u32, collections: Vec<String>, mode: &str) -> SearchRequest {
    serde_json::from_value(serde_json::json!({
        "query": query,
        "k": k,
        "collections": collections,
        "mode": mode,
    }))
    .expect("valid MCP SearchRequest JSON")
}

// ---------- happy path: `search` dispatches on `mode` and reaches hits ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_with_fts_mode_returns_fts_envelope_with_hits() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .search(Parameters(req("release", 20, vec![], "fts")))
            .await,
    );

    assert_eq!(resp.mode, SearchMode::Fts);
    assert!(resp.total >= 1, "expected hits, got {resp:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_with_vector_mode_returns_vector_envelope_with_hits() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .search(Parameters(req("release", 20, vec![], "vector")))
            .await,
    );

    assert_eq!(resp.mode, SearchMode::Vector);
    assert!(resp.total >= 1, "expected hits, got {resp:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_with_hybrid_mode_returns_hybrid_envelope_with_hits() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .search(Parameters(req("release", 20, vec![], "hybrid")))
            .await,
    );

    assert_eq!(resp.mode, SearchMode::Hybrid);
    assert!(resp.total >= 1, "expected hits, got {resp:?}");
    Ok(())
}

// ---------- defaults: omitted `mode` runs hybrid, omitted `k` is 20 ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omitted_mode_defaults_to_hybrid_in_envelope() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    // Omitting `mode` exercises SearchMode's Default on deserialize; the
    // envelope's echoed `mode` proves the `search` tool dispatched to hybrid.
    let omitted: SearchRequest = serde_json::from_value(serde_json::json!({ "query": "release" }))?;
    assert_eq!(omitted.mode, SearchMode::Hybrid);
    let resp = tool_ok(server.search(Parameters(omitted)).await);
    assert_eq!(resp.mode, SearchMode::Hybrid);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn omitted_k_defaults_to_twenty_in_envelope() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    // Omitting `k` exercises `default_k()` on deserialize; the envelope's
    // `limit` proves the `k` → `limit` rename carried the default through.
    let omitted: SearchRequest = serde_json::from_value(serde_json::json!({ "query": "release" }))?;
    assert_eq!(omitted.k, 20);
    let resp = tool_ok(server.search(Parameters(omitted)).await);
    assert_eq!(resp.limit, 20);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_k_carries_into_limit() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .search(Parameters(req("release", 5, vec![], "fts")))
            .await,
    );
    assert_eq!(resp.limit, 5);
    Ok(())
}

// ---------- validation maps to structured McpToolError ----------
//
// The MCP error contract is `Result<_, Json<McpToolError>>`:
// a stable `code`, the source error's
// `Display` as `message`, and per-code `details`. The empty-query case is
// checked on all three modes to prove each dispatch path routes through the
// shared `validate`; the other two rules (zero limit, unknown collection)
// are checked once each — on a different mode — because the rule logic
// itself is engine-tested and re-running it per mode would only re-prove
// the routing.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_query_is_err_for_every_mode() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let fts = server
        .search(Parameters(req("   ", 20, vec![], "fts")))
        .await;
    let vector = server
        .search(Parameters(req("   ", 20, vec![], "vector")))
        .await;
    let hybrid = server
        .search(Parameters(req("   ", 20, vec![], "hybrid")))
        .await;
    for boxed in [
        fts.err().expect("fts rejects empty query"),
        vector.err().expect("vector rejects empty query"),
        hybrid.err().expect("hybrid rejects empty query"),
    ] {
        let err = boxed.0;
        assert_eq!(err.code, McpErrorCode::EmptyQuery);
        assert!(
            err.message.contains("query must be non-empty"),
            "got: {}",
            err.message
        );
        assert!(err.details.is_none(), "empty query needs no details");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_k_is_invalid_limit_error() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let err = server
        .search(Parameters(req("release", 0, vec![], "vector")))
        .await
        .err()
        .expect("zero k rejected")
        .0;
    assert_eq!(err.code, McpErrorCode::InvalidLimit);
    assert!(
        err.message.contains("limit must be >= 1"),
        "got: {}",
        err.message
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_collection_is_err_with_typo_hint() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let err = server
        .search(Parameters(req(
            "release",
            20,
            vec!["ghost".to_string()],
            "hybrid",
        )))
        .await
        .err()
        .expect("unknown collection rejected")
        .0;
    assert_eq!(err.code, McpErrorCode::UnknownCollection);
    assert!(
        err.message.contains("unknown collection: 'ghost'"),
        "got: {}",
        err.message
    );
    assert_eq!(
        err.details,
        Some(serde_json::json!({ "collection": "ghost" }))
    );
    Ok(())
}

// ---------- get_document: echoes locator + faithful content ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_echoes_locator_and_returns_faithful_content() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .get_document(Parameters(GetDocumentRequest {
                collection: "notes".to_string(),
                path: "release.md".to_string(),
                chunk: None,
            }))
            .await,
    );

    assert_eq!(resp.collection, "notes");
    assert_eq!(resp.path, "release.md");
    assert_eq!(resp.content, "# Release\n\nrelease checklist.\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_missing_path_maps_to_not_found_error() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let err = server
        .get_document(Parameters(GetDocumentRequest {
            collection: "notes".to_string(),
            path: "ghost.md".to_string(),
            chunk: None,
        }))
        .await
        .err()
        .expect("missing path rejected")
        .0;
    assert_eq!(err.code, McpErrorCode::NotFound);
    assert!(
        err.message.contains("document not found"),
        "got: {}",
        err.message
    );
    assert_eq!(
        err.details,
        Some(serde_json::json!({ "collection": "notes", "path": "ghost.md" }))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_with_chunk_returns_a_chunk_body_for_a_valid_sequence() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let resp = tool_ok(
        server
            .get_document(Parameters(GetDocumentRequest {
                collection: "notes".to_string(),
                path: "release.md".to_string(),
                chunk: Some(0),
            }))
            .await,
    );

    assert_eq!(resp.collection, "notes");
    assert_eq!(resp.path, "release.md");
    // chunk body is lossy vs the raw source; a substring is the read-path
    // assertion. The MCP envelope still echoes the locator unchanged.
    assert!(
        resp.content.contains("release checklist"),
        "chunk body should carry the section text: {:?}",
        resp.content
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_with_out_of_range_chunk_carries_chunk_sequence_in_details() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let err = server
        .get_document(Parameters(GetDocumentRequest {
            collection: "notes".to_string(),
            path: "release.md".to_string(),
            chunk: Some(9999),
        }))
        .await
        .err()
        .expect("out-of-range chunk_sequence rejected")
        .0;
    assert_eq!(err.code, McpErrorCode::NotFound);
    assert!(
        err.message.contains("chunk 9999 not found"),
        "got: {}",
        err.message
    );
    // `chunk_sequence` in details is the bit that lets a client tell
    // chunk-not-found apart from document-not-found.
    assert_eq!(
        err.details,
        Some(serde_json::json!({
            "collection": "notes",
            "path": "release.md",
            "chunk_sequence": 9999,
        }))
    );
    Ok(())
}

// ---------- get_document: output-size guard (get.mcp_max_bytes) ----------

/// Build a seeded server whose `get.mcp_max_bytes` is `mcp_max_bytes` and
/// whose corpus holds an over-cap `big.md`. Returns the server and the
/// document's exact bytes so a test can assert against the faithful content.
async fn seeded_server_with_mcp_cap(tmp: &TempDir, mcp_max_bytes: u64) -> Result<(Server, String)> {
    let (base, coll_dir) = fresh_corpus(tmp).await?;
    let cfg_path = base.join("config.yml");
    let mut cfg = mdya::config::load(&cfg_path)?;
    cfg.get.mcp_max_bytes = mcp_max_bytes;
    save(&cfg_path, &cfg)?;

    let big = format!("# Big\n\n{}\n", "lorem ipsum dolor ".repeat(8));
    write_md(&coll_dir, "big.md", &big);
    ingest(&base, &coll_dir).await?;

    let engine = SearchEngine::open(&base, DEFAULT_MODEL_ID, DEFAULT_VECTOR_DIM_I32).await?;
    let server = Server::with_seeded_embedder(
        &base,
        Path::new(":seeded-embedder-no-cache-needed:"),
        Arc::new(engine),
        Arc::new(MockEmbedder),
    );
    Ok((server, big))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_over_mcp_cap_is_payload_too_large_with_byte_details() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let (server, big) = seeded_server_with_mcp_cap(&tmp, 64).await?;

    let err = server
        .get_document(Parameters(GetDocumentRequest {
            collection: "notes".to_string(),
            path: "big.md".to_string(),
            chunk: None,
        }))
        .await
        .err()
        .expect("over-cap full document rejected")
        .0;
    assert_eq!(err.code, McpErrorCode::PayloadTooLarge);
    // The structured details let an LLM see exactly how far over it went and
    // decide to narrow the request (e.g. fetch a single chunk instead).
    assert_eq!(
        err.details,
        Some(serde_json::json!({ "size_bytes": big.len(), "limit_bytes": 64 }))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_chunk_path_ignores_the_mcp_cap() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    // Cap of 1 byte: the full-document path would fail, but a `chunk` read
    // must still succeed — chunk bodies are out of the guard's scope.
    let (server, _big) = seeded_server_with_mcp_cap(&tmp, 1).await?;

    let resp = tool_ok(
        server
            .get_document(Parameters(GetDocumentRequest {
                collection: "notes".to_string(),
                path: "big.md".to_string(),
                chunk: Some(0),
            }))
            .await,
    );
    assert!(
        resp.content.contains("lorem ipsum dolor"),
        "chunk body returned despite a 1-byte cap: {:?}",
        resp.content
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_document_with_mcp_cap_zero_returns_the_full_document() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    // `0` disables the cap, so an otherwise over-cap document comes back whole.
    let (server, big) = seeded_server_with_mcp_cap(&tmp, 0).await?;

    let resp = tool_ok(
        server
            .get_document(Parameters(GetDocumentRequest {
                collection: "notes".to_string(),
                path: "big.md".to_string(),
                chunk: None,
            }))
            .await,
    );
    assert_eq!(resp.content, big);
    Ok(())
}

// ---------- introspection: list_collections ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_collections_reports_notes_with_document_count() -> Result<()> {
    let _env_lock = LANCE_ENV_LOCK.lock().await;
    let tmp = TempDir::new()?;
    let _guard = ScopedLanceLanguageModelHome::set(&lance_models_dir(tmp.path()));
    let server = seeded_server(&tmp).await?;

    let report = tool_ok(server.list_collections().await);
    let notes = report
        .collections
        .iter()
        .find(|c| c.name == "notes")
        .expect("notes collection present");
    // seeded_server ingests release.md + other.md into `notes`.
    assert_eq!(notes.document_count, 2);
    Ok(())
}
