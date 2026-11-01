//! A test double for [`Embedding`], for the unit tests of the components that
//! use it.

// Parts of the double are for tests that the components using it have not
// written yet.
#![allow(dead_code)]

use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{Embedding, EmbeddingError};
use crate::types::{ChunkEmbedding, EmbeddingModel, EmbeddingVector};

/// Makes vectors without a model. The vector of a text depends only on the
/// text and the dimension (see [`TestEmbedding::vector_for`]), and covers the
/// whole chunk. Records every call.
pub(crate) struct TestEmbedding {
    model: EmbeddingModel,
    max_chunks_per_call: NonZeroUsize,
    failure: Option<EmbeddingFailure>,
    calls: Mutex<Vec<EmbeddingCall>>,
}

/// A failure the double returns instead of a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmbeddingFailure {
    /// `model`, `embed_chunks`, and `embed_query` return
    /// `EmbeddingError::ModelUnavailable`.
    ModelUnavailable,
    /// `embed_query` returns `EmbeddingError::QueryExceedsContextLength`.
    QueryExceedsContextLength,
}

/// A call the double received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EmbeddingCall {
    Model,
    EmbedChunks(Vec<String>),
    EmbedQuery(String),
}

impl TestEmbedding {
    /// A double for the model `name` with `dimension`, taking up to 16 chunks
    /// per call.
    pub(crate) fn new(name: &str, dimension: usize) -> Self {
        Self {
            model: EmbeddingModel {
                name: name.to_owned(),
                dimension,
            },
            max_chunks_per_call: NonZeroUsize::new(16).unwrap(),
            failure: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Takes up to `count` chunks per call. `embed_chunks` panics when given
    /// more, as a check on the caller.
    pub(crate) fn with_max_chunks_per_call(mut self, count: usize) -> Self {
        self.max_chunks_per_call = NonZeroUsize::new(count).unwrap();
        self
    }

    pub(crate) fn failing_with(mut self, failure: EmbeddingFailure) -> Self {
        self.failure = Some(failure);
        self
    }

    pub(crate) fn calls(&self) -> Vec<EmbeddingCall> {
        self.calls.lock().unwrap().clone()
    }

    /// The vector this double makes for `text`.
    pub(crate) fn vector_for(&self, text: &str) -> EmbeddingVector {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let seed = hasher.finish();
        (0..self.model.dimension)
            .map(|position| (seed.wrapping_add(position as u64) % 1000) as f32 / 1000.0)
            .collect()
    }

    fn record(&self, call: EmbeddingCall) {
        self.calls.lock().unwrap().push(call);
    }

    fn model_unavailable(&self) -> Result<(), EmbeddingError> {
        match self.failure {
            Some(EmbeddingFailure::ModelUnavailable) => Err(EmbeddingError::ModelUnavailable {
                source: "the test double's model is unavailable".into(),
            }),
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl Embedding for TestEmbedding {
    fn model_name(&self) -> &str {
        &self.model.name
    }

    fn max_chunks_per_call(&self) -> NonZeroUsize {
        self.max_chunks_per_call
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        self.record(EmbeddingCall::Model);
        self.model_unavailable()?;
        Ok(self.model.clone())
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        assert!(
            texts.len() <= self.max_chunks_per_call.get(),
            "embed_chunks got {} texts, more than max_chunks_per_call ({})",
            texts.len(),
            self.max_chunks_per_call
        );
        self.record(EmbeddingCall::EmbedChunks(
            texts.iter().map(|text| (*text).to_owned()).collect(),
        ));
        self.model_unavailable()?;
        Ok(texts
            .iter()
            .map(|text| ChunkEmbedding {
                vector: self.vector_for(text),
                covers_whole_chunk: true,
            })
            .collect())
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        self.record(EmbeddingCall::EmbedQuery(query.to_owned()));
        self.model_unavailable()?;
        if self.failure == Some(EmbeddingFailure::QueryExceedsContextLength) {
            return Err(EmbeddingError::QueryExceedsContextLength);
        }
        Ok(self.vector_for(query))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn makes_the_same_vector_for_the_same_text_and_records_the_calls() {
        let double = Arc::new(TestEmbedding::new("test-model", 4));
        let embedding: Arc<dyn Embedding> = double.clone();

        let chunks = embedding.embed_chunks(&["a", "b"]).await.unwrap();
        let query = embedding.embed_query("a").await.unwrap();

        assert_eq!(embedding.model_name(), "test-model");
        assert_eq!(
            embedding.model().await.unwrap(),
            EmbeddingModel {
                name: "test-model".to_owned(),
                dimension: 4
            }
        );
        assert_eq!(chunks[0].vector, query);
        assert_ne!(chunks[0].vector, chunks[1].vector);
        assert_eq!(chunks[0].vector.len(), 4);
        assert!(chunks.iter().all(|chunk| chunk.covers_whole_chunk));
        assert_eq!(
            double.calls(),
            vec![
                EmbeddingCall::EmbedChunks(vec!["a".to_owned(), "b".to_owned()]),
                EmbeddingCall::EmbedQuery("a".to_owned()),
                EmbeddingCall::Model,
            ]
        );
    }

    #[tokio::test]
    async fn returns_the_configured_failure() {
        let unavailable: Arc<dyn Embedding> =
            Arc::new(TestEmbedding::new("m", 2).failing_with(EmbeddingFailure::ModelUnavailable));
        let too_long: Arc<dyn Embedding> = Arc::new(
            TestEmbedding::new("m", 2).failing_with(EmbeddingFailure::QueryExceedsContextLength),
        );

        assert!(matches!(
            unavailable.embed_chunks(&["a"]).await,
            Err(EmbeddingError::ModelUnavailable { .. })
        ));
        assert!(matches!(
            too_long.embed_query("a").await,
            Err(EmbeddingError::QueryExceedsContextLength)
        ));
        assert!(too_long.embed_chunks(&["a"]).await.is_ok());
    }

    #[tokio::test]
    #[should_panic(expected = "more than max_chunks_per_call")]
    async fn panics_when_given_more_chunks_than_it_takes() {
        let embedding = TestEmbedding::new("m", 2).with_max_chunks_per_call(1);

        let _ = embedding.embed_chunks(&["a", "b"]).await;
    }
}
