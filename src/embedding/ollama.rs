//! `OllamaEmbedder` — optional `ollama:<model>` embedding backend.
//!
//! Sends text to a local Ollama server's `/api/embed` endpoint instead of
//! running on-device candle inference. Opt-in via `config.yml`'s
//! `embedding.model: ollama:<model>`; the default `cl-nagoya/ruri-v3-30m`
//! path is unaffected. mdya treats the model as an opaque embedding service:
//! it sends raw text with no retrieval prefix (any prefix the model needs is
//! the Ollama model template's responsibility) and learns the
//! vector dimension by probing the endpoint once at construction.
//!
//! ## Locality
//! The endpoint comes entirely from `config.yml`'s
//! `embedding.ollama.endpoint` key (default `http://127.0.0.1:11434`,
//! matching Ollama's own bind). mdya honours the YAML value verbatim
//! and never reads any environment variable to derive the endpoint —
//! that closes the "ambient env var redirects the request off-device"
//! surface. Pointing the YAML value at a non-loopback host is a
//! deliberate user choice and means embedding text leaves the device;
//! there is no code guard against this, only the explicit YAML opt-in.
//!
//! ## Sync/async bridge
//! The [`Embedder`] trait is synchronous and is called from blocking contexts
//! (`block_in_place` in search, `spawn_blocking` in ingest). reqwest is async,
//! and `reqwest::blocking` panics inside `block_in_place`. So each embedder
//! owns one dedicated OS thread running its own current-thread runtime; the
//! sync methods hand a job to that thread over a `std::sync::mpsc` channel and
//! block on the reply. The calling thread only does a std channel `recv()`,
//! which never touches the ambient tokio runtime, so it is panic-proof from
//! any context.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde::{Deserialize, Serialize};

use super::{EmbedError, Embedder};

/// `config.yml` `embedding.model` prefix that selects this backend.
pub const OLLAMA_PREFIX: &str = "ollama:";

/// Maximum number of UTF-8 bytes of a server-returned error body that
/// `sanitize_error_body` will splice into an `EmbedError::Ollama`
/// message. A malicious or misconfigured Ollama process could otherwise
/// return a multi-gigabyte body that `EmbedError` would have to carry
/// to the logger. The chosen size is enough to surface a typical
/// Ollama JSON error reply while keeping the message bounded. Note
/// the actual upper bound on `sanitize_error_body`'s return value is
/// `ERROR_BODY_MAX_BYTES + 3` (the ellipsis `…` is 3 UTF-8 bytes), so
/// callers can size their downstream buffers as `~ERROR_BODY_MAX_BYTES`
/// without an explicit headroom calculation.
const ERROR_BODY_MAX_BYTES: usize = 512;

/// Embedding backend that delegates inference to a local Ollama server.
pub struct OllamaEmbedder {
    /// The full `config.yml` value (`ollama:<model>`). This is the pin stored
    /// on every chunk (`chunks.embedding_model`), so `model_id` returns it
    /// verbatim — not the bare model name sent to the API.
    model_id: String,
    /// Probed at construction (`/api/embed` reports no dimension field, so we
    /// read it off the first embedding's length).
    dim: usize,
    bridge: EmbedBridge,
}

impl OllamaEmbedder {
    /// Construct from the full `embedding.model` string (e.g.
    /// `ollama:nomic-embed-text`). Spawns the worker thread and probes the
    /// endpoint once to learn the vector dimension. The probe runs off the
    /// async scheduler via `spawn_blocking`, so it never blocks a runtime
    /// worker. Requires the Ollama server to be reachable.
    pub async fn new(model_id: &str, endpoint: &str) -> Result<Self, EmbedError> {
        let model = parse_ollama_model(model_id)?;
        // Drop a trailing `/` so the `{base}/api/embed` request URL never
        // collapses into `//api/embed`. The endpoint otherwise flows through
        // unchanged — mdya does not silently rewrite what the user wrote in
        // `config.yml`.
        let base = endpoint.trim_end_matches('/').to_string();
        let bridge = EmbedBridge::spawn(base, model);
        let dim = probe_dim(bridge.clone()).await?;
        Ok(Self {
            model_id: model_id.to_string(),
            dim,
            bridge,
        })
    }
}

impl Embedder for OllamaEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    // Query and passage embed identically: mdya applies no prefix, so there
    // is no query/passage distinction to make.
    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.bridge.embed(texts)
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.bridge.embed(texts)
    }
}

/// Split the Ollama model name out of the full `ollama:<name>` pin.
fn parse_ollama_model(model_id: &str) -> Result<String, EmbedError> {
    let name = model_id
        .strip_prefix(OLLAMA_PREFIX)
        .ok_or_else(|| EmbedError::Ollama(format!("'{model_id}' is not an 'ollama:' model")))?;
    if name.is_empty() {
        return Err(EmbedError::Ollama(
            "model name after 'ollama:' is empty".to_string(),
        ));
    }
    Ok(name.to_string())
}

