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
//! mdya does NOT enforce a loopback endpoint. The protected value
//! ("inference stays on-device") is guaranteed by this backend being opt-in,
//! not by a code guard. `OLLAMA_HOST` is honoured verbatim, defaulting to the
//! loopback address Ollama itself binds.
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

/// Default endpoint when `OLLAMA_HOST` is unset — Ollama binds
/// `127.0.0.1:11434` by default.
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

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
    pub async fn new(model_id: &str) -> Result<Self, EmbedError> {
        let model = parse_ollama_model(model_id)?;
        let bridge = EmbedBridge::spawn(resolve_base_url(), model);
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

/// Resolve the base URL from `OLLAMA_HOST` (Ollama's own env var), defaulting
/// to the loopback endpoint. No loopback enforcement.
fn resolve_base_url() -> String {
    match std::env::var("OLLAMA_HOST") {
        Ok(raw) if !raw.trim().is_empty() => normalize_base_url(raw.trim()),
        _ => DEFAULT_BASE_URL.to_string(),
    }
}

/// `OLLAMA_HOST` may be `host:port`, `scheme://host:port`, or a bare host.
/// Assume `http://` when no scheme is present and drop a trailing slash so
/// `{base}/api/embed` joins cleanly.
fn normalize_base_url(raw: &str) -> String {
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    with_scheme.trim_end_matches('/').to_string()
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
            body.trim()
        )));
    }
    let parsed: EmbedResponse = response
        .json()
        .await
        .map_err(|e| EmbedError::Ollama(format!("decode /api/embed response: {e}")))?;
    Ok(parsed.embeddings)
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
    fn normalize_base_url_adds_http_scheme_when_absent() {
        assert_eq!(
            normalize_base_url("127.0.0.1:11434"),
            "http://127.0.0.1:11434"
        );
    }

    #[test]
    fn normalize_base_url_keeps_explicit_scheme() {
        assert_eq!(
            normalize_base_url("https://gpu.lan:11434"),
            "https://gpu.lan:11434"
        );
    }

    #[test]
    fn normalize_base_url_drops_trailing_slash() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:11434/"),
            "http://127.0.0.1:11434"
        );
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
        let embedder = OllamaEmbedder::new("ollama:nomic-embed-text")
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
