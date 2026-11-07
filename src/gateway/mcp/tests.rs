//! The MCP server over stdio, driven as an MCP client drives it: JSON-RPC
//! messages, one per line, over a pair of in-memory pipes in place of stdin
//! and stdout.

#![expect(
    clippy::result_large_err,
    reason = "the operation handlers return the operations' error, as the trait does"
)]

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadBuf,
};
use tokio::sync::oneshot;

use crate::query::{ChunkRangeRequest, SearchMode, SearchUnit};
use crate::types::{CollectionName, Key};

use super::super::error_report::test_double::{ReportCall, TestErrorReport};
use super::super::error_report::{Explanation, GatewayError, McpErrorCode};
use super::super::formatter;
use super::super::operations::test_double::TestOperations;
use super::super::operations::{
    CollectionEntry, CollectionList, DocumentText, OperationError, SafeInteger, SafeJsonObject,
    SearchLimit, SearchModeName, SearchOutput, SearchUnitName, version,
};
use super::server::INSTRUCTIONS;
use super::{McpError, McpServerParts, serve_stdio, socket_address};

/// The test doubles a server is made with, kept so that a test can look at
/// what they received.
struct Doubles {
    operations: Arc<TestOperations>,
    error_report: Arc<TestErrorReport>,
}

fn parts(operations: TestOperations) -> (McpServerParts, Doubles) {
    let operations = Arc::new(operations);
    let error_report = Arc::new(TestErrorReport::new());
    let parts = McpServerParts {
        operations: operations.clone(),
        show_version: true,
        size_limit: Some(1024),
        error_report: error_report.clone(),
    };
    (
        parts,
        Doubles {
            operations,
            error_report,
        },
    )
}

/// The client's ends of the pipes: it writes the server's stdin and reads the
/// server's stdout.
struct Client {
    stdin: DuplexStream,
    stdout: Lines<BufReader<DuplexStream>>,
}

/// What the server returned, once it has.
type Ended = oneshot::Receiver<Result<(), McpError>>;

/// Starts the server over a pair of pipes. The server runs on a thread of its
/// own, in a runtime that waits for it with `block_on`, as it is run for real:
/// its future cannot be spawned as a task.
fn start(parts: McpServerParts) -> (Client, Ended) {
    start_in_runtime(parts, false)
}

/// Starts the server as [`start`] does, with the clock of the server's
/// runtime paused: whenever the server has nothing to do, its clock moves on
/// to the next timer, so that a wait with a time limit inside rmcp ends at
/// once.
fn start_with_paused_clock(parts: McpServerParts) -> (Client, Ended) {
    start_in_runtime(parts, true)
}

fn start_in_runtime(parts: McpServerParts, paused_clock: bool) -> (Client, Ended) {
    let (stdin, server_reader) = tokio::io::duplex(1 << 16);
    let (server_writer, stdout) = tokio::io::duplex(1 << 16);
    let (end, ended) = oneshot::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(paused_clock)
            .build()
            .unwrap();
        let returned = runtime.block_on(serve_stdio(parts, server_reader, server_writer));
        let _ = end.send(returned);
    });
    let client = Client {
        stdin,
        stdout: BufReader::new(stdout).lines(),
    };
    (client, ended)
}

impl Client {
    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        serde_json::from_str(&self.receive_line().await).unwrap()
    }

    /// The next line the server wrote, as it wrote it.
    async fn receive_line(&mut self) -> String {
        self.stdout.next_line().await.unwrap().unwrap()
    }

    /// Initializes the server and returns the result of `initialize`.
    async fn initialize(&mut self) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }))
        .await;
        let response = self.receive().await;
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        response["result"].clone()
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        let response = self.receive().await;
        assert_eq!(response["id"], json!(id));
        response
    }

    /// Calls the tool and returns the result of the call.
    async fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let response = self
            .request(
                id,
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        response["result"].clone()
    }
}