/// Probe the endpoint for the model's output dimension. Runs the blocking
/// bridge call on a `spawn_blocking` thread so construction (an async fn) does
/// not stall a runtime worker.
async fn probe_dim(bridge: EmbedBridge) -> Result<usize, EmbedError> {
    let embeddings = tokio::task::spawn_blocking(move || bridge.embed(&["dim probe"]))
        .await
        .map_err(|e| EmbedError::Ollama(format!("dim probe task: {e}")))??;
    embeddings
        .first()
        .map(Vec::len)
        .filter(|&d| d > 0)
        .ok_or_else(|| EmbedError::Ollama("dim probe returned no embedding".to_string()))
}

/// Blocking handle to the worker thread (see module docs). Cloneable so the
/// constructor's dim probe and steady-state embed calls share one worker.
#[derive(Clone)]
struct EmbedBridge {
    jobs: Sender<EmbedJob>,
}

impl EmbedBridge {
    /// Spawn the worker thread; it owns a current-thread runtime + reqwest
    /// client and serves jobs until every `EmbedBridge` clone is dropped.
    fn spawn(base_url: String, model: String) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        thread::spawn(move || worker_loop(&base_url, &model, &jobs_rx));
        Self { jobs: jobs_tx }
    }

    /// Block until the worker returns embeddings. Safe from any context: the
    /// only blocking primitive touched here is std mpsc, never the ambient
    /// tokio runtime.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let job = EmbedJob {
            inputs: texts.iter().map(|t| (*t).to_string()).collect(),
            reply: reply_tx,
        };
        self.jobs
            .send(job)
            .map_err(|_| EmbedError::Ollama("embed worker thread is gone".to_string()))?;
        reply_rx
            .recv()
            .map_err(|_| EmbedError::Ollama("embed worker dropped the reply".to_string()))?
    }
}

/// One embed job: the input texts and a reply channel for the result.
struct EmbedJob {
    inputs: Vec<String>,
    reply: Sender<Result<Vec<Vec<f32>>, EmbedError>>,
}

fn worker_loop(base_url: &str, model: &str, jobs: &Receiver<EmbedJob>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Fail every queued job loudly instead of letting callers hang on
            // `recv()` forever when the worker cannot start.
            let message = format!("build ollama worker runtime: {e}");
            for job in jobs.iter() {
                let _ = job.reply.send(Err(EmbedError::Ollama(message.clone())));
            }
            return;
        }
    };
    let client = reqwest::Client::new();
    for job in jobs.iter() {
        let result = runtime.block_on(request_embeddings(&client, base_url, model, &job.inputs));
        let _ = job.reply.send(result);
    }
}

async fn request_embeddings(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let url = format!("{base_url}/api/embed");
    let response = client
        .post(&url)
        .json(&EmbedRequest {
            model,
            input: inputs,
        })
        .send()
        .await
        .map_err(|e| EmbedError::Ollama(format!("POST {url}: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(EmbedError::Ollama(format!(
            "{url} returned {status}: {}",
            sanitize_error_body(&body)
        )));
    }
    let parsed: EmbedResponse = response
        .json()
        .await
        .map_err(|e| EmbedError::Ollama(format!("decode /api/embed response: {e}")))?;
    Ok(parsed.embeddings)
}

