//! Integration tests of the MCP server: the mdya executable runs as a
//! separate process, started as an MCP client starts it, and the tests talk
//! MCP to it over stdio or over HTTP.

// `allow-unwrap-in-tests` and `allow-panic-in-tests` in clippy.toml cover
// test functions and `#[cfg(test)]` code, which the helpers below, the MCP
// client of the tests, are not.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::{Outcome, Workspace};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The MCP version the tests ask for: one that rmcp, the MCP library of the
/// server, knows.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How long a test waits for one answer, long enough for a call that loads
/// the real embedding model.
const ANSWER_WAIT: Duration = Duration::from_secs(120);

const RELEASE: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";
const TAGS: &str =
    "# タグの付け方\n\nタグの名前は、版の番号にします。\n\n# 例\n\nv1.2.3 のように書きます。\n";
const MEMO: &str = "# 覚え書き\n\n古いタグは消さずに残します。\n";

/// The mdya MCP server over stdio, as an MCP client runs it: its stdin and
/// stdout carry the exchange.
struct StdioServer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<io::Result<String>>,
    stderr: Option<JoinHandle<String>>,
    next_id: u64,
}

/// How a server over stdio ended.
struct Ended {
    status: ExitStatus,
    /// The lines that the server wrote on stdout after the last answer the
    /// test waited for.
    stdout: Vec<String>,
    stderr: String,
}

