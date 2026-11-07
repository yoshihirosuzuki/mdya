//! Listening, the checks of the host name and the origin, and the MCP server
//! over HTTP, driven with HTTP requests on a local port.

#![expect(
    clippy::result_large_err,
    reason = "the operation handler returns the operations' error, as the trait does"
)]

use serde_json::{Value, json};

use super::super::super::error_report::test_double::TestErrorReport;
use super::super::super::log_progress::test_double::TestConsole;
use super::super::super::operations::test_double::TestOperations;
use super::super::super::operations::{
    CollectionEntry, CollectionList, SafeInteger, SafeJsonObject,
};
use super::super::listen_http;
use super::*;

/// A description with characters that the human-readable output would
/// replace: ESC, which JSON escapes, and DEL, a C1 control, and a character
/// that changes the direction of text, which JSON leaves as they are.
const DESCRIPTION: &str = "a\u{1b}b\u{7f}c\u{9b}d\u{202e}e";

fn parts() -> McpServerParts {
    McpServerParts {
        operations: Arc::new(TestOperations::new().on_list_collections(|_| {
            Ok(CollectionList {
                collections: vec![CollectionEntry {
                    name: "notes".to_owned(),
                    collection_type: "raw".to_owned(),
                    description: Some(DESCRIPTION.to_owned()),
                    document_count: SafeInteger::new(0),
                    attributes: SafeJsonObject::new(serde_json::Map::new()),
                }],
            })
        })),
        show_version: true,
        size_limit: None,
        error_report: Arc::new(TestErrorReport::new()),
        console: Arc::new(TestConsole::new()),
    }
}

/// Starts the server on a port the OS chooses on 127.0.0.1, and returns that
/// address and the task the server runs in.
async fn start() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = listen_http("127.0.0.1", 0).await.unwrap();
    let addr = listener.local_addrs()[0];
    let server = tokio::spawn(listener.serve(parts()));
    (addr, server)
}

/// A client that does not go through a proxy, whatever the environment says.
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" }
        }
    })
}

/// Posts the message to the MCP path, addressed to `host`.
async fn post(addr: SocketAddr, host: &str, message: &Value) -> reqwest::Response {
    client()
        .post(format!("http://{addr}{PATH}"))
        .header(header::HOST, host)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(message)
        .send()
        .await
        .unwrap()
}

/// Posts the message to the MCP path, addressed to the server's address, as a
/// script of a web page of `origin` would.
async fn post_from(addr: SocketAddr, origin: &str, message: &Value) -> reqwest::Response {
    client()
        .post(format!("http://{addr}{PATH}"))
        .header(header::ORIGIN, origin)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .json(message)
        .send()
        .await
        .unwrap()
}

/// The JSON-RPC message in the response: the body when it is JSON, or the
/// data of the event that holds it when it is an event stream.
async fn message(response: reqwest::Response) -> Value {
    message_in(&response.text().await.unwrap())
}

fn message_in(body: &str) -> Value {
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(body);
    serde_json::from_str(data).unwrap()
}

#[tokio::test]
async fn listening_on_an_ip_address_with_port_0_gets_a_port_from_the_os() {
    let listener = listen_http("127.0.0.1", 0).await.unwrap();

    let addrs = listener.local_addrs();
    assert_eq!(addrs.len(), 1);
    assert_eq!(addrs[0].ip(), IpAddr::from([127, 0, 0, 1]));
    assert_ne!(addrs[0].port(), 0);
}

#[tokio::test]
async fn a_host_name_listens_on_the_loopback_addresses_it_resolves_to() {
    let listener = listen_http("localhost", 0).await.unwrap();

    let addrs = listener.local_addrs();
    assert!(!addrs.is_empty());
    assert!(addrs.iter().all(|addr| addr.ip().is_loopback()));
    assert!(addrs.iter().all(|addr| addr.port() == addrs[0].port()));
}

#[tokio::test]
async fn every_address_listens_on_the_port_the_os_chose_for_the_first() {
    let ips = vec![IpAddr::from([127, 0, 0, 1]), "::1".parse().unwrap()];

    let listener = bind_all(ips, 0).await.unwrap();

    let addrs = listener.local_addrs();
    assert_eq!(addrs.len(), 2);
    assert_eq!(addrs[0].port(), addrs[1].port());
}

#[tokio::test]
async fn a_port_in_use_makes_the_address_unavailable() {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();

    let error = listen_http("127.0.0.1", port).await.unwrap_err();

    assert!(matches!(
        error,
        McpError::AddressUnavailable { host, port: p, .. } if host == "127.0.0.1" && p == port
    ));
}

