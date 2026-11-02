//! Ollama's embedding endpoint over HTTP (`POST /api/embed`).

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::{EmbedEndpoint, EmbedFailure};
use crate::embedding::supported::OllamaModelName;
use crate::types::{BoxError, EmbeddingVector};

/// The error message Ollama returns when `truncate` is false and an input
/// exceeds the context length. Ollama 0.40.13 returns it with status 400 when
/// its own count of the tokens finds the input too long (`EmbedHandler` in
/// `server/routes.go`), and with status 413 when the model runner refuses the
/// input (`normalizeEmbeddingError` in `llm/llama_server.go`).
const CONTEXT_LENGTH_EXCEEDED: &str = "the input length exceeds the context length";

/// How much of the body of an error answer is read, in bytes. The endpoint is
/// set by the user and can be any server, which could answer with a body of
/// any size; Ollama's error answers are a short JSON object.
const ERROR_BODY_MAX_BYTES: usize = 16 * 1024;

/// How many bytes of an error message are kept in the error, including the
/// `…` that marks a cut.
const MESSAGE_MAX_BYTES: usize = 512;

pub(in crate::embedding) struct HttpEmbedEndpoint {
    client: reqwest::Client,
    url: String,
    model: OllamaModelName,
}

impl HttpEmbedEndpoint {
    /// Connects to the Ollama at `endpoint` (an `http://` or `https://` URL),
    /// for `model`.
    pub(in crate::embedding) fn connect(
        endpoint: &str,
        model: &OllamaModelName,
    ) -> Result<Arc<dyn EmbedEndpoint>, BoxError> {
        // Redirects are not followed: for status 307 and 308, reqwest would
        // send the same request, texts included, to the URL the answer names,
        // which the endpoint setting does not name. A redirect answer is taken
        // as an error answer instead.
        let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        // An Ollama on this PC is reached directly, whatever proxy the
        // environment variables name, as Go's standard library does for
        // `localhost` and loopback addresses: a proxy on another machine would
        // take the texts off this PC, and would look for the Ollama on its own
        // machine. Any other endpoint goes through the proxy that `HTTP_PROXY`
        // and the like name, unless `NO_PROXY` lists its host, as reqwest
        // reads them. The host looked at is that of the URL the requests go
        // to, so that it is the host they reach.
        let url = embed_url(endpoint);
        if is_on_this_pc(&url) {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        Ok(Arc::new(Self {
            client,
            url,
            model: model.clone(),
        }))
    }
}

/// Whether `url` names this PC: its host is `localhost` or a loopback
/// address. A string that is not a URL names no host.
fn is_on_this_pc(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // An IPv6 host comes in brackets.
    let address = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    address
        .parse::<IpAddr>()
        .is_ok_and(|address| address.to_canonical().is_loopback())
}

/// The URL of the embedding endpoint of the Ollama at `endpoint`. A trailing
/// `/` is dropped so that the path does not start with `//`; the endpoint is
/// otherwise used as the user wrote it.
fn embed_url(endpoint: &str) -> String {
    format!("{}/api/embed", endpoint.trim_end_matches('/'))
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
    truncate: bool,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<EmbeddingVector>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
}

#[async_trait]
impl EmbedEndpoint for HttpEmbedEndpoint {
    async fn embed(
        &self,
        inputs: &[&str],
        truncate: bool,
    ) -> Result<Vec<EmbeddingVector>, EmbedFailure> {
        let request = EmbedRequest {
            model: self.model.as_str(),
            input: inputs,
            truncate,
        };
        let mut response = self
            .client
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .map_err(|error| EmbedFailure::Unavailable(Box::new(error)))?;
        let status = response.status();
        if !status.is_success() {
            let mut body = BoundedBody::new(ERROR_BODY_MAX_BYTES);
            while let Ok(Some(piece)) = response.chunk().await {
                if !body.add(&piece) {
                    break;
                }
            }
            let body = body.into_bytes();
            return Err(failure_from_answer(status, &String::from_utf8_lossy(&body)));
        }
        let answer: EmbedResponse = response
            .json()
            .await
            .map_err(|error| EmbedFailure::Unavailable(Box::new(error)))?;
        Ok(answer.embeddings)
    }
}

/// Ollama answered with an error status.
#[derive(Debug, thiserror::Error)]
#[error("Ollama answered {status}: {message}")]
struct ErrorAnswer {
    status: StatusCode,
    message: String,
}

/// The first bytes of a body, collected piece by piece up to a limit, so that
/// a long body is not read to its end.
struct BoundedBody {
    read: Vec<u8>,
    max: usize,
}

impl BoundedBody {
    fn new(max: usize) -> Self {
        Self {
            read: Vec::new(),
            max,
        }
    }

