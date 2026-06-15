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

/// Retrieval prefixes a model prepends to query vs passage text before
/// embedding. The query/passage asymmetry is model-specific — `ruri-v3-30m`
/// uses Japanese retrieval prefixes, a plain sentence-transformer uses none —
/// so wrapping the pair lets every embedder apply it through one uniform seam.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetrievalPrefixes {
    query: &'static str,
    passage: &'static str,
}

impl RetrievalPrefixes {
    pub(crate) const fn new(query: &'static str, passage: &'static str) -> Self {
        Self { query, passage }
    }

    /// Prepend the query prefix to `text`.
    pub(crate) fn apply_query(&self, text: &str) -> String {
        format!("{}{text}", self.query)
    }

    /// Prepend the passage prefix to `text`.
    pub(crate) fn apply_passage(&self, text: &str) -> String {
        format!("{}{text}", self.passage)
    }
}

/// Static metadata for one on-device embedding model. [`ON_DEVICE_PRESETS`] is
/// the single source of recognized on-device models; adding a model is a new
/// entry there plus its constructor arm in [`build_embedder`].
pub(crate) struct OnDevicePreset {
    /// Repo id. Doubles as the `config.yml` `embedding.model` value and the
    /// per-chunk DB pin (`chunks.embedding_model`).
    model_id: &'static str,
    /// Query/passage retrieval prefixes this model expects.
    prefixes: RetrievalPrefixes,
}

/// Recognized on-device presets. The Ollama backend is intentionally absent:
/// it accepts any `ollama:<model>` name, so it is matched by prefix in
/// [`classify_model`] rather than enumerated here.
const ON_DEVICE_PRESETS: &[OnDevicePreset] = &[OnDevicePreset {
    model_id: RURI_V3_30M_MODEL_ID,
    prefixes: RetrievalPrefixes::new("検索クエリ: ", "検索文書: "),
}];

/// Look up the on-device preset whose `model_id` equals `model`.
pub(crate) fn find_on_device_preset(model: &str) -> Option<&'static OnDevicePreset> {
    ON_DEVICE_PRESETS
        .iter()
        .find(|preset| preset.model_id == model)
}

/// Whether the config layer accepts `model`: a recognized on-device preset or
/// an `ollama:<model>` value. The allowlist lives only here so the config
/// layer has a single source to validate against.
pub fn is_supported_model(model: &str) -> bool {
    classify_model(model).is_some()
}

/// The recognized on-device model ids, for building "supported models" hints
/// in config validation errors.
pub fn on_device_model_ids() -> Vec<&'static str> {
    ON_DEVICE_PRESETS
        .iter()
        .map(|preset| preset.model_id)
        .collect()
}

/// Which concrete backend a `config.yml` `embedding.model` string selects.
/// Separated from [`build_embedder`] so the dispatch decision is a pure,
/// I/O-free function the tests can exercise without constructing an embedder
/// (the on-device path downloads a model, the Ollama path probes a server).
enum ModelKind {
    /// A recognized on-device preset from [`ON_DEVICE_PRESETS`]. Only one
    /// on-device architecture ships today, so the matched preset carries no
    /// payload; a second preset will reintroduce it to drive arch dispatch.
    OnDevice,
    /// An `ollama:<name>` backend.
    Ollama,
}

fn classify_model(model: &str) -> Option<ModelKind> {
    if find_on_device_preset(model).is_some() {
        Some(ModelKind::OnDevice)
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
    ollama_endpoint: &str,
    cache: &ModelCache,
) -> Result<Arc<dyn Embedder>, EmbedError> {
    match classify_model(model) {
        // Only one on-device architecture ships today (ruri-v3-30m). A second
        // on-device preset will reintroduce the preset payload on
        // `ModelKind::OnDevice` and match on it here to select the
        // architecture-specific constructor.
        Some(ModelKind::OnDevice) => Ok(Arc::new(RuriV3_30m::new(cache).await?)),
        Some(ModelKind::Ollama) => Ok(Arc::new(OllamaEmbedder::new(model, ollama_endpoint).await?)),
        None => Err(EmbedError::UnsupportedModel(model.to_string())),
    }
}

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error(
        "unsupported embedding model '{0}': expected a recognized on-device model or an 'ollama:<model>' value"
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
            Some(ModelKind::OnDevice)
        ));
    }

    #[test]
    fn find_on_device_preset_returns_ruri_with_its_prefixes() {
        let preset = find_on_device_preset(RURI_V3_30M_MODEL_ID).expect("ruri preset registered");
        assert_eq!(preset.model_id, RURI_V3_30M_MODEL_ID);
        assert_eq!(preset.prefixes.apply_query("x"), "検索クエリ: x");
        assert_eq!(preset.prefixes.apply_passage("y"), "検索文書: y");
    }

    #[test]
    fn find_on_device_preset_is_none_for_an_ollama_value() {
        // `ollama:` targets are matched by prefix, never enumerated as presets.
        assert!(find_on_device_preset("ollama:nomic-embed-text").is_none());
    }

    #[test]
    fn is_supported_model_accepts_presets_and_ollama_rejects_unknown() {
        assert!(is_supported_model(RURI_V3_30M_MODEL_ID));
        assert!(is_supported_model("ollama:nomic-embed-text"));
        assert!(!is_supported_model("some/other-model"));
    }

    #[test]
    fn on_device_model_ids_lists_the_ruri_default() {
        assert!(on_device_model_ids().contains(&RURI_V3_30M_MODEL_ID));
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
        // The unsupported branch errors before either input is touched; the
        // endpoint placeholder satisfies the new signature without reaching
        // out to a real Ollama process.
        let result = futures::executor::block_on(build_embedder(
            "some/other-model",
            "http://127.0.0.1:11434",
            &cache,
        ));
        // `Arc<dyn Embedder>` is not `Debug`, so match instead of `expect_err`.
        assert!(matches!(
            result,
            Err(EmbedError::UnsupportedModel(ref m)) if m == "some/other-model"
        ));
    }
}