fn collections() -> CollectionList {
    CollectionList {
        collections: vec![CollectionEntry {
            name: "notes".to_owned(),
            collection_type: "directory".to_owned(),
            description: None,
            document_count: SafeInteger::new(2),
            attributes: SafeJsonObject::new(serde_json::Map::new()),
        }],
    }
}

fn search_output() -> SearchOutput {
    SearchOutput {
        query: "q".to_owned(),
        mode: SearchModeName::Hybrid,
        unit: SearchUnitName::Document,
        collections: Vec::new(),
        limit: SearchLimit::new(20),
        hit_count: SafeInteger::new(0),
        hits: Vec::new(),
        warnings: Vec::new(),
    }
}

fn not_found() -> OperationError {
    OperationError::CollectionNotFound {
        collection: CollectionName("notes".to_owned()),
    }
}

/// The JSON that a successful call returns, both as structured content and
/// as text.
fn assert_success(result: &Value, expected: Value) {
    assert_eq!(result["isError"], json!(false));
    assert_eq!(result["structuredContent"], expected);
    assert_eq!(result["content"][0]["type"], json!("text"));
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, expected);
}

/// The error that a failed call returns, both as structured content and as
/// text.
fn assert_error(result: &Value, expected: Value) {
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["structuredContent"], expected);
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, expected);
}

/// The error that the server returns for `error`: what `formatter` makes of
/// the code and the explanation that the error report double gives for it.
fn test_error(error: &GatewayError) -> Value {
    let code = McpErrorCode {
        code: "test",
        details: serde_json::Map::new(),
    };
    made_error(code, &TestErrorReport::explanation_of(error))
}

/// The error that the server returns for a call that failed unexpectedly.
fn unexpected_failure() -> Value {
    let code = McpErrorCode {
        code: "internal",
        details: serde_json::Map::new(),
    };
    made_error(code, &TestErrorReport::panic_explanation_of())
}

/// The error as `formatter` makes it, in the object that the server returns.
fn made_error(code: McpErrorCode, explanation: &Explanation) -> Value {
    let error = formatter::mcp_tool_error(code, explanation).unwrap();
    json!({ "code": error.code, "message": error.message, "details": error.details })
}

#[tokio::test]
async fn initialize_returns_mdya_and_its_version_and_the_instructions() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);

    let result = client.initialize().await;

    assert_eq!(result["serverInfo"]["name"], json!("mdya"));
    assert_eq!(
        result["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(result["instructions"], json!(INSTRUCTIONS));
    assert!(result["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn without_showing_the_version_initialize_and_get_version_return_only_mdya() {
    let (mut parts, _) = parts(TestOperations::new());
    parts.show_version = false;
    let (mut client, _server) = start(parts);

    let info = client.initialize().await;
    let result = client.call(1, "get_version", json!({})).await;

    assert_eq!(info["serverInfo"]["version"], json!("mdya"));
    assert_success(&result, json!({ "version": "mdya" }));
}

#[tokio::test]
async fn get_version_returns_mdya_and_its_version() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client.call(1, "get_version", json!({})).await;

    assert_success(&result, json!({ "version": version().version }));
}

#[tokio::test]
async fn the_list_of_tools_has_the_four_tools_with_their_schemas() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let response = client.request(1, "tools/list", json!({})).await;

    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["list_collections", "search", "get_document", "get_version"]
    );
    for tool in tools {
        assert!(tool["description"].is_string());
        assert_eq!(tool["inputSchema"]["type"], json!("object"));
        assert_eq!(tool["outputSchema"]["type"], json!("object"));
    }
}

