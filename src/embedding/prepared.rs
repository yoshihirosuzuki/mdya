//! The part of the component that every embedding model shares: preparing the
//! model when a function first needs it, and keeping the prepared model.

use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::OnceCell;

use super::{Embedding, EmbeddingError};
use crate::types::{ChunkEmbedding, EmbeddingModel, EmbeddingVector};

/// An embedding model that is ready to make embedding vectors.
#[async_trait]
pub(super) trait PreparedModel: Send + Sync {
    /// The dimension of the vectors the model makes.
    fn dimension(&self) -> usize;
    /// Makes the embedding vectors of chunk texts, in order. Returns only
    /// `EmbeddingError::ModelUnavailable`.
    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError>;
    /// Makes the embedding vector of a search query.
    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError>;
}

/// Prepares an embedding model: whatever it takes to make it ready, and to
/// learn its dimension by using it.
#[async_trait]
pub(super) trait ModelPreparation: Send + Sync {
    /// Returns only `EmbeddingError::ModelUnavailable`.
    async fn prepare(&self) -> Result<Arc<dyn PreparedModel>, EmbeddingError>;
}

/// The component: prepares the model when a function first needs it and keeps
/// it, with its dimension, for as long as the component lives. A preparation
/// that fails or panics keeps nothing, so the next call prepares again.
pub(super) struct PreparedOnFirstUse {
    name: String,
    max_chunks_per_call: NonZeroUsize,
    preparation: Box<dyn ModelPreparation>,
    prepared: OnceCell<Arc<dyn PreparedModel>>,
}

impl PreparedOnFirstUse {
    pub(super) fn new(
        name: String,
        max_chunks_per_call: NonZeroUsize,
        preparation: Box<dyn ModelPreparation>,
    ) -> Self {
        Self {
            name,
            max_chunks_per_call,
            preparation,
            prepared: OnceCell::new(),
        }
    }

    async fn prepared(&self) -> Result<&Arc<dyn PreparedModel>, EmbeddingError> {
        self.prepared
            .get_or_try_init(|| self.preparation.prepare())
            .await
    }
}

#[async_trait]
impl Embedding for PreparedOnFirstUse {
    fn model_name(&self) -> &str {
        &self.name
    }

    fn max_chunks_per_call(&self) -> NonZeroUsize {
        self.max_chunks_per_call
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        let prepared = self.prepared().await?;
        Ok(EmbeddingModel {
            name: self.name.clone(),
            dimension: prepared.dimension(),
        })
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prepared = self.prepared().await?;
        let embeddings = prepared.embed_chunks(texts).await?;
        check_count(texts.len(), embeddings.len())?;
        for embedding in &embeddings {
            check_dimension(prepared.dimension(), embedding.vector.len())?;
        }
        Ok(embeddings)
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        let prepared = self.prepared().await?;
        let vector = prepared.embed_query(query).await?;
        check_dimension(prepared.dimension(), vector.len())?;
        Ok(vector)
    }
}

/// The model returned a different number of vectors than the texts it got.
#[derive(Debug, thiserror::Error)]
#[error("the embedding model returned {returned} vectors for {given} texts")]
struct VectorCountMismatch {
    given: usize,
    returned: usize,
}

/// The model returned a vector whose dimension differs from the one learned
/// when it was prepared.
#[derive(Debug, thiserror::Error)]
#[error(
    "the embedding model returned a vector of dimension {returned}, not {prepared} as when it was prepared"
)]
struct DimensionMismatch {
    prepared: usize,
    returned: usize,
}

fn check_count(given: usize, returned: usize) -> Result<(), EmbeddingError> {
    if given == returned {
        return Ok(());
    }
    Err(EmbeddingError::ModelUnavailable {
        source: Box::new(VectorCountMismatch { given, returned }),
    })
}