impl StdioServer {
    /// Starts `mdya mcp` from `command`, the executable with the test's
    /// directories.
    fn start(mut command: Command) -> Self {
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = lines_of(child.stdout.take().unwrap());
        let mut stderr = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            stderr.read_to_string(&mut text).unwrap();
            text
        });
        Self {
            child,
            stdin,
            stdout,
            stderr: Some(stderr),
            next_id: 1,
        }
    }

    /// Starts the exchange as an MCP client does, and returns the result of
    /// `initialize`.
    fn initialize(&mut self) -> Value {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "mdya-integration-tests", "version": "0" }
            }),
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        result
    }

    /// Calls `tool` and returns its result.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// Sends a request and returns the result of the answer with its id.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let (_, answer) = self.request_with_line(method, params);
        answer
    }

    /// Sends a request and returns the line of the answer with its id, as the
    /// server wrote it, and the answer's result. Each line that the server
    /// writes on stdout meanwhile must be a JSON-RPC message.
    fn request_with_line(&mut self, method: &str, params: Value) -> (String, Value) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            let line = match next_line(&self.stdout, deadline) {
                Ok(line) => line,
                Err(error) => panic!("no answer to {method}: {error}"),
            };
            let message = mcp_message(&line);
            if message["id"] == json!(id) {
                let result = message
                    .get("result")
                    .unwrap_or_else(|| panic!("{method} failed: {message}"))
                    .clone();
                return (line, result);
            }
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Closes stdin, as an MCP client does to end the exchange, and waits for
    /// the server to end.
    fn finish(&mut self) -> Ended {
        drop(self.stdin.take());
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut stdout = Vec::new();
        loop {
            match next_line(&self.stdout, deadline) {
                Ok(line) => stdout.push(line),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the server did not end"),
            }
        }
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the server did not end");
            std::thread::sleep(Duration::from_millis(50));
        };
        // The thread ends when the server's stderr closes, which it does as
        // the server ends.
        let stderr = self.stderr.take().unwrap().join().unwrap();
        Ended {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for StdioServer {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// The mdya MCP server over HTTP, listening on a port that the OS chose.
struct HttpServer {
    child: Child,
    /// Where the server said it listens, `127.0.0.1:<port>`.
    address: String,
    /// The lines of stderr, read on a thread of their own until the server
    /// ends, so that the server never waits to write there.
    stderr: Receiver<io::Result<String>>,
}

impl HttpServer {
    /// Starts `mdya mcp --http` from `command`, the executable with the test's
    /// directories, and reads where it listens from its message on stderr.
    fn start(mut command: Command) -> Self {
        let mut child = command
            .args(["mcp", "--http", "--address", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = lines_of(child.stderr.take().unwrap());
        // Owned before anything can fail, so that it is stopped if the test
        // fails here.
        let mut server = Self {
            child,
            address: String::new(),
            stderr,
        };
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut before = Vec::new();
        server.address = loop {
            let line = match next_line(&server.stderr, deadline) {
                Ok(line) => line,
                Err(error) => panic!("the server did not listen: {error}: {before:?}"),
            };
            if let Some(url) = line.strip_prefix("Listening on http://") {
                break url.strip_suffix("/mcp").unwrap().to_owned();
            }
            before.push(line);
        };
        server
    }

    /// Posts `message` to the MCP path, with `headers` besides the ones that
    /// MCP asks for.
    fn post(&self, message: &Value, headers: &[(&str, &str)]) -> HttpAnswer {
        let body = message.to_string();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nConnection: close\r\n\
             Content-Length: {}\r\n",
            body.len()
        );
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            request.push_str(&format!("Host: {}\r\n", self.address));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&body);
        let deadline = Instant::now() + ANSWER_WAIT;
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        // Reads until the server closes the connection, as `Connection: close`
        // asks, waiting no later than the deadline in all.
        let mut answer = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "no whole answer in time: {answer:?}");
            stream.set_read_timeout(Some(left)).unwrap();
            match stream.read(&mut buffer).unwrap() {
                0 => break,
                read => answer.extend_from_slice(&buffer[..read]),
            }
        }
        HttpAnswer::parse(&answer)
    }

    /// Starts an exchange, and returns the result of `initialize` and the id
    /// of the session that the server gave.
    fn initialize(&self, headers: &[(&str, &str)]) -> (Value, String) {
        let answer = self.post(&initialize_request(), headers);
        assert_eq!(answer.status, 200, "{answer:?}");
        assert_eq!(
            answer.header("content-type"),
            Some("text/event-stream"),
            "{answer:?}"
        );
        let session = answer.header("mcp-session-id").unwrap().to_owned();
        let initialized = self.post(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            &[("Mcp-Session-Id", &session)],
        );
        assert_eq!(initialized.status, 202, "{initialized:?}");
        (answer.result(), session)
    }

    /// Ends the server and returns what it wrote on stdout.
    fn stop(mut self) -> String {
        stop(&mut self.child);
        let mut stdout = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        stdout
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// What the server answered to one HTTP request.
#[derive(Debug)]
struct HttpAnswer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpAnswer {
    fn parse(answer: &[u8]) -> Self {
        let end_of_head = find(answer, b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&answer[..end_of_head]).unwrap();
        let body = &answer[end_of_head + 4..];
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap().split(' ').nth(1).unwrap();
        let headers: Vec<(String, String)> = lines
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.trim().to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        let chunked = headers
            .iter()
            .any(|(name, value)| name == "transfer-encoding" && value == "chunked");
        // A chunk's size counts bytes, and a chunk may end inside a character,
        // so the body is text only once its chunks are joined.
        let body = if chunked {
            unchunk(body)
        } else {
            body.to_vec()
        };
        Self {
            status: status.parse().unwrap(),
            headers,
            body: String::from_utf8(body).unwrap(),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The result of the JSON-RPC answer in the server-sent events of the
    /// body.
    fn result(&self) -> Value {
        let message = self
            .body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .find(|data| !data.is_empty())
            .map(mcp_message)
            .unwrap_or_else(|| panic!("no message in {self:?}"));
        message
            .get("result")
            .unwrap_or_else(|| panic!("the request failed: {message}"))
            .clone()
    }
}

/// The body of a response sent in chunks, without the lines of the chunks'
/// sizes and their extensions.
fn unchunk(mut body: &[u8]) -> Vec<u8> {
    let mut whole = Vec::new();
    loop {
        let end_of_line = find(body, b"\r\n").unwrap();
        let line = std::str::from_utf8(&body[..end_of_line]).unwrap();
        let size = line.split(';').next().unwrap().trim();
        let size = usize::from_str_radix(size, 16).unwrap();
        if size == 0 {
            return whole;
        }
        let rest = &body[end_of_line + 2..];
        whole.extend_from_slice(&rest[..size]);
        body = &rest[size + 2..];
    }
}

/// Where `needle` first begins in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "mdya-integration-tests", "version": "0" }
        }
    })
}

/// A line that a server wrote on stdout, which must be a JSON-RPC message.
fn mcp_message(line: &str) -> Value {
    let message: Value = serde_json::from_str(line)
        .unwrap_or_else(|error| panic!("not a JSON-RPC message: {line:?}: {error}"));
    assert_eq!(message["jsonrpc"], "2.0", "{line}");
    message
}

