//! Embedder abstract layer.
//!
//! Two implementations currently ship: `RuriV3_30m` (default, on-device
//! `cl-nagoya/ruri-v3-30m`) and `OllamaEmbedder` (selected by an
//! `ollama:<model>` value in `config.yml`). Alternative backends plug in
//! behind the same trait.
//! The trait freezes only the input/output concepts (query/doc distinction +
//! `model_id` / `dim` accessors); concrete batching, threading, and pooling
//! decisions are encapsulated inside each impl.

mod cache;
mod ollama;
mod ruri_v3;

use std::sync::Arc;

pub use cache::{ModelCache, ModelCacheError};
pub use ollama::{OLLAMA_PREFIX, OllamaEmbedder};
pub use ruri_v3::{
    HIDDEN_SIZE as RURI_V3_30M_DIM, RURI_V3_30M_MODEL_ID, RURI_V3_30M_REVISION, RuriV3_30m,
};

use thiserror::Error;

/// Stable contract every embedding backend implements.
///
/// All implementations MUST:
/// - return one row per input text in the same order,
/// - return vectors whose length equals [`Embedder::dim`].
///
/// Prefix handling is backend-specific: `RuriV3_30m` applies retrieval
/// prefixes internally; `OllamaEmbedder` delegates prefix responsibility to the
/// Ollama model template. Callers always pass plain text.
pub trait Embedder: Send + Sync {
    /// Stable model identifier stored on every chunk as the actual pin
    /// (`chunks.embedding_model`). MUST equal the value declared in
    /// `config.yml`'s `embedding.model`.
    fn model_id(&self) -> &str;

    /// Vector dimension exposed to the LanceDB schema
    /// (`FixedSizeList<Float32, dim>`).
    fn dim(&self) -> usize;

    /// Embed a batch of search-query strings. Backend-specific prefix handling
    /// applies (see trait-level note).
    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Embed a batch of passage (chunk body) strings. Backend-specific prefix
    /// handling applies (see trait-level note).
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// Which concrete backend a `config.yml` `embedding.model` string selects.
/// Separated from [`build_embedder`] so the dispatch decision is a pure,
/// I/O-free function the tests can exercise without constructing an embedder
/// (the Ruri path downloads a model, the Ollama path probes a server).
enum ModelKind {
    /// The on-device default `cl-nagoya/ruri-v3-30m`.
    Ruri,
    /// An `ollama:<name>` backend.
    Ollama,
}

fn classify_model(model: &str) -> Option<ModelKind> {
    if model == RURI_V3_30M_MODEL_ID {
        Some(ModelKind::Ruri)
    } else if model.starts_with(OLLAMA_PREFIX) {
        Some(ModelKind::Ollama)
    } else {
        None
    }
}

/// Construct the embedder selected by `config.yml`'s `embedding.model`.
/// The default `cl-nagoya/ruri-v3-30m` loads on-device via `cache`; an
/// `ollama:<model>` value builds an [`OllamaEmbedder`] that talks to a local
/// Ollama server and ignores `cache` (Ollama owns the model lifecycle
/// server-side). An unrecognized model string is a config error.
pub async fn build_embedder(
    model: &str,
    cache: &ModelCache,
) -> Result<Arc<dyn Embedder>, EmbedError> {
    match classify_model(model) {
        Some(ModelKind::Ruri) => Ok(Arc::new(RuriV3_30m::new(cache).await?)),
        Some(ModelKind::Ollama) => Ok(Arc::new(OllamaEmbedder::new(model).await?)),
        None => Err(EmbedError::UnsupportedModel(model.to_string())),
    }
}

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error(
        "unsupported embedding model '{0}': expected 'cl-nagoya/ruri-v3-30m' or an 'ollama:<model>' value"
    )]
    UnsupportedModel(String),

    #[error("ollama backend: {0}")]
    Ollama(String),

    #[error("model cache: {0}")]
    Cache(#[from] ModelCacheError),

    #[error("read model file: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse config.json: {0}")]
    ConfigJson(#[from] serde_json::Error),

    // TODO: when retry-policy dispatch needs to classify these errors (e.g.
    // recoverable vs not), replace stringly-typed variants with a structured
    // enum.
    #[error("tokenizer: {0}")]
    Tokenizer(String),

    #[error("model graph: {0}")]
    Candle(#[from] candle_core::Error),

    // TODO: same condition as the `Tokenizer` TODO above. `Forward` currently
    // covers shape invariants and `to_vec2` conversion only.
    #[error("forward pass: {0}")]
    Forward(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_model_recognizes_the_ruri_default() {
        assert!(matches!(
            classify_model(RURI_V3_30M_MODEL_ID),
            Some(ModelKind::Ruri)
        ));
    }

    #[test]
    fn classify_model_recognizes_an_ollama_prefix() {
        assert!(matches!(
            classify_model("ollama:nomic-embed-text"),
            Some(ModelKind::Ollama)
        ));
    }

    #[test]
    fn classify_model_rejects_an_unknown_model() {
        assert!(classify_model("some/other-model").is_none());
    }

    #[test]
    fn build_embedder_rejects_an_unsupported_model() {
        // The unsupported branch errors before the cache is read; the temp dir
        // is only needed to construct a `ModelCache` (which performs no I/O).
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cache = ModelCache::new(tmp.path()).expect("cache");
        let result = futures::executor::block_on(build_embedder("some/other-model", &cache));
        // `Arc<dyn Embedder>` is not `Debug`, so match instead of `expect_err`.
        assert!(matches!(
            result,
            Err(EmbedError::UnsupportedModel(ref m)) if m == "some/other-model"
        ));
    }
}