fn check_dimension(prepared: usize, returned: usize) -> Result<(), EmbeddingError> {
    if prepared == returned {
        return Ok(());
    }
    Err(EmbeddingError::ModelUnavailable {
        source: Box::new(DimensionMismatch { prepared, returned }),
    })
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A prepared model that makes vectors of a fixed dimension, or of
    /// another length when told to.
    struct FixedModel {
        dimension: usize,
        returned_length: usize,
        covers_whole_chunk: bool,
        query_exceeds_context_length: bool,
        drops_the_last_vector: bool,
    }

    impl FixedModel {
        fn new(dimension: usize) -> Self {
            Self {
                dimension,
                returned_length: dimension,
                covers_whole_chunk: true,
                query_exceeds_context_length: false,
                drops_the_last_vector: false,
            }
        }
    }

    #[async_trait]
    impl PreparedModel for FixedModel {
        fn dimension(&self) -> usize {
            self.dimension
        }

        async fn embed_chunks(
            &self,
            texts: &[&str],
        ) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
            let mut embeddings: Vec<_> = texts
                .iter()
                .map(|text| ChunkEmbedding {
                    vector: vec![text.len() as f32; self.returned_length],
                    covers_whole_chunk: self.covers_whole_chunk,
                })
                .collect();
            if self.drops_the_last_vector {
                embeddings.pop();
            }
            Ok(embeddings)
        }

        async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
            if self.query_exceeds_context_length {
                return Err(EmbeddingError::QueryExceedsContextLength);
            }
            Ok(vec![query.len() as f32; self.returned_length])
        }
    }

    /// What one call to `prepare` does, instead of succeeding.
    enum Attempt {
        Fail,
        Panic,
    }

    /// Counts its calls and does what the next planned attempt says. After
    /// the planned attempts, it succeeds.
    struct PlannedPreparation {
        model: Arc<FixedModel>,
        attempts: Mutex<Vec<Attempt>>,
        calls: Arc<AtomicUsize>,
    }

    impl PlannedPreparation {
        fn new(model: FixedModel, mut attempts: Vec<Attempt>) -> (Self, Arc<AtomicUsize>) {
            attempts.reverse();
            let calls = Arc::new(AtomicUsize::new(0));
            let preparation = Self {
                model: Arc::new(model),
                attempts: Mutex::new(attempts),
                calls: calls.clone(),
            };
            (preparation, calls)
        }
    }

    #[async_trait]
    impl ModelPreparation for PlannedPreparation {
        async fn prepare(&self) -> Result<Arc<dyn PreparedModel>, EmbeddingError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Let other calls run, so that concurrent calls overlap.
            tokio::task::yield_now().await;
            let attempt = self.attempts.lock().unwrap().pop();
            match attempt {
                Some(Attempt::Fail) => Err(EmbeddingError::ModelUnavailable {
                    source: "the network is down".into(),
                }),
                Some(Attempt::Panic) => panic!("the preparation panicked"),
                None => Ok(self.model.clone()),
            }
        }
    }

    fn component(
        model: FixedModel,
        attempts: Vec<Attempt>,
    ) -> (Arc<dyn Embedding>, Arc<AtomicUsize>) {
        let (preparation, calls) = PlannedPreparation::new(model, attempts);
        let component = PreparedOnFirstUse::new(
            "test-model".to_owned(),
            NonZeroUsize::new(16).unwrap(),
            Box::new(preparation),
        );
        (Arc::new(component), calls)
    }

    #[tokio::test]
    async fn answers_the_name_and_the_limit_without_preparing_the_model() {
        let (embedding, calls) = component(FixedModel::new(4), vec![]);

        assert_eq!(embedding.model_name(), "test-model");
        assert_eq!(embedding.max_chunks_per_call().get(), 16);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn answers_the_model_by_name_and_the_dimension_learned_when_preparing() {
        let (embedding, calls) = component(FixedModel::new(4), vec![]);

        let model = embedding.model().await.unwrap();

        assert_eq!(
            model,
            EmbeddingModel {
                name: "test-model".to_owned(),
                dimension: 4,
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn prepares_the_model_once_and_keeps_it_for_later_calls() {
        let (embedding, calls) = component(FixedModel::new(4), vec![]);

        embedding.model().await.unwrap();
        embedding.embed_chunks(&["a"]).await.unwrap();
        embedding.embed_query("q").await.unwrap();
        embedding.model().await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // The preparation yields, so the tasks overlap even on one thread.
    #[tokio::test]
    async fn prepares_the_model_once_when_called_from_several_tasks_at_once() {
        let (embedding, calls) = component(FixedModel::new(4), vec![]);

        let tasks: Vec<_> = (0..8)
            .map(|index| {
                let embedding = embedding.clone();
                tokio::spawn(async move {
                    match index % 3 {
                        0 => embedding.model().await.map(|_| ()),
                        1 => embedding.embed_chunks(&["a", "b"]).await.map(|_| ()),
                        _ => embedding.embed_query("q").await.map(|_| ()),
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reports_why_the_model_cannot_be_used_and_prepares_again_on_the_next_call() {
        let (embedding, calls) = component(FixedModel::new(4), vec![Attempt::Fail]);

        let error = embedding.embed_query("q").await.unwrap_err();

        assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
        assert_eq!(error.source().unwrap().to_string(), "the network is down");
        assert!(embedding.embed_query("q").await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stays_usable_after_a_preparation_panics() {
        let (embedding, calls) = component(FixedModel::new(4), vec![Attempt::Panic]);

        let first = tokio::spawn({
            let embedding = embedding.clone();
            async move { embedding.model().await }
        })
        .await;

        assert!(first.unwrap_err().is_panic());
        assert_eq!(embedding.model().await.unwrap().dimension, 4);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn returns_the_chunk_vectors_in_order_with_whether_each_covers_the_whole_chunk() {
        let model = FixedModel {
            covers_whole_chunk: false,
            ..FixedModel::new(2)
        };
        let (embedding, _) = component(model, vec![]);

        let embeddings = embedding.embed_chunks(&["a", "bbb"]).await.unwrap();

        assert_eq!(
            embeddings,
            vec![
                ChunkEmbedding {
                    vector: vec![1.0, 1.0],
                    covers_whole_chunk: false,
                },
                ChunkEmbedding {
                    vector: vec![3.0, 3.0],
                    covers_whole_chunk: false,
                },
            ]
        );
    }

    #[tokio::test]
    async fn returns_no_vectors_for_no_chunks_without_preparing_the_model() {
        let (embedding, calls) = component(FixedModel::new(4), vec![Attempt::Fail]);

        assert_eq!(embedding.embed_chunks(&[]).await.unwrap(), Vec::new());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn passes_on_that_the_query_exceeds_the_context_length() {
        let model = FixedModel {
            query_exceeds_context_length: true,
            ..FixedModel::new(4)
        };
        let (embedding, _) = component(model, vec![]);

        assert!(matches!(
            embedding.embed_query("q").await,
            Err(EmbeddingError::QueryExceedsContextLength)
        ));
    }

    #[tokio::test]
    async fn treats_a_vector_of_another_dimension_as_the_model_being_unavailable() {
        let model = FixedModel {
            returned_length: 3,
            ..FixedModel::new(4)
        };
        let (embedding, _) = component(model, vec![]);

        let chunks = embedding.embed_chunks(&["a"]).await.unwrap_err();
        let query = embedding.embed_query("q").await.unwrap_err();

        for error in [chunks, query] {
            assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
            assert!(
                error
                    .source()
                    .unwrap()
                    .to_string()
                    .contains("dimension 3, not 4")
            );
        }
    }

    #[tokio::test]
    async fn treats_a_missing_chunk_vector_as_the_model_being_unavailable() {
        let model = FixedModel {
            drops_the_last_vector: true,
            ..FixedModel::new(4)
        };
        let (embedding, _) = component(model, vec![]);

        let error = embedding.embed_chunks(&["a", "b"]).await.unwrap_err();

        assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
        assert_eq!(
            error.source().unwrap().to_string(),
            "the embedding model returned 1 vectors for 2 texts"
        );
    }
}
