//! `EmbeddingGemmaEmbedder` — `google/embeddinggemma-300m` via the inline
//! `embeddinggemma_model` module.
//!
//! A Gemma3 bidirectional sentence encoder (768-dim). The model is **gated** on
//! Hugging Face: using it requires accepting Google's Gemma terms once on the
//! model page and providing an access token (`HF_TOKEN` or `hf auth login`).
//! This adapter fetches the five checkpoint files through [`ModelCache`] and
//! wraps the crate's network-free encoder, applying EmbeddingGemma's distinct
//! query / document task prompts (sourced from the preset registry).

use super::embeddinggemma_model::{EmbeddingGemma, ModelFiles};

use super::{EmbedError, Embedder, ModelCache, RetrievalPrefixes, find_on_device_preset};

pub const EMBEDDINGGEMMA_MODEL_ID: &str = "google/embeddinggemma-300m";

/// Pinned commit hash of `google/embeddinggemma-300m` on Hugging Face,
/// captured from its model API (`main`) when this embedder was added
/// (2026-06-16). Bumped only via a deliberate version change.
pub const EMBEDDINGGEMMA_REVISION: &str = "57c266a740f537b4dc058e1b0cda161fd15afa75";

/// Output vector dimension. Re-exported as `EMBEDDINGGEMMA_DIM` from the
/// `embedding` module so the preset registry and schema-dim resolver can pin
/// the LanceDB `FixedSizeList` without instantiating the embedder.
pub const HIDDEN_SIZE: usize = 768;

pub struct EmbeddingGemmaEmbedder {
    inner: EmbeddingGemma,
    /// Query / document task prompts, sourced from this model's preset entry.
    prefixes: RetrievalPrefixes,
}

impl EmbeddingGemmaEmbedder {
    /// Fetch the checkpoint files via `cache` and load the encoder. The gated
    /// download needs a Hugging Face token (see [`ModelCache::new`]); without
    /// one the fetch fails with an auth error.
    pub async fn new(cache: &ModelCache) -> Result<Self, EmbedError> {
        let config = cache
            .fetch_model_file(
                EMBEDDINGGEMMA_MODEL_ID,
                EMBEDDINGGEMMA_REVISION,
                "config.json",
            )
            .await?;
        let tokenizer = cache
            .fetch_model_file(
                EMBEDDINGGEMMA_MODEL_ID,
                EMBEDDINGGEMMA_REVISION,
                "tokenizer.json",
            )
            .await?;
        let weights = cache
            .fetch_model_file(
                EMBEDDINGGEMMA_MODEL_ID,
                EMBEDDINGGEMMA_REVISION,
                "model.safetensors",
            )
            .await?;
        let dense2 = cache
            .fetch_model_file(
                EMBEDDINGGEMMA_MODEL_ID,
                EMBEDDINGGEMMA_REVISION,
                "2_Dense/model.safetensors",
            )
            .await?;
        let dense3 = cache
            .fetch_model_file(
                EMBEDDINGGEMMA_MODEL_ID,
                EMBEDDINGGEMMA_REVISION,
                "3_Dense/model.safetensors",
            )
            .await?;

        let inner = EmbeddingGemma::load(&ModelFiles {
            config: &config,
            tokenizer: &tokenizer,
            weights: &weights,
            dense2: &dense2,
            dense3: &dense3,
        })
        .map_err(|e| EmbedError::EmbeddingGemma(e.to_string()))?;

        let prefixes = find_on_device_preset(EMBEDDINGGEMMA_MODEL_ID)
            .expect("embeddinggemma-300m must be registered in ON_DEVICE_PRESETS")
            .prefixes;

        Ok(Self { inner, prefixes })
    }

    fn embed_with_prefix(&self, prefixed: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
        self.inner
            .embed(&refs)
            .map_err(|e| EmbedError::EmbeddingGemma(e.to_string()))
    }
}

impl Embedder for EmbeddingGemmaEmbedder {
    fn model_id(&self) -> &str {
        EMBEDDINGGEMMA_MODEL_ID
    }

    fn dim(&self) -> usize {
        HIDDEN_SIZE
    }

    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefixed: Vec<String> = texts.iter().map(|t| self.prefixes.apply_query(t)).collect();
        self.embed_with_prefix(&prefixed)
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| self.prefixes.apply_passage(t))
            .collect();
        self.embed_with_prefix(&prefixed)
    }
}