/// Reads `output` line by line on a thread of its own, so that a test can
/// stop waiting for a line. A line that cannot be read, such as one that is
/// not UTF-8, is passed on as an error, and the reading stops there.
fn lines_of(output: impl Read + Send + 'static) -> Receiver<io::Result<String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let unreadable = line.is_err();
            if sender.send(line).is_err() || unreadable {
                break;
            }
        }
    });
    receiver
}

/// The next line from `lines`, waiting no later than `deadline`. A line that
/// could not be read fails the test.
fn next_line(
    lines: &Receiver<io::Result<String>>,
    deadline: Instant,
) -> Result<String, RecvTimeoutError> {
    lines
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map(|line| line.unwrap_or_else(|error| panic!("a line cannot be read: {error}")))
}

fn stop(child: &mut Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The structured content of a tool's successful result, which its text
/// content must hold too.
fn succeeded(result: &Value) -> Value {
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        text_content(result),
        result["structuredContent"],
        "{result}"
    );
    result["structuredContent"].clone()
}

/// The error of a tool's failed result, which its text content must hold too.
fn failed(result: &Value) -> Value {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        text_content(result),
        result["structuredContent"],
        "{result}"
    );
    result["structuredContent"].clone()
}

fn text_content(result: &Value) -> Value {
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{result}");
    assert_eq!(content[0]["type"], "text", "{result}");
    serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap()
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, std::process::ExitCode::SUCCESS, "{outcome:?}");
}

/// The result of `mdya <args> --json`, which must succeed.
fn json_of(workspace: &Workspace, args: &[&str]) -> Value {
    let mut with_json = args.to_vec();
    with_json.push("--json");
    let outcome = workspace.run(&with_json);
    assert_success(&outcome);
    serde_json::from_str(&outcome.stdout).unwrap()
}

/// What `mdya <args>` writes on stderr when it fails, without its last line
/// break: the MCP tools give the same in their error's message.
fn cli_error(workspace: &Workspace, args: &[&str]) -> String {
    let outcome = workspace.run(args);
    assert_ne!(outcome.code, std::process::ExitCode::SUCCESS, "{outcome:?}");
    outcome.stderr.trim_end_matches('\n').to_owned()
}

fn raw_collection(workspace: &Workspace, name: &str, documents: &[(&str, &str)]) {
    assert_success(&workspace.run(&["collection", "add", name, "--type", "raw"]));
    for &(key, contents) in documents {
        put(workspace, name, key, contents);
    }
}

