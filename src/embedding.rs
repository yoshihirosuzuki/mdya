//! The `embedding` component: makes embedding vectors with the configured
//! embedding model.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{BoxError, ChunkEmbedding, EmbeddingModel, EmbeddingVector};

#[cfg(test)]
pub(crate) mod test_double;

/// What the component is created with. Not `Debug`: it holds a token.
pub struct EmbeddingSettings {
    /// The embedding model setting. It must name a supported model (see
    /// [`is_supported_model`]).
    pub model: String,
    pub model_cache_dir: PathBuf,
    pub hf_token: Option<String>,
    pub ollama_endpoint: String,
}

/// Creates the component. Nothing is downloaded or loaded until a function
/// first needs the model.
pub fn create(settings: EmbeddingSettings) -> Arc<dyn Embedding> {
    let _ = settings;
    todo!()
}

/// Whether the embedding model setting names a model this component supports.
/// Callable without creating the component.
pub fn is_supported_model(value: &str) -> bool {
    let _ = value;
    todo!()
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// The embedding model cannot be used. `source` tells why.
    #[error("the embedding model cannot be used")]
    ModelUnavailable { source: BoxError },
    /// The query is longer than the embedding model's context length. Only
    /// `embed_query` returns it.
    #[error("the query is longer than the embedding model's context length")]
    QueryExceedsContextLength,
}

#[async_trait]
pub trait Embedding: Send + Sync {
    /// The name of the embedding model, from the setting.
    fn model_name(&self) -> &str;
    /// How many chunks `embed_chunks` accepts in one call.
    fn max_chunks_per_call(&self) -> NonZeroUsize;
    /// The embedding model, with its dimension. May prepare the model.
    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError>;
    /// Makes the embedding vectors of chunk texts, in order. At most
    /// `max_chunks_per_call` texts per call.
    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError>;
    /// Makes the embedding vector of a search query.
    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError>;
}