#[tokio::test]
async fn list_collections_returns_the_list() {
    let (parts, doubles) = parts(TestOperations::new().on_list_collections(|_| Ok(collections())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client.call(1, "list_collections", json!({})).await;

    assert_success(&result, serde_json::to_value(collections()).unwrap());
    assert_eq!(doubles.operations.received().list_collections.len(), 1);
}

#[tokio::test]
async fn search_without_the_optional_inputs_takes_their_defaults() {
    let (parts, doubles) = parts(TestOperations::new().on_search(|_| Ok(search_output())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client.call(1, "search", json!({ "query": "q" })).await;

    assert_success(&result, serde_json::to_value(search_output()).unwrap());
    let received = doubles.operations.received();
    let input = &received.search[0];
    assert_eq!(input.query, "q");
    assert_eq!(input.mode, SearchMode::Hybrid);
    assert!(input.collections.is_empty());
    assert_eq!(input.limit, SearchLimit::new(20));
    assert_eq!(input.unit, SearchUnit::Document);
}

#[tokio::test]
async fn search_passes_on_the_inputs_given() {
    let (parts, doubles) = parts(TestOperations::new().on_search(|_| Ok(search_output())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    client
        .call(
            1,
            "search",
            json!({
                "query": "q",
                "mode": "fts",
                "collections": ["notes", "work"],
                "limit": 5,
                "unit": "chunk"
            }),
        )
        .await;

    let received = doubles.operations.received();
    let input = &received.search[0];
    assert_eq!(input.mode, SearchMode::FullText);
    assert_eq!(
        input.collections,
        [
            CollectionName("notes".to_owned()),
            CollectionName("work".to_owned())
        ]
    );
    assert_eq!(input.limit, SearchLimit::new(5));
    assert_eq!(input.unit, SearchUnit::Chunk);
}

#[tokio::test]
async fn get_document_passes_on_the_range_and_the_mcp_output_size_limit() {
    let text = DocumentText {
        collection: "notes".to_owned(),
        key: "a.md".to_owned(),
        text: "body".to_owned(),
    };
    let returned = text.clone();
    let (parts, doubles) =
        parts(TestOperations::new().on_get_document(move |_| Ok(returned.clone())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client
        .call(
            1,
            "get_document",
            json!({ "collection": "notes", "key": "a.md", "chunk_index": 2, "end_chunk_index": 4 }),
        )
        .await;

    assert_success(&result, serde_json::to_value(text).unwrap());
    let received = doubles.operations.received();
    let input = &received.get_document[0];
    assert_eq!(input.collection, CollectionName("notes".to_owned()));
    assert_eq!(input.key, Key("a.md".to_owned()));
    assert_eq!(
        input.range,
        ChunkRangeRequest {
            start: Some(2),
            end: Some(4)
        }
    );
    assert_eq!(input.size_limit, Some(1024));
}

#[tokio::test]
async fn an_operation_error_is_returned_as_the_error_report_shows_it() {
    let (parts, doubles) = parts(TestOperations::new().on_list_collections(|_| Err(not_found())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client.call(1, "list_collections", json!({})).await;

    let error = GatewayError::Operation(not_found());
    assert_error(&result, test_error(&error));
    assert_eq!(
        doubles.error_report.calls(),
        vec![
            ReportCall::McpError(format!("{error:?}")),
            ReportCall::Explanation(format!("{error:?}"))
        ]
    );
}

#[tokio::test]
async fn an_input_that_does_not_fit_is_returned_as_the_error_report_shows_it() {
    let (parts, doubles) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client.call(1, "search", json!({ "query": 1 })).await;

    let error = GatewayError::Mcp(McpError::InvalidToolInput {
        input: "query".to_owned(),
        detail: "it is not a string".to_owned(),
    });
    assert_error(&result, test_error(&error));
    assert_eq!(
        doubles.error_report.calls(),
        vec![
            ReportCall::McpError(format!("{error:?}")),
            ReportCall::Explanation(format!("{error:?}"))
        ]
    );
    assert!(doubles.operations.received().search.is_empty());
}

#[tokio::test]
async fn a_limit_of_zero_is_returned_as_the_error_report_shows_it_without_searching() {
    let (parts, doubles) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client
        .call(1, "search", json!({ "query": "q", "limit": 0 }))
        .await;

    assert_error(&result, test_error(&GatewayError::Mcp(McpError::ZeroLimit)));
    assert!(doubles.operations.received().search.is_empty());
}

#[tokio::test]
async fn get_document_refuses_an_input_that_does_not_fit() {
    let (parts, doubles) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let result = client
        .call(1, "get_document", json!({ "collection": "notes" }))
        .await;

    assert_eq!(result["isError"], json!(true));
    assert!(doubles.operations.received().get_document.is_empty());
}

#[tokio::test]
async fn a_tool_without_inputs_refuses_an_input() {
    let (parts, doubles) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let collections = client.call(1, "list_collections", json!({ "x": 1 })).await;
    let version = client.call(2, "get_version", json!({ "x": 1 })).await;

    assert_eq!(collections["isError"], json!(true));
    assert_eq!(version["isError"], json!(true));
    assert!(doubles.operations.received().list_collections.is_empty());
}

#[tokio::test]
async fn a_tool_called_without_arguments_has_no_inputs() {
    let (parts, _) = parts(TestOperations::new().on_list_collections(|_| Ok(collections())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let response = client
        .request(1, "tools/call", json!({ "name": "list_collections" }))
        .await;

    assert_eq!(response["result"]["isError"], json!(false));
}

#[tokio::test]
async fn an_unknown_tool_is_a_json_rpc_error() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let response = client
        .request(1, "tools/call", json!({ "name": "delete_everything" }))
        .await;

    assert_eq!(response["error"]["code"], json!(-32602));
    assert_eq!(
        response["error"]["message"],
        json!("there is no tool of this name")
    );
    assert!(response.get("result").is_none());
}

#[tokio::test]
async fn a_tool_call_that_panics_fails_alone_and_the_server_goes_on() {
    let (parts, doubles) = parts(
        TestOperations::new()
            .on_list_collections(|_| panic!("a bug in a tool call"))
            .on_search(|_| Ok(search_output())),
    );
    let (mut client, _server) = start(parts);
    client.initialize().await;

    let panicked = client.call(1, "list_collections", json!({})).await;
    let next = client.call(2, "search", json!({ "query": "q" })).await;

    assert_error(&panicked, unexpected_failure());
    let calls = doubles.error_report.calls();
    assert!(calls.contains(&ReportCall::McpPanicError));
    assert!(calls.contains(&ReportCall::PanicExplanation));
    assert_eq!(next["isError"], json!(false));
}

#[tokio::test]
async fn the_server_ends_without_an_error_when_the_client_closes_stdin_after_starting() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, server) = start(parts);
    client.initialize().await;

    drop(client.stdin);

    assert!(server.await.unwrap().is_ok());
}

#[tokio::test]
async fn the_exchange_cannot_start_when_the_client_closes_stdin_first() {
    let (parts, _) = parts(TestOperations::new());
    let (client, server) = start(parts);

    drop(client.stdin);

    assert!(matches!(
        server.await.unwrap(),
        Err(McpError::Transport { .. })
    ));
}

#[tokio::test]
async fn the_exchange_cannot_start_when_a_notification_comes_first() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, server) = start(parts);

    client
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    assert!(matches!(
        server.await.unwrap(),
        Err(McpError::Transport { .. })
    ));
}

#[tokio::test]
async fn the_server_ends_without_an_error_when_stdout_is_closed_after_starting() {
    let (parts, _) = parts(TestOperations::new());
    let (mut client, server) = start(parts);
    client.initialize().await;
    let Client { mut stdin, stdout } = client;

    drop(stdout);
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n")
        .await
        .unwrap();
    drop(stdin);

    assert!(server.await.unwrap().is_ok());
}

#[tokio::test]
async fn the_server_waits_for_the_tool_calls_in_progress_before_it_returns() {
    let (release, released) = oneshot::channel::<()>();
    let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
    let (started, has_started) = oneshot::channel::<()>();
    let started = Arc::new(Mutex::new(Some(started)));
    let (parts, doubles) = parts(TestOperations::new().on_search_async(move |_| {
        let released = released.clone();
        let started = started.clone();
        Box::pin(async move {
            if let Some(started) = started.lock().unwrap().take() {
                started.send(()).unwrap();
            }
            if let Some(released) = released.lock().await.take() {
                released.await.unwrap();
            }
            Ok(search_output())
        })
    }));
    // rmcp itself waits a while for the calls in progress once stdin is closed
    // (5 seconds in rmcp 3.9.0). With the server's clock paused, that wait
    // ends at once, so that only the server's own waiting holds it back.
    let (mut client, mut server) = start_with_paused_clock(parts);
    client.initialize().await;
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "search", "arguments": { "query": "q" } }
        }))
        .await;
    has_started.await.unwrap();

    drop(client.stdin);
    // Long enough for the server to see stdin closed and end the exchange.
    // Were it not to wait for the call, it would have returned by then.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        matches!(server.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
        "the server returned before the tool call ended"
    );
    release.send(()).unwrap();
    let ended = server.await.unwrap();

    assert!(ended.is_ok());
    assert_eq!(doubles.operations.received().search.len(), 1);
}

/// A text with characters that the human-readable output would replace: ESC,
/// which JSON escapes, and DEL, a C1 control, and a character that changes
/// the direction of text, which JSON leaves as they are.
const TEXT: &str = "a\u{1b}b\u{7f}c\u{9b}d\u{202e}e";

// The line carries the result twice, as structured content and as text, and
// JSON escapes ESC as `\u001b` in both. Nothing else is escaped.
#[tokio::test]
async fn the_strings_in_a_result_reach_stdout_with_only_jsons_own_escaping() {
    let text = DocumentText {
        collection: "notes".to_owned(),
        key: "a.md".to_owned(),
        text: TEXT.to_owned(),
    };
    let (parts, _) = parts(TestOperations::new().on_get_document(move |_| Ok(text.clone())));
    let (mut client, _server) = start(parts);
    client.initialize().await;

    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "get_document", "arguments": { "collection": "notes", "key": "a.md" } }
        }))
        .await;
    let line = client.receive_line().await;

    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["result"]["structuredContent"]["text"], json!(TEXT));
    assert!(line.contains("\u{7f}c\u{9b}d\u{202e}e"));
    assert!(line.contains(&format!("a{0}u001bb", '\\')));
    for escaped in ["u007f", "u009b", "u202e"] {
        assert!(!line.contains(&format!("{}{escaped}", '\\')), "{escaped}");
    }
}

