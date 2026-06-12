//! rmcp Streamable HTTP smoke: `mdya init` a tempdir, spawn
//! the built binary in `mcp --http` mode on an ephemeral port, discover the
//! bound port from its stderr, and talk to it as an MCP client over HTTP.
//!
//! Mirrors `smoke_mcp_stdio.rs`: this proves the real binary wires rmcp, axum,
//! and Streamable HTTP and advertises the three `search_*` tools. Tool
//! correctness is covered at the library layer (`smoke_mcp_search.rs`), so this
//! stays download- and index-free — `mdya init` leaves an empty `chunks` table
//! and the one tool call uses an empty `query` that the engine rejects before
//! any FTS index or embedder is touched.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use assert_cmd::Command as CliCommand;
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::StreamableHttpClientTransport};
use serde_json::{Map, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn mcp_http_advertises_search_tools_and_surfaces_validation_error() -> Result<()> {
    let tmp = TempDir::new()?;
    let config_dir = tmp.path().to_str().expect("UTF-8 tempdir path");

    // `Server::new` opens the engine at startup, which requires the `chunks`
    // table + schema-metadata pin that `init` writes.
    CliCommand::cargo_bin("mdya")?
        .args(["--config-dir", config_dir, "init"])
        .assert()
        .success();

    // Bind port 0 so the OS picks a free port; the daemon logs the real
    // address, which we parse back out of its stderr below.
    let mut child = Command::new(env!("CARGO_BIN_EXE_mdya"))
        .args([
            "--config-dir",
            config_dir,
            "mcp",
            "--http",
            "--addr",
            "127.0.0.1:0",
        ])
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = child.stderr.take().expect("piped stderr");
    let base_url = read_listening_url(stderr).await?;

    let transport = StreamableHttpClientTransport::from_uri(base_url);
    let client = ().serve(transport).await?;

    let tools = client.list_all_tools().await?;
    let tool_names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in ["search", "get_document", "list_collections"] {
        assert!(
            tool_names.contains(&expected),
            "expected `{expected}` tool, got {tool_names:?}"
        );
    }

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
        "expected structured error code over HTTP, got: {structured}"
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

    let _ = client.cancel().await;
    // SIGKILL is fine for teardown: the OS releases the pid-file lock, and the
    // leftover pid file lives under the tempdir that `tmp` removes on drop.
    child.start_kill()?;
    let _ = child.wait().await;
    Ok(())
}

/// Read the daemon's stderr until its `listening on http://HOST:PORT/mcp` line
/// and return that URL. Bounded by a deadline so a daemon that never binds
/// fails the test instead of hanging it.
async fn read_listening_url(stderr: impl AsyncRead + Unpin) -> Result<String> {
    let mut lines = BufReader::new(stderr).lines();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => bail!("daemon did not log a listening URL within 30s"),
            line = lines.next_line() => match line? {
                Some(line) => {
                    if let Some(url) = extract_mcp_url(&line) {
                        return Ok(url);
                    }
                }
                None => bail!("daemon stderr closed before it logged a listening URL"),
            },
        }
    }
}

/// Pull the `http://.../mcp` endpoint out of a tracing log line.
fn extract_mcp_url(line: &str) -> Option<String> {
    let start = line.find("http://")?;
    let rest = &line[start..];
    let end = rest.find("/mcp")? + "/mcp".len();
    Some(rest[..end].to_string())
}