    /// Keeps as much of the next piece of the body as fits under the limit.
    /// Returns whether to read more.
    fn add(&mut self, piece: &[u8]) -> bool {
        let room = self.max - self.read.len();
        self.read.extend_from_slice(&piece[..piece.len().min(room)]);
        self.read.len() < self.max
    }

    /// The bytes kept, at most the limit.
    fn into_bytes(self) -> Vec<u8> {
        self.read
    }
}

/// The failure for an error answer: exceeding the context length when Ollama
/// says so, otherwise Ollama being unavailable with its message, as it came.
fn failure_from_answer(status: StatusCode, body: &str) -> EmbedFailure {
    let message = serde_json::from_str::<ErrorResponse>(body)
        .map(|answer| answer.error)
        .unwrap_or_else(|_| body.to_owned());
    let context_length_status = matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE
    );
    if context_length_status && message == CONTEXT_LENGTH_EXCEEDED {
        return EmbedFailure::ExceedsContextLength;
    }
    EmbedFailure::Unavailable(Box::new(ErrorAnswer {
        status,
        message: shortened(message),
    }))
}

/// `message` when it is at most `MESSAGE_MAX_BYTES` bytes long, otherwise its
/// beginning, cut on a character boundary, followed by `…`, together at most
/// `MESSAGE_MAX_BYTES` bytes.
fn shortened(mut message: String) -> String {
    const ELLIPSIS: char = '…';
    if message.len() <= MESSAGE_MAX_BYTES {
        return message;
    }
    let mut end = MESSAGE_MAX_BYTES - ELLIPSIS.len_utf8();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push(ELLIPSIS);
    message
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// Stands in for Ollama on a local port: reads one request to its end and
    /// answers it with `answer`, a whole HTTP response. Returns the endpoint.
    async fn answer_once(answer: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            socket.write_all(answer.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        format!("http://{address}")
    }

    /// Reads the head of a request and as much body as its `content-length`
    /// says.
    async fn read_request(socket: &mut tokio::net::TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            let Some(head_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
            let body_length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map_or(0, |length| length.trim().parse::<usize>().unwrap());
            if request.len() >= head_end + 4 + body_length {
                return;
            }
        }
    }

    async fn embed_against(answer: &'static str) -> Result<Vec<EmbeddingVector>, EmbedFailure> {
        let endpoint = answer_once(answer).await;
        let model = OllamaModelName::for_test("all-minilm");
        let connected = HttpEmbedEndpoint::connect(&endpoint, &model).unwrap();
        connected.embed(&["Why is the sky blue?"], false).await
    }

    #[tokio::test]
    async fn returns_the_vectors_of_a_successful_answer() {
        let answer = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 49\r\nconnection: close\r\n\r\n{\"model\":\"all-minilm\",\"embeddings\":[[0.5,-0.25]]}";

        assert_eq!(embed_against(answer).await.unwrap(), vec![vec![0.5, -0.25]]);
    }

    #[tokio::test]
    async fn takes_a_context_length_answer_as_exceeding_the_context_length() {
        let answer = "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: 55\r\nconnection: close\r\n\r\n{\"error\":\"the input length exceeds the context length\"}";

        assert!(matches!(
            embed_against(answer).await,
            Err(EmbedFailure::ExceedsContextLength)
        ));
    }

    // Following it would send the texts to the server the answer names, which
    // the endpoint setting does not name.
    #[tokio::test]
    async fn takes_a_redirect_as_an_error_answer_without_following_it() {
        let answer = "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:9/api/embed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

        let failure = embed_against(answer).await.unwrap_err();

        assert_eq!(
            unavailable_message(failure),
            "Ollama answered 307 Temporary Redirect: "
        );
    }

    fn unavailable_message(failure: EmbedFailure) -> String {
        match failure {
            EmbedFailure::Unavailable(source) => source.to_string(),
            EmbedFailure::ExceedsContextLength => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn writes_the_model_the_inputs_and_whether_to_truncate() {
        let request = EmbedRequest {
            model: "nomic-embed-text",
            input: &["a", "b"],
            truncate: false,
        };

        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"model": "nomic-embed-text", "input": ["a", "b"], "truncate": false})
        );
    }

    #[test]
    fn reads_the_vectors_from_the_answer() {
        let answer: EmbedResponse = serde_json::from_str(
            r#"{"model":"all-minilm","embeddings":[[0.5,-0.25],[1,0]],"total_duration":14143917}"#,
        )
        .unwrap();

        assert_eq!(answer.embeddings, vec![vec![0.5, -0.25], vec![1.0, 0.0]]);
    }

    #[test]
    fn takes_ollamas_context_length_error_as_exceeding_the_context_length() {
        // Ollama 0.40.13 answers 400 when it finds the input too long, and 413
        // when the model runner refuses it.
        for status in [StatusCode::BAD_REQUEST, StatusCode::PAYLOAD_TOO_LARGE] {
            let failure = failure_from_answer(
                status,
                r#"{"error":"the input length exceeds the context length"}"#,
            );

            assert!(
                matches!(failure, EmbedFailure::ExceedsContextLength),
                "{status}"
            );
        }
    }

    #[test]
    fn takes_the_context_length_message_with_another_status_as_ollama_being_unavailable() {
        let failure = failure_from_answer(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"the input length exceeds the context length"}"#,
        );

        assert_eq!(
            unavailable_message(failure),
            "Ollama answered 500 Internal Server Error: the input length exceeds the context length"
        );
    }

    #[test]
    fn takes_other_error_answers_as_ollama_being_unavailable_with_its_message() {
        let not_found = failure_from_answer(
            StatusCode::NOT_FOUND,
            r#"{"error":"model 'no-such-model' not found"}"#,
        );
        let other_bad_request =
            failure_from_answer(StatusCode::BAD_REQUEST, r#"{"error":"invalid input type"}"#);
        let not_json = failure_from_answer(StatusCode::BAD_GATEWAY, "upstream down");

        assert_eq!(
            unavailable_message(not_found),
            "Ollama answered 404 Not Found: model 'no-such-model' not found"
        );
        assert_eq!(
            unavailable_message(other_bad_request),
            "Ollama answered 400 Bad Request: invalid input type"
        );
        assert_eq!(
            unavailable_message(not_json),
            "Ollama answered 502 Bad Gateway: upstream down"
        );
    }

    // Replacing control characters is left to whoever prints the error.
    #[test]
    fn keeps_the_message_as_it_came_with_its_control_characters() {
        let failure = failure_from_answer(StatusCode::BAD_GATEWAY, "\nline 1\nline 2\x1b[0m\n");

        assert_eq!(
            unavailable_message(failure),
            "Ollama answered 502 Bad Gateway: \nline 1\nline 2\x1b[0m\n"
        );
    }

    #[test]
    fn asks_for_no_more_of_a_body_once_it_has_enough() {
        let mut body = BoundedBody::new(25);

        let wants_more: Vec<bool> = (0..3).map(|_| body.add(&[b'x'; 10])).collect();

        assert_eq!(wants_more, [true, true, false]);
        assert_eq!(body.into_bytes(), vec![b'x'; 25]);
    }

    #[test]
    fn keeps_no_more_than_the_limit_of_a_piece_larger_than_the_limit() {
        let mut body = BoundedBody::new(25);

        let wants_more = body.add(&[b'x'; 40]);

        assert!(!wants_more);
        assert_eq!(body.into_bytes(), vec![b'x'; 25]);
    }

    #[test]
    fn keeps_all_of_a_short_body() {
        let mut body = BoundedBody::new(25);

        body.add(b"hel");
        body.add(b"lo");

        assert_eq!(body.into_bytes(), b"hello");
    }

    #[test]
    fn keeps_a_message_that_fits_as_it_is() {
        let fits = "a".repeat(MESSAGE_MAX_BYTES);

        assert_eq!(shortened(fits.clone()), fits);
    }

    #[test]
    fn cuts_a_long_message_so_that_it_fits_with_the_ellipsis() {
        // "…" takes 3 bytes, so 509 bytes of the message fit with it.
        let long = "a".repeat(MESSAGE_MAX_BYTES + 1);

        let cut = shortened(long);

        assert_eq!(cut, format!("{}…", "a".repeat(MESSAGE_MAX_BYTES - 3)));
        assert_eq!(cut.len(), MESSAGE_MAX_BYTES);
    }

    #[test]
    fn cuts_a_long_message_on_a_character_boundary() {
        // Byte 509, where the cut would fall, is inside "あ" (bytes 508 to
        // 510), so the cut moves back to byte 508.
        let long = format!("{}あいう", "a".repeat(MESSAGE_MAX_BYTES - 4));

        let cut = shortened(long);

        assert_eq!(cut, format!("{}…", "a".repeat(MESSAGE_MAX_BYTES - 4)));
        assert!(cut.len() <= MESSAGE_MAX_BYTES);
    }

    #[test]
    fn takes_localhost_and_loopback_addresses_as_this_pc() {
        for url in [
            "http://localhost:11434/api/embed",
            "http://LOCALHOST:11434/api/embed",
            "http://127.0.0.1:11434/api/embed",
            "http://127.1.2.3/api/embed",
            "http://[::1]:11434/api/embed",
            "http://[::ffff:127.0.0.1]:11434/api/embed",
        ] {
            assert!(is_on_this_pc(url), "{url}");
        }
    }

    // The host of each of these URLs is another form of a loopback IPv4
    // address, which `reqwest::Url` reads as `127.0.0.1`. Requests to these
    // URLs go to `127.0.0.1`, so the URLs are taken as this PC too.
    #[test]
    fn takes_other_forms_of_a_loopback_ipv4_address_as_this_pc() {
        for url in [
            "http://127.1:11434/api/embed",
            "http://2130706433:11434/api/embed",
            "http://0x7f.0.0.1:11434/api/embed",
            "http://0177.0.0.1:11434/api/embed",
        ] {
            assert_eq!(
                reqwest::Url::parse(url).unwrap().host_str(),
                Some("127.0.0.1"),
                "{url}"
            );
            assert!(is_on_this_pc(url), "{url}");
        }
    }

    #[test]
    fn takes_other_hosts_and_strings_that_are_not_urls_as_not_this_pc() {
        for url in [
            "http://192.168.1.10:11434/api/embed",
            "https://example.com/ollama/api/embed",
            "http://localhost.example.com/api/embed",
            // Reaches this PC on some systems, but is not a loopback address,
            // as in Go's standard library.
            "http://0.0.0.0:11434/api/embed",
            "http://[::2]:11434/api/embed",
            "http://[::ffff:192.168.1.10]:11434/api/embed",
            "not a URL",
        ] {
            assert!(!is_on_this_pc(url), "{url}");
        }
    }

    // An endpoint with no host, `http://`, becomes `http:/api/embed`, which
    // reqwest reads as the host `api`; the judgment is about that host too.
    #[test]
    fn judges_the_host_of_the_url_that_the_requests_go_to() {
        let no_host = embed_url("http://");

        assert!(is_on_this_pc(&embed_url("http://localhost:11434/")));
        assert_eq!(
            reqwest::Url::parse(&no_host).unwrap().host_str(),
            Some("api")
        );
        assert!(!is_on_this_pc(&no_host));
    }

    #[test]
    fn appends_the_api_path_to_the_endpoint_with_or_without_a_trailing_slash() {
        assert_eq!(
            embed_url("http://127.0.0.1:11434"),
            "http://127.0.0.1:11434/api/embed"
        );
        assert_eq!(
            embed_url("http://127.0.0.1:11434/"),
            "http://127.0.0.1:11434/api/embed"
        );
        assert_eq!(
            embed_url("https://example.com/ollama"),
            "https://example.com/ollama/api/embed"
        );
    }
}
