//! rmcp stdio smoke: `mdya init` a tempdir, spawn the built
//! binary in `mcp` mode, and talk to it as an MCP client over its
//! stdin/stdout. Verifies the real binary path (not just the library)
//! wires rmcp + tokio + stdio on every supported platform, and that the
//! `search` / `get_document` / `list_collections` tools are advertised.
//!
//! Stays download- and index-free: `mdya init` creates an empty `chunks`
//! table, and the one tool call uses an empty `query` so the engine's
//! `validate` rejects it (the `EmptyQuery` path runs before any FTS
//! index or embedder is touched). A populated query would need the
//! ~140 MB model and an INVERTED index, neither of which a transport
//! smoke should pay for — that surface is covered by
//! `smoke_mcp_search.rs` at the library layer.

use anyhow::Result;
use assert_cmd::Command as CliCommand;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::TokioChildProcess};
use serde_json::{Map, json};
use tempfile::TempDir;
use tokio::process::Command;

#[tokio::test]
async fn mcp_stdio_advertises_search_tools_and_surfaces_validation_error() -> Result<()> {
    let tmp = TempDir::new()?;
    let config_dir = tmp.path().to_str().expect("UTF-8 tempdir path");

    // `Server::new` opens the engine at startup, which requires the
    // `chunks` table + schema-metadata pin that `init` writes.
    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", config_dir, "init"])
        .assert()
        .success();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mdya"));
    cmd.args(["--config-dir", config_dir, "mcp"]);
    let transport = TokioChildProcess::new(cmd)?;

    let client = ().serve(transport).await?;

    let tools = client.list_all_tools().await?;
    let tool_names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in ["search", "get_document", "list_collections"] {
        assert!(
            tool_names.contains(&expected),
            "expected `{expected}` tool, got {tool_names:?}"
        );
    }
    assert!(
        !tool_names.contains(&"ping"),
        "ping placeholder should be gone, got {tool_names:?}"
    );

    // Empty query → the tool returns `Err(Json<McpToolError>)`,
    // which rmcp surfaces as a `CallToolResult` flagged `is_error` whose
    // `structured_content` carries `{ code, message, details }`; the same
    // JSON is mirrored in the text content for backwards compatibility.
    // `mode: "fts"` keeps this empty-query rejection embedder-free.
    let mut args = Map::new();
    args.insert("query".to_string(), json!(""));
    args.insert("mode".to_string(), json!("fts"));
    let result = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(args))
        .await?;
    assert_eq!(result.is_error, Some(true), "expected error result");
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured error content");
    assert_eq!(
        structured.get("code").and_then(|c| c.as_str()),
        Some("empty_query"),
        "expected structured error code over the transport, got: {structured}"
    );
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("query must be non-empty"),
        "expected validation message, got: {text}"
    );

    // An unknown `mode` value fails rmcp's argument deserialization
    // before the tool body runs. The deserialization failure surfaces as
    // a `CallToolResult` flagged `is_error` (carrying the validator
    // message back to the model), mirroring the empty-query path above,
    // rather than as a JSON-RPC protocol error on the wire.
    let mut bad_mode = Map::new();
    bad_mode.insert("query".to_string(), json!("release"));
    bad_mode.insert("mode".to_string(), json!("keyword"));
    let invalid_mode = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(bad_mode))
        .await?;
    assert_eq!(
        invalid_mode.is_error,
        Some(true),
        "unknown `mode` should produce a tool error result"
    );

    // Best-effort shutdown: propagating a JoinError here would mask the
    // smoke assertions above if the child cleanup happens to panic.
    let _ = client.cancel().await;
    Ok(())
}