/// Sanitize a server-returned error body before splicing it into an
/// `EmbedError::Ollama` message: drop ANSI CSI escape sequences (`ESC
/// [ … letter`) plus other control characters so a malicious server
/// cannot inject terminal control codes into mdya's stderr or
/// `tracing` output, then truncate to `ERROR_BODY_MAX_BYTES` so a
/// server returning a multi-gigabyte body cannot make the error
/// message itself the failure mode. CR/LF/HT are kept so multi-line
/// server-side messages still read naturally.
///
/// **Scope**: only CSI sequences are matched as escape sequences.
/// Other ANSI introducers (OSC `ESC ]`, SS3 `ESC O`, etc.) have their
/// `ESC` byte dropped by the introducer rule but their payload survives
/// as ordinary characters. That is intentional — without the `ESC`
/// the terminal cannot reinterpret the payload as a control sequence,
/// so the residue is at worst noisy text, not an injection vector.
fn sanitize_error_body(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len().min(ERROR_BODY_MAX_BYTES));
    let mut in_csi_escape = false;
    let mut byte_len = 0usize;
    for c in trimmed.chars() {
        // ESC (`\x1b`) introduces an ANSI escape sequence; skip until
        // the closing letter (CSI: `ESC [ … letter`). For
        // single-character ESC sequences without a `[` follow-up the
        // next character closes it, which still drops the unsafe byte.
        if c == '\x1b' {
            in_csi_escape = true;
            continue;
        }
        if in_csi_escape {
            if c.is_ascii_alphabetic() {
                in_csi_escape = false;
            }
            continue;
        }
        // Drop other control characters except CR/LF/HT so the message
        // cannot embed bell, backspace, etc.
        if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
            continue;
        }
        let c_len = c.len_utf8();
        if byte_len + c_len > ERROR_BODY_MAX_BYTES {
            out.push('…');
            break;
        }
        out.push(c);
        byte_len += c_len;
    }
    out
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ollama_model_strips_the_prefix() {
        assert_eq!(
            parse_ollama_model("ollama:nomic-embed-text").expect("valid"),
            "nomic-embed-text"
        );
    }

    #[test]
    fn parse_ollama_model_rejects_missing_prefix() {
        let err = parse_ollama_model("cl-nagoya/ruri-v3-30m").expect_err("no prefix");
        assert!(err.to_string().contains("not an 'ollama:' model"));
    }

    #[test]
    fn parse_ollama_model_rejects_empty_name() {
        let err = parse_ollama_model("ollama:").expect_err("empty name");
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn sanitize_error_body_passes_short_text_through() {
        assert_eq!(sanitize_error_body("model not found"), "model not found");
    }

    #[test]
    fn sanitize_error_body_strips_ansi_color_escape() {
        let raw = "\x1b[31mboom\x1b[0m at line 1";
        assert_eq!(sanitize_error_body(raw), "boom at line 1");
    }

    #[test]
    fn sanitize_error_body_drops_other_control_characters() {
        // Bell + backspace + form feed must not survive into the log
        // message; tab / newline / carriage return are kept.
        let raw = "abc\x07\x08\x0Cdef\nghi\tjkl";
        assert_eq!(sanitize_error_body(raw), "abcdef\nghi\tjkl");
    }

    #[test]
    fn sanitize_error_body_truncates_overlong_input_with_ellipsis() {
        let raw = "a".repeat(ERROR_BODY_MAX_BYTES + 100);
        let sanitized = sanitize_error_body(&raw);
        let expected_prefix = "a".repeat(ERROR_BODY_MAX_BYTES);
        let expected = format!("{expected_prefix}…");
        assert_eq!(sanitized, expected);
    }

    #[test]
    fn sanitize_error_body_truncate_does_not_split_a_multibyte_char() {
        // Pad with `ERROR_BODY_MAX_BYTES - 2` ASCII bytes so the next
        // input character is a 3-byte UTF-8 codepoint that would push
        // the running byte length to `ERROR_BODY_MAX_BYTES + 1`. The
        // truncate guard must refuse to splice the partial codepoint
        // and emit the ellipsis instead, so the result is the ASCII
        // prefix followed by `…` — never a malformed UTF-8 prefix.
        let prefix_len = ERROR_BODY_MAX_BYTES - 2;
        let raw = format!("{}中trailing", "a".repeat(prefix_len));
        let sanitized = sanitize_error_body(&raw);
        let expected = format!("{}…", "a".repeat(prefix_len));
        assert_eq!(sanitized, expected);
    }

    #[test]
    fn embed_response_deserializes_batched_embeddings() {
        let json = r#"{"model":"nomic-embed-text","embeddings":[[0.1,0.2],[0.3,0.4]]}"#;
        let parsed: EmbedResponse = serde_json::from_str(json).expect("valid response");
        assert_eq!(parsed.embeddings, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
    }

    #[test]
    fn embed_request_serializes_model_and_input_list() {
        let inputs = vec!["a".to_string(), "b".to_string()];
        let json = serde_json::to_string(&EmbedRequest {
            model: "nomic-embed-text",
            input: &inputs,
        })
        .expect("serialize");
        assert_eq!(json, r#"{"model":"nomic-embed-text","input":["a","b"]}"#);
    }

    // End-to-end smoke against a live server. Not run by `just check` / CI:
    //   ollama serve && ollama pull nomic-embed-text
    //   cargo test --lib ollama -- --ignored
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires a running Ollama server with nomic-embed-text pulled"]
    async fn smoke_construct_and_embed_against_live_ollama() {
        let embedder = OllamaEmbedder::new("ollama:nomic-embed-text", "http://127.0.0.1:11434")
            .await
            .expect("construct against live ollama");
        assert!(embedder.dim() > 0, "probed dim should be positive");

        let queries = embedder.embed_queries(&["hello"]).expect("embed query");
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].len(), embedder.dim());

        let passages = embedder
            .embed_passages(&["first doc", "second doc"])
            .expect("embed passages");
        assert_eq!(passages.len(), 2);
        assert_eq!(passages[0].len(), embedder.dim());
    }
}