// The `.invalid` top-level domain never resolves (RFC 6761, 6.4).
#[tokio::test]
async fn a_name_that_does_not_resolve_makes_the_address_unavailable() {
    let error = listen_http("mdya-test.invalid", 0).await.unwrap_err();

    assert!(matches!(
        error,
        McpError::AddressUnavailable { host, .. } if host == "mdya-test.invalid"
    ));
}

#[test]
fn the_addresses_a_name_resolves_to_are_kept_once_each_in_order() {
    let addrs = [
        SocketAddr::from(([127, 0, 0, 1], 80)),
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 80)),
        SocketAddr::from(([127, 0, 0, 1], 80)),
    ];

    let ips = distinct(addrs).unwrap();

    assert_eq!(
        ips,
        [
            IpAddr::from([127, 0, 0, 1]),
            "::1".parse::<IpAddr>().unwrap()
        ]
    );
}

#[test]
fn a_name_that_resolves_to_no_address_is_an_error() {
    let error = distinct(Vec::<SocketAddr>::new()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

/// Whether a request with this `Host` header names this computer.
fn host_names_this_computer(host: &str) -> bool {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
    names_this_computer(&headers, &Uri::from_static("/mcp"))
}

#[test]
fn localhost_and_the_loopback_addresses_name_this_computer_with_any_port() {
    for host in [
        "localhost",
        "LocalHost:8000",
        "127.0.0.1",
        "127.0.0.1:8000",
        "127.1.2.3:9",
        "[::1]",
        "[::1]:8000",
        "[::ffff:127.0.0.1]:8000",
        "[::ffff:127.9.9.9]",
    ] {
        assert!(host_names_this_computer(host), "{host}");
    }
}

#[test]
fn other_host_names_and_addresses_do_not_name_this_computer() {
    for host in [
        "example.com",
        "example.com:8000",
        "localhost.",
        "sub.localhost",
        "10.0.0.1:8000",
        "127.1:8000",
        "127.0.0.1.example:8000",
        "user@localhost:8000",
        "0.0.0.0:8000",
        "[::]:8000",
        "[::ffff:10.0.0.1]",
        "[fe80::1]",
        "not a host",
    ] {
        assert!(!host_names_this_computer(host), "{host}");
    }
}

#[test]
fn a_host_header_that_is_not_text_does_not_name_this_computer() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_bytes(b"\xfflocalhost").unwrap(),
    );

    assert!(!names_this_computer(&headers, &Uri::from_static("/mcp")));
}

#[test]
fn without_a_host_header_the_host_in_the_uri_is_looked_at() {
    let headers = HeaderMap::new();

    assert!(names_this_computer(
        &headers,
        &Uri::from_static("http://localhost:8000/mcp")
    ));
    assert!(!names_this_computer(
        &headers,
        &Uri::from_static("http://example.com/mcp")
    ));
    assert!(!names_this_computer(
        &headers,
        &Uri::from_static("http://user@localhost/mcp")
    ));
    assert!(!names_this_computer(&headers, &Uri::from_static("/mcp")));
}

/// Whether a request with these `Origin` headers names a page of this
/// computer.
fn origins_accepted(origins: &[&[u8]]) -> bool {
    let mut headers = HeaderMap::new();
    for origin in origins {
        headers.append(header::ORIGIN, HeaderValue::from_bytes(origin).unwrap());
    }
    origins_name_this_computer(&headers)
}

#[test]
fn a_request_without_an_origin_is_accepted() {
    assert!(origins_accepted(&[]));
}

#[test]
fn http_and_https_origins_of_localhost_and_the_loopback_addresses_are_accepted_with_any_port() {
    for origin in [
        "http://localhost",
        "https://LocalHost:8443",
        "HTTP://localhost:3000",
        "http://127.0.0.1:3000",
        "http://127.5.6.7:9",
        "http://[::1]:8000",
        "https://[::ffff:127.0.0.1]",
    ] {
        assert!(origins_accepted(&[origin.as_bytes()]), "{origin}");
    }
}