fn put(workspace: &Workspace, collection: &str, key: &str, contents: &str) {
    assert_success(&workspace.run_with_stdin(
        &["put", collection, key, "--file-type", "md", "-"],
        contents.as_bytes(),
    ));
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn over_stdio_the_server_gives_its_name_version_and_tools_and_ends_with_0_when_stdin_closes() {
    let workspace = Workspace::new();
    let mut server = StdioServer::start(workspace.executable());

    let initialized = server.initialize();
    let tools = server.request("tools/list", json!({}));
    let ended = server.finish();

    assert_eq!(
        initialized["serverInfo"],
        json!({ "name": "mdya", "version": VERSION })
    );
    assert!(
        initialized["capabilities"]["tools"].is_object(),
        "{initialized}"
    );
    let tools = tools["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["list_collections", "search", "get_document", "get_version"]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{tool}");
    }
    assert_eq!(ended.status.code(), Some(0), "{}", ended.stderr);
    for line in &ended.stdout {
        mcp_message(line);
    }
    assert!(!ended.stderr.contains("Error"), "{}", ended.stderr);
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn over_stdio_the_exchange_cannot_start_after_a_notification_or_stdin_closed_and_ends_with_1() {
    let workspace = Workspace::new();
    let mut notified_first = StdioServer::start(workspace.executable());
    notified_first.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let mut closed_first = StdioServer::start(workspace.executable());

    for ended in [notified_first.finish(), closed_first.finish()] {
        assert_eq!(ended.status.code(), Some(1), "{}", ended.stderr);
        assert_eq!(ended.stdout, Vec::<String>::new());
        assert!(
            ended
                .stderr
                .starts_with("Error: the connection with the MCP client cannot be started\n"),
            "{}",
            ended.stderr
        );
        assert!(
            ended.stderr.ends_with(
                "How to fix:\n    Check the MCP client's settings, then start the MCP server \
                 again from the MCP client.\n"
            ),
            "{}",
            ended.stderr
        );
    }
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_server_shows_its_version_unless_the_setting_it_started_with_hides_it() {
    let workspace = Workspace::new();
    let mut started_showing = StdioServer::start(workspace.executable());
    let initialized = started_showing.initialize();

    assert_success(&workspace.run(&["config", "set", "mcp.show_version", "false"]));
    let version_after_the_change = started_showing.call("get_version", json!({}));
    let mut started_hiding = StdioServer::start(workspace.executable());
    let initialized_hiding = started_hiding.initialize();
    let version_hidden = started_hiding.call("get_version", json!({}));
    let cli_version = workspace.run_executable(&["--version"]);

    assert_eq!(initialized["serverInfo"]["version"], VERSION);
    assert_eq!(
        succeeded(&version_after_the_change),
        json!({ "version": format!("mdya {VERSION}") })
    );
    assert_eq!(initialized_hiding["serverInfo"]["version"], "mdya");
    assert_eq!(succeeded(&version_hidden), json!({ "version": "mdya" }));
    assert_eq!(
        String::from_utf8(cli_version.stdout).unwrap(),
        format!("mdya {VERSION}\n")
    );
    for server in [&mut started_showing, &mut started_hiding] {
        assert_eq!(server.finish().status.code(), Some(0));
    }
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn list_collections_returns_what_the_cli_returns_as_json_with_strings_as_they_are() {
    let workspace = Workspace::new();
    // A control character, which JSON escapes, and a character that changes
    // the direction of text, which MCP leaves as it is.
    let description = "red \u{1b}[31m and \u{202e}reversed";
    assert_success(&workspace.run(&[
        "collection",
        "add",
        "notes",
        "--type",
        "raw",
        "--description",
        description,
    ]));
    raw_collection(&workspace, "memo", &[]);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();

    let (line, result) =
        server.request_with_line("tools/call", json!({ "name": "list_collections" }));

    assert_eq!(
        succeeded(&result),
        json_of(&workspace, &["collection", "list"])
    );
    assert!(
        line.contains("red \\u001b[31m and \u{202e}reversed"),
        "{line}"
    );
    assert_eq!(server.finish().status.code(), Some(0));
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_tool_error_gives_a_code_the_clis_error_message_and_details_to_call_again_with() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "notes", &[]);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();
    // Each call with the code, details, and command line of the same error.
    let calls = [
        (
            "search",
            json!({ "query": " ", "mode": "fts" }),
            "invalid_input",
            json!({ "input": "query" }),
            vec!["search", " ", "--mode", "fts"],
        ),
        (
            "search",
            json!({ "query": "x", "mode": "fts", "limit": 0 }),
            "invalid_input",
            json!({ "input": "limit" }),
            vec!["search", "x", "--mode", "fts", "-n", "0"],
        ),
        (
            "get_document",
            json!({ "collection": "nothing", "key": "a.md" }),
            "collection_not_found",
            json!({ "collection": "nothing" }),
            vec!["get", "nothing", "a.md"],
        ),
        (
            "get_document",
            json!({ "collection": "notes", "key": "a.md" }),
            "document_not_found",
            json!({ "collection": "notes", "key": "a.md" }),
            vec!["get", "notes", "a.md"],
        ),
    ];
    // Inputs that do not fit the input schema, with no command line of their
    // own, and the input each names.
    let unfit = [
        (json!({ "query": "x", "mode": "nearest" }), "mode"),
        (json!({ "query": "x", "limit": "20" }), "limit"),
        (json!({ "mode": "fts" }), "query"),
        (json!({ "query": "x", "mode": "fts", "page": 2 }), "page"),
    ];

    for (tool, arguments, code, details, command_line) in calls {
        let error = failed(&server.call(tool, arguments));
        assert_eq!(error["code"], code, "{error}");
        assert_eq!(error["details"], details, "{error}");
        assert_eq!(
            error["message"],
            cli_error(&workspace, &command_line),
            "{error}"
        );
    }
    for (arguments, input) in unfit {
        let error = failed(&server.call("search", arguments));
        assert_eq!(error["code"], "invalid_input", "{error}");
        assert_eq!(error["details"], json!({ "input": input }), "{error}");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.starts_with(&format!("Error: the tool input {input} is not valid: ")),
            "{message}"
        );
    }
    assert_eq!(server.finish().status.code(), Some(0));
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn without_the_embedding_model_a_vector_search_fails_with_its_code_and_full_text_search_works() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "notes", &[]);
    let mut server = StdioServer::start(workspace.executable_without_the_model());
    server.initialize();

    let vector = server.call("search", json!({ "query": "タグ", "mode": "vector" }));
    let full_text = server.call("search", json!({ "query": "タグ", "mode": "fts" }));

    let error = failed(&vector);
    assert_eq!(error["code"], "embedding_model_unavailable", "{error}");
    assert_eq!(error["details"], json!({}), "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("Error: the embedding model cannot be used\n"),
        "{error}"
    );
    assert_eq!(succeeded(&full_text)["hits"], json!([]));
    assert_eq!(server.finish().status.code(), Some(0));
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn when_the_index_cannot_be_read_a_tool_fails_as_internal_and_the_server_goes_on() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo", &[]);
    // Each version of the table of the collections, and the file that points
    // to the latest one.
    let versions = workspace
        .data_dir()
        .join("lancedb/collection.lance/_versions");
    for entry in std::fs::read_dir(versions).unwrap() {
        std::fs::write(entry.unwrap().path(), "not a version").unwrap();
    }
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();

    let listed = server.call("list_collections", json!({}));
    let version = server.call("get_version", json!({}));

    let error = failed(&listed);
    assert_eq!(error["code"], "internal", "{error}");
    assert_eq!(error["details"], json!({}), "{error}");
    assert_eq!(
        error["message"],
        cli_error(&workspace, &["collection", "list"]),
        "{error}"
    );
    assert_eq!(
        succeeded(&version),
        json!({ "version": format!("mdya {VERSION}") })
    );
    assert_eq!(server.finish().status.code(), Some(0));
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn over_http_the_server_tells_where_it_listens_and_answers_on_the_mcp_path_after_a_failed_request()
{
    let workspace = Workspace::new();
    let server = HttpServer::start(workspace.executable());

    let without_a_session = server.post(
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        &[],
    );
    let (initialized, session) = server.initialize(&[]);
    let version = server.post(
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "get_version", "arguments": {} }
        }),
        &[("Mcp-Session-Id", &session)],
    );
    let stdout = server.stop();

    assert_eq!(without_a_session.status, 400, "{without_a_session:?}");
    assert_eq!(
        initialized["serverInfo"],
        json!({ "name": "mdya", "version": VERSION })
    );
    assert_eq!(version.status, 200, "{version:?}");
    assert_eq!(
        succeeded(&version.result()),
        json!({ "version": format!("mdya {VERSION}") })
    );
    assert_eq!(stdout, "");
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn over_http_a_request_to_another_host_name_or_from_another_origin_is_refused_with_403() {
    let workspace = Workspace::new();
    let server = HttpServer::start(workspace.executable());
    let port = server.address.rsplit_once(':').unwrap().1.to_owned();
    let local_origin = format!("http://localhost:{port}");

    let other_host = server.post(&initialize_request(), &[("Host", "example.com")]);
    let other_origin = server.post(&initialize_request(), &[("Origin", "https://example.com")]);
    let (from_this_computer, _) = server.initialize(&[("Origin", &local_origin)]);

    assert_eq!(other_host.status, 403, "{other_host:?}");
    assert_eq!(
        other_host.body,
        "the host name of the request is not a loopback host name"
    );
    assert_eq!(other_origin.status, 403, "{other_origin:?}");
    assert_eq!(
        other_origin.body,
        "the origin of the request is not a loopback origin"
    );
    assert_eq!(from_this_computer["serverInfo"]["name"], "mdya");
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn search_over_mcp_returns_what_the_cli_returns_as_json() {
    let workspace = Workspace::with_real_model();
    raw_collection(
        &workspace,
        "notes",
        &[("release.md", RELEASE), ("tags.md", TAGS)],
    );
    // Each document has contents of its own: documents that score the same
    // may come in either order, and in a hybrid search, their order changes
    // their scores.
    raw_collection(&workspace, "memo", &[("other.md", MEMO)]);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();
    // Each tool input with the command line of the same search.
    let searches = [
        (json!({ "query": "タグ" }), vec!["search", "タグ"]),
        (
            json!({
                "query": "タグ",
                "mode": "fts",
                "collections": ["notes"],
                "limit": 1,
                "unit": "chunk"
            }),
            vec![
                "search", "タグ", "--mode", "fts", "-c", "notes", "-n", "1", "--unit", "chunk",
            ],
        ),
        (
            json!({ "query": "変更履歴", "mode": "vector" }),
            vec!["search", "変更履歴", "--mode", "vector"],
        ),
    ];

    for (arguments, command_line) in searches {
        let found = succeeded(&server.call("search", arguments));
        assert_eq!(
            found,
            json_of(&workspace, &command_line),
            "{command_line:?}"
        );
    }
    assert_eq!(server.finish().status.code(), Some(0));
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn get_document_returns_the_text_or_its_chunks_and_refuses_more_than_the_mcp_limit() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "notes", &[("tags.md", TAGS)]);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();
    // Each tool input with the command line of the same range.
    let gets = [
        (
            json!({ "collection": "notes", "key": "tags.md" }),
            vec!["get", "notes", "tags.md"],
        ),
        (
            json!({ "collection": "notes", "key": "tags.md", "chunk_index": 1 }),
            vec!["get", "notes", "tags.md", "--chunk-index", "1"],
        ),
        (
            json!({
                "collection": "notes",
                "key": "tags.md",
                "chunk_index": 0,
                "end_chunk_index": 1
            }),
            vec![
                "get",
                "notes",
                "tags.md",
                "--chunk-index",
                "0",
                "--end-chunk-index",
                "1",
            ],
        ),
    ];
    // Ranges that are not valid, and the input each names.
    let refused = [
        (json!({ "chunk_index": 2 }), "chunk_index"),
        (json!({ "end_chunk_index": 1 }), "end_chunk_index"),
        (
            json!({ "chunk_index": 1, "end_chunk_index": 0 }),
            "end_chunk_index",
        ),
    ];

    for (arguments, command_line) in gets {
        let got = succeeded(&server.call("get_document", arguments));
        let cli = workspace.run(&command_line);
        assert_success(&cli);
        assert_eq!(
            got,
            json!({ "collection": "notes", "key": "tags.md", "text": cli.stdout }),
            "{command_line:?}"
        );
    }
    for (mut arguments, input) in refused {
        arguments["collection"] = json!("notes");
        arguments["key"] = json!("tags.md");
        let error = failed(&server.call("get_document", arguments));
        assert_eq!(error["code"], "invalid_input", "{error}");
        assert_eq!(error["details"], json!({ "input": input }), "{error}");
    }
    assert_eq!(server.finish().status.code(), Some(0));

    assert_success(&workspace.run(&["config", "set", "get.mcp_limit_bytes", "10"]));
    let mut limited = StdioServer::start(workspace.executable());
    limited.initialize();
    let too_large = limited.call(
        "get_document",
        json!({ "collection": "notes", "key": "tags.md" }),
    );
    let cli = workspace.run(&["get", "notes", "tags.md"]);

    let error = failed(&too_large);
    assert_eq!(error["code"], "output_too_large", "{error}");
    assert_eq!(
        error["details"],
        json!({ "size_bytes": TAGS.len(), "limit_bytes": 10 }),
        "{error}"
    );
    assert_success(&cli);
    assert_eq!(cli.stdout, TAGS);
    assert_eq!(limited.finish().status.code(), Some(0));
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_put_while_the_server_runs_is_found_by_its_next_calls() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "notes", &[("release.md", RELEASE)]);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();
    let before = server.call("search", json!({ "query": "タグ", "mode": "fts" }));

    put(&workspace, "notes", "tags.md", TAGS);
    let found = server.call("search", json!({ "query": "タグ", "mode": "fts" }));
    let got = server.call(
        "get_document",
        json!({ "collection": "notes", "key": "tags.md" }),
    );
    let listed = server.call("list_collections", json!({}));

    let keys = |result: &Value| -> Vec<String> {
        succeeded(result)["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["key"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(keys(&before), ["release.md"]);
    assert!(keys(&found).contains(&"tags.md".to_owned()), "{found}");
    assert_eq!(succeeded(&got)["text"], TAGS);
    assert_eq!(succeeded(&listed)["collections"][0]["document_count"], 2);
    assert_eq!(server.finish().status.code(), Some(0));
}
