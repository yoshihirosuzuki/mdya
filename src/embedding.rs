//! The `embedding` component: makes embedding vectors with the configured
//! embedding model.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{BoxError, ChunkEmbedding, EmbeddingModel, EmbeddingVector};

use ollama::{HttpEmbedEndpoint, OllamaPreparation};
use prepared::PreparedOnFirstUse;
use supported::SupportedModel;

mod ollama;
mod prepared;
mod supported;
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
///
/// Panics when the setting names no supported model: `config` hands over only
/// a setting that does.
pub fn create(settings: EmbeddingSettings) -> Arc<dyn Embedding> {
    #[expect(clippy::expect_used)]
    let model = SupportedModel::from_setting(&settings.model)
        .expect("config hands over only a setting that names a supported model");
    match model {
        SupportedModel::Ollama(ollama_model) => {
            let endpoint = settings.ollama_endpoint;
            let preparation = OllamaPreparation::new(Box::new(move || {
                HttpEmbedEndpoint::connect(&endpoint, &ollama_model)
            }));
            Arc::new(PreparedOnFirstUse::new(
                settings.model,
                ollama::MAX_CHUNKS_PER_CALL,
                Box::new(preparation),
            ))
        }
        SupportedModel::OnDevice(_) => todo!("the models run on this PC"),
    }
}

/// Whether the embedding model setting names a model this component supports:
/// one of the models in the fixed list, by its Hugging Face repository, or
/// `ollama:` followed by a model name of at least one character. Callable
/// without creating the component.
pub fn is_supported_model(value: &str) -> bool {
    SupportedModel::from_setting(value).is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(model: &str) -> EmbeddingSettings {
        EmbeddingSettings {
            model: model.to_owned(),
            model_cache_dir: PathBuf::from("not-used"),
            hf_token: None,
            ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
        }
    }

    #[test]
    fn tells_whether_a_setting_names_a_supported_model() {
        assert!(is_supported_model("cl-nagoya/ruri-v3-30m"));
        assert!(is_supported_model("google/embeddinggemma-300m"));
        assert!(is_supported_model("sentence-transformers/all-MiniLM-L6-v2"));
        assert!(is_supported_model("ollama:nomic-embed-text"));
        assert!(!is_supported_model("ollama:"));
        assert!(!is_supported_model("nomic-embed-text"));
    }

    #[test]
    fn answers_the_name_and_the_limit_of_a_model_that_ollama_runs() {
        let embedding = create(settings("ollama:nomic-embed-text"));

        assert_eq!(embedding.model_name(), "ollama:nomic-embed-text");
        assert_eq!(embedding.max_chunks_per_call(), ollama::MAX_CHUNKS_PER_CALL);
    }

    #[test]
    #[should_panic(expected = "names a supported model")]
    fn refuses_to_create_the_component_for_a_setting_that_names_no_supported_model() {
        create(settings("no-such-model"));
    }
}