// An origin is a scheme, `://`, a host, and a port if any, and nothing else
// (RFC 6454, 7.1). `null` stands for an origin that is not a scheme, a host,
// and a port, and is sent from a privacy-sensitive context (RFC 6454, 6.2 and
// 7.3). One header may list several origins separated by a space (RFC 6454,
// 7.1).
#[test]
fn other_origins_are_refused() {
    for origin in [
        "https://example.com",
        "http://example.com:8000",
        "http://localhost.",
        "http://sub.localhost",
        "http://10.0.0.1",
        "http://127.1",
        "http://127.0.0.1.example",
        "http://[fe80::1]",
        "http://localhost/",
        "http://localhost/path",
        "http://localhost?query",
        "http://localhost#fragment",
        "http://user@localhost",
        "http://",
        "null",
        "http://localhost http://example.com",
        "file://localhost",
        "chrome-extension://abcdefgh",
        "localhost:8000",
        "",
        "not an origin",
    ] {
        assert!(!origins_accepted(&[origin.as_bytes()]), "{origin}");
    }
}

#[test]
fn an_origin_that_is_not_text_is_refused() {
    assert!(!origins_accepted(&[b"http://\xfflocalhost"]));
}

#[test]
fn a_request_is_refused_when_any_of_its_origins_is_refused() {
    assert!(!origins_accepted(&[
        b"http://localhost",
        b"https://example.com"
    ]));
}

#[tokio::test]
async fn the_server_answers_initialize_on_the_mcp_path() {
    let (addr, _server) = start().await;

    let response = post(addr, &addr.to_string(), &initialize()).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let message = message(response).await;
    assert_eq!(message["result"]["serverInfo"]["name"], json!("mdya"));
}

#[tokio::test]
async fn a_request_addressed_to_another_host_name_is_refused() {
    let (addr, _server) = start().await;

    let response = post(addr, "attacker.example:8000", &initialize()).await;

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        response.text().await.unwrap(),
        "the host name of the request is not a loopback host name"
    );
}

// rmcp's own check of the host name lists only `localhost`, `127.0.0.1`, and
// `::1`. It is off, so that every loopback address is answered.
#[tokio::test]
async fn a_request_addressed_to_any_loopback_address_is_answered() {
    let (addr, _server) = start().await;

    let response = post(addr, &format!("127.5.6.7:{}", addr.port()), &initialize()).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn a_request_from_a_page_of_another_origin_is_refused() {
    let (addr, _server) = start().await;

    let response = post_from(addr, "https://attacker.example", &initialize()).await;

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        response.text().await.unwrap(),
        "the origin of the request is not a loopback origin"
    );
}

#[tokio::test]
async fn a_request_from_a_page_of_this_computer_is_answered() {
    let (addr, _server) = start().await;

    let response = post_from(addr, "http://localhost:3000", &initialize()).await;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn the_server_answers_on_the_mcp_path_only() {
    let (addr, _server) = start().await;

    let response = client()
        .post(format!("http://{addr}/other"))
        .json(&initialize())
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_failed_exchange_does_not_stop_the_server() {
    let (addr, server) = start().await;

    let broken = client()
        .post(format!("http://{addr}{PATH}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .body("{ not json")
        .send()
        .await
        .unwrap();
    let after = post(addr, &addr.to_string(), &initialize()).await;

    assert!(broken.status().is_client_error());
    assert_eq!(after.status(), reqwest::StatusCode::OK);
    assert!(!server.is_finished());
}

// The body carries the result twice, as structured content and as text, and
// JSON escapes ESC as `\u001b` in both. Nothing else is escaped.
#[tokio::test]
async fn a_tool_call_over_http_returns_the_result_with_only_jsons_own_escaping() {
    let (addr, _server) = start().await;
    let initialized = post(addr, &addr.to_string(), &initialize()).await;
    let session = initialized
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    message(initialized).await;
    let send = |message: Value| {
        client()
            .post(format!("http://{addr}{PATH}"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-session-id", session.clone())
            .header("mcp-protocol-version", "2025-06-18")
            .json(&message)
            .send()
    };

    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await
        .unwrap();
    let response = send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "list_collections", "arguments": {} }
    }))
    .await
    .unwrap();

    let body = response.text().await.unwrap();
    let message = message_in(&body);
    assert_eq!(message["result"]["isError"], json!(false));
    let collection = &message["result"]["structuredContent"]["collections"][0];
    assert_eq!(collection["name"], json!("notes"));
    assert_eq!(collection["description"], json!(DESCRIPTION));
    assert!(body.contains("\u{7f}c\u{9b}d\u{202e}e"));
    assert!(body.contains(&format!("a{0}u001bb", '\\')));
    for escaped in ["u007f", "u009b", "u202e"] {
        assert!(!body.contains(&format!("{}{escaped}", '\\')), "{escaped}");
    }
}
