//! Full CLI golden-path E2E: drives the real `mdya` binary through
//! `init` → `collection add` → `update-all` → `search fts/vector/hybrid`
//! → `mcp`, asserting each stage on a single ingested corpus. This is
//! the one test that proves the loop holds together end-to-end through
//! the binary, not just per-stage in library smoke.
//!
//! Marked `#[ignore]` because `update-all` and `search vector/hybrid`
//! download the real `cl-nagoya/ruri-v3-30m` model (~140 MB) on first
//! run — the binary has no embedder override (verified: the embedder is
//! hard-wired to `RuriV3_30m` in every CLI path), so a fixture-backed
//! fast variant is impossible without an out-of-scope feature. Run via
//! `just full-flow-e2e`; not part of `just check`.
//!
//! NOTE: `--config-dir` points at a fresh `TempDir`, so the model cache
//! lives inside it and is re-downloaded every run (same trade-off as
//! `tests/e2e_update_all.rs`, keeping the user's home cache untouched).
//! Within a single run the model is fetched once by `update-all`; the
//! later `search` processes reuse that tempdir cache.

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use predicates::prelude::*;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::{Map, json};
use tempfile::TempDir;
use tokio::process::Command;

use mdya::search::{SearchMode, SearchResponse};

const QUERY: &str = "release";

fn mdya(config_dir: &str) -> CliCommand {
    let mut cmd = CliCommand::cargo_bin("mdya").expect("binary built");
    cmd.args(["--config-dir", config_dir]);
    cmd
}

// A plain `#[test]` (like `e2e_update_all.rs`): the CLI stages are
// synchronous `assert_cmd` calls, so the blocking model download never
// runs on a tokio worker. Only the MCP stage needs async, so it gets a
// dedicated runtime via `block_on` rather than making the whole test
// async and blocking the executor on every `assert_cmd` call.
#[test]
#[ignore = "downloads ~140 MB model; run with `just full-flow-e2e`"]
fn golden_path_init_to_mcp_through_real_binary() -> Result<()> {
    let tmp = TempDir::new()?;
    let base = tmp.path();
    let config_dir = base.to_str().expect("UTF-8 tempdir path");
    let coll_dir = base.join("notes");
    std::fs::create_dir(&coll_dir)?;
    std::fs::write(
        coll_dir.join("release.md"),
        "# Release\n\nThe release checklist covers the steps before a release.\n",
    )?;

    // 1. init → config + empty chunks table materialised.
    mdya(config_dir).arg("init").assert().success();
    assert!(base.join("config.yml").is_file(), "config.yml not created");
    let chunks_table = base.join("index").join("chunks.lance");
    assert!(
        chunks_table.exists(),
        "chunks table not created at {}",
        chunks_table.display()
    );

    // 2. collection add → registered in config.yml.
    mdya(config_dir)
        .args(["collection", "add"])
        .arg(&coll_dir)
        .assert()
        .success();

    // 3. update-all → ingest summary for the one new document.
    mdya(config_dir)
        .arg("update-all")
        .assert()
        .success()
        .stdout(predicate::str::contains("Indexed 1 documents"))
        .stdout(predicate::str::contains("new: 1"));

    // 4-6. each search mode returns at least one hit. `score=` only
    // appears on a hit header (zero hits emit the footer alone), so it is
    // a mode-agnostic "got a hit" assertion across fts/vector/hybrid.
    for mode in ["fts", "vector", "hybrid"] {
        mdya(config_dir)
            .args(["search", mode, QUERY])
            .assert()
            .success()
            .stdout(predicate::str::contains("score="));
    }

    // 7. the MCP server, on the same ingested corpus, advertises the
    // `search` tool and answers a `mode: "fts"` search call with a hit.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(assert_mcp_lists_tools_and_answers_fts(config_dir))?;
    Ok(())
}

async fn assert_mcp_lists_tools_and_answers_fts(config_dir: &str) -> Result<()> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mdya"));
    cmd.args(["--config-dir", config_dir, "mcp"]);
    let client = ().serve(TokioChildProcess::new(cmd)?).await?;

    let tools = client.list_all_tools().await?;
    let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in ["search", "get_document", "list_collections"] {
        assert!(names.contains(&expected), "missing `{expected}`: {names:?}");
    }

    let mut args = Map::new();
    args.insert("query".to_string(), json!(QUERY));
    args.insert("mode".to_string(), json!("fts"));
    let result = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(args))
        .await?;
    assert_eq!(result.is_error, Some(false), "search call errored");
    let resp = parse_search_response(result.structured_content.as_ref());
    assert_eq!(resp.mode, SearchMode::Fts);
    assert!(!resp.hits.is_empty(), "expected a hit, got {resp:?}");

    // Best-effort shutdown; a cleanup panic must not mask the assertions.
    let _ = client.cancel().await;
    Ok(())
}

fn parse_search_response(structured: Option<&serde_json::Value>) -> SearchResponse {
    let value = structured.expect("search tool returns structured_content");
    serde_json::from_value(value.clone()).expect("structured_content is a SearchResponse")
}