/// Gives the bytes it holds, then panics when it is read again, as a reader
/// with a bug would.
struct PanickingReader {
    bytes: Vec<u8>,
    read: usize,
}

impl AsyncRead for PanickingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read == self.bytes.len() {
            panic!("a bug in reading stdin");
        }
        let end = self.bytes.len().min(self.read + buf.remaining());
        buf.put_slice(&self.bytes[self.read..end]);
        self.read = end;
        Poll::Ready(Ok(()))
    }
}

// rmcp reads the client's messages in the task that it runs the exchange in,
// once the exchange has started: the reader panics there.
#[test]
fn a_panic_in_the_exchange_outside_the_tool_calls_is_passed_on_as_a_panic() {
    let (parts, _) = parts(TestOperations::new());
    let mut handshake = String::new();
    for message in [
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ] {
        handshake.push_str(&format!("{message}\n"));
    }
    let reader = PanickingReader {
        bytes: handshake.into_bytes(),
        read: 0,
    };
    // The client's end is kept open, so that the server's answer is written.
    let (writer, _client_end) = tokio::io::duplex(1 << 16);

    let ended = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(serve_stdio(parts, reader, writer))
    })
    .join();

    let panic = ended.unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"a bug in reading stdin")
    );
}

fn unavailable(host: &str) -> McpError {
    McpError::AddressUnavailable {
        host: host.to_owned(),
        port: 8000,
        source: io::Error::new(io::ErrorKind::AddrInUse, "address in use"),
    }
}

#[test]
fn an_unavailable_address_is_shown_with_only_an_ipv6_host_in_brackets() {
    assert_eq!(
        unavailable("::1").to_string(),
        "cannot listen on [::1]:8000"
    );
    assert_eq!(
        unavailable("127.0.0.1").to_string(),
        "cannot listen on 127.0.0.1:8000"
    );
    assert_eq!(socket_address("localhost", 80), "localhost:80");
}
