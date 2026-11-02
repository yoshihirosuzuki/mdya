//! The `embedding` component: makes embedding vectors with the configured
//! embedding model.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{BoxError, ChunkEmbedding, EmbeddingModel, EmbeddingVector};

use ollama::{HttpEmbedEndpoint, OllamaPreparation};
use on_device::{HubSettings, OnDevicePreparation};
use prepared::PreparedOnFirstUse;
use supported::SupportedModel;

mod ollama;
mod on_device;
mod prepared;
mod supported;
#[cfg(test)]
pub(crate) mod test_double;

/// What the component is created with. Not `Debug`: it holds a token.
pub struct EmbeddingSettings {
    /// The embedding model setting. It must name a supported model (see
    /// [`is_supported_model`]).
    pub model: String,
    /// Decides the embedding model cache directory and returns it, as an
    /// absolute path. It is called each time a model run on this PC is
    /// prepared, again after a preparation that failed or panicked, and never
    /// when Ollama runs the model. It is called from async code, so it should
    /// not block for long. An error it returns makes the model unavailable,
    /// as the source of [`EmbeddingError::ModelUnavailable`].
    pub model_cache_dir: Arc<dyn Fn() -> Result<PathBuf, BoxError> + Send + Sync>,
    pub hf_token: Option<String>,
    pub ollama_endpoint: String,
}

/// Creates the component. Nothing is downloaded or loaded until the model is
/// first needed. `model_cache_dir` is not called before then, nor ever for a
/// model that Ollama runs.
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
        SupportedModel::OnDevice(on_device_model) => {
            let preparation = OnDevicePreparation::new(
                on_device_model,
                settings.model_cache_dir,
                HubSettings::hugging_face(settings.hf_token),
            );
            Arc::new(PreparedOnFirstUse::new(
                settings.model,
                on_device::spec(on_device_model).max_chunks_per_call,
                Box::new(preparation),
            ))
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Settings whose `model_cache_dir` must not be called.
    fn settings(model: &str) -> EmbeddingSettings {
        EmbeddingSettings {
            model: model.to_owned(),
            model_cache_dir: Arc::new(|| panic!("model_cache_dir was called")),
            hf_token: None,
            ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("the cache directory cannot be decided")]
    struct NoCacheDir;

    /// Settings for a model run on this PC, whose cache directory cannot be
    /// decided. Counts the calls to `model_cache_dir` in `calls`.
    fn settings_without_a_cache_dir(calls: Arc<AtomicUsize>) -> EmbeddingSettings {
        EmbeddingSettings {
            model: "sentence-transformers/all-MiniLM-L6-v2".to_owned(),
            model_cache_dir: Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(Box::new(NoCacheDir))
            }),
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

    #[tokio::test]
    async fn does_not_call_model_cache_dir_when_ollama_runs_the_model() {
        // `settings` gives a `model_cache_dir` that panics when called.
        let embedding = create(EmbeddingSettings {
            // Nothing listens here, so preparing the model fails.
            ollama_endpoint: "http://127.0.0.1:9".to_owned(),
            ..settings("ollama:nomic-embed-text")
        });

        assert!(embedding.model().await.is_err());
    }

    #[tokio::test]
    async fn returns_an_error_of_model_cache_dir_as_the_source_of_an_unavailable_model() {
        let embedding = create(settings_without_a_cache_dir(Arc::default()));

        let error = embedding.model().await.unwrap_err();

        let EmbeddingError::ModelUnavailable { source } = error else {
            panic!("expected ModelUnavailable, got {error:?}");
        };
        assert!(source.is::<NoCacheDir>());
    }

    #[tokio::test]
    async fn calls_model_cache_dir_for_each_preparation_of_the_model() {
        let calls = Arc::new(AtomicUsize::new(0));
        let embedding = create(settings_without_a_cache_dir(calls.clone()));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        assert!(embedding.model().await.is_err());
        assert!(embedding.embed_query("query").await.is_err());

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    #[should_panic(expected = "names a supported model")]
    fn refuses_to_create_the_component_for_a_setting_that_names_no_supported_model() {
        create(settings("no-such-model"));
    }
}
