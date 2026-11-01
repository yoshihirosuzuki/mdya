//! Handing embedding to Ollama: preparing the model by learning its dimension
//! from Ollama, and telling chunks and queries that exceed the context length
//! apart.

use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;

use super::EmbeddingError;
use super::prepared::{ModelPreparation, PreparedModel};
use crate::types::{BoxError, ChunkEmbedding, EmbeddingVector};

mod http;

pub(super) use http::HttpEmbedEndpoint;

// Tests that talk to a real Ollama, run by hand.
#[cfg(test)]
mod real_ollama_tests;

/// How many chunks one call hands to Ollama.
#[expect(clippy::expect_used)]
pub(super) const MAX_CHUNKS_PER_CALL: NonZeroUsize = NonZeroUsize::new(32).expect("32 is not zero");

/// The text embedded once to learn the model's dimension.
const DIMENSION_PROBE: &str = "dimension";

/// One request to Ollama's embedding endpoint.
#[async_trait]
pub(super) trait EmbedEndpoint: Send + Sync {
    /// Embeds `inputs`, in order. With `truncate`, Ollama cuts each input
    /// that exceeds the context length to fit; without it, Ollama refuses the
    /// whole request when any input exceeds it.
    async fn embed(
        &self,
        inputs: &[&str],
        truncate: bool,
    ) -> Result<Vec<EmbeddingVector>, EmbedFailure>;
}

/// Why a request to Ollama failed.
#[derive(Debug)]
pub(super) enum EmbedFailure {
    /// An input exceeds the context length. Only without `truncate`.
    ExceedsContextLength,
    /// Ollama cannot be used. The source tells why.
    Unavailable(BoxError),
}

/// Makes the connection to Ollama. It may fail, for example when the TLS
/// configuration cannot be loaded, so it runs when the model is prepared,
/// not when the component is created.
pub(super) type Connect = Box<dyn Fn() -> Result<Arc<dyn EmbedEndpoint>, BoxError> + Send + Sync>;

/// Prepares a model that Ollama runs: connects, and learns the dimension by
/// embedding a short text.
pub(super) struct OllamaPreparation {
    connect: Connect,
}

impl OllamaPreparation {
    pub(super) fn new(connect: Connect) -> Self {
        Self { connect }
    }
}

#[async_trait]
impl ModelPreparation for OllamaPreparation {
    async fn prepare(&self) -> Result<Arc<dyn PreparedModel>, EmbeddingError> {
        let endpoint = (self.connect)().map_err(unavailable)?;
        let vectors = endpoint
            .embed(&[DIMENSION_PROBE], true)
            .await
            .map_err(EmbedFailure::into_error)?;
        let dimension = single_vector(vectors)?.len();
        if dimension == 0 {
            return Err(unavailable(Box::new(EmptyVector)));
        }
        Ok(Arc::new(OllamaModel {
            endpoint,
            dimension,
        }))
    }
}

/// A model that Ollama runs, with the dimension learned when it was prepared.
struct OllamaModel {
    endpoint: Arc<dyn EmbedEndpoint>,
    dimension: usize,
}

impl OllamaModel {
    /// Embeds one chunk on its own: whole when it fits the context length,
    /// otherwise its beginning that fits.
    async fn embed_one_chunk(&self, text: &str) -> Result<ChunkEmbedding, EmbeddingError> {
        let (vectors, covers_whole_chunk) = match self.endpoint.embed(&[text], false).await {
            Ok(vectors) => (vectors, true),
            Err(EmbedFailure::ExceedsContextLength) => {
                let vectors = self
                    .endpoint
                    .embed(&[text], true)
                    .await
                    .map_err(EmbedFailure::into_error)?;
                (vectors, false)
            }
            Err(failure) => return Err(failure.into_error()),
        };
        Ok(ChunkEmbedding {
            vector: single_vector(vectors)?,
            covers_whole_chunk,
        })
    }
}

#[async_trait]
impl PreparedModel for OllamaModel {
    fn dimension(&self) -> usize {
        self.dimension
    }

    // Ollama does not say which input exceeded the context length, only that
    // one did. So the chunks go together first, and one by one only when one
    // of them is too long.
    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        match self.endpoint.embed(texts, false).await {
            Ok(vectors) => Ok(vectors
                .into_iter()
                .map(|vector| ChunkEmbedding {
                    vector,
                    covers_whole_chunk: true,
                })
                .collect()),
            Err(EmbedFailure::ExceedsContextLength) => {
                let mut embeddings = Vec::with_capacity(texts.len());
                for text in texts {
                    embeddings.push(self.embed_one_chunk(text).await?);
                }
                Ok(embeddings)
            }
            Err(failure) => Err(failure.into_error()),
        }
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        match self.endpoint.embed(&[query], false).await {
            Ok(vectors) => single_vector(vectors),
            Err(EmbedFailure::ExceedsContextLength) => {
                Err(EmbeddingError::QueryExceedsContextLength)
            }
            Err(failure) => Err(failure.into_error()),
        }
    }
}

impl EmbedFailure {
    /// The error for a failure where exceeding the context length cannot
    /// happen or is not expected.
    fn into_error(self) -> EmbeddingError {
        match self {
            Self::ExceedsContextLength => unavailable(Box::new(UnexpectedContextLengthError)),
            Self::Unavailable(source) => unavailable(source),
        }
    }
}

/// The vector of a request with one input. Ollama returning another number of
/// vectors means it cannot be used as expected.
fn single_vector(vectors: Vec<EmbeddingVector>) -> Result<EmbeddingVector, EmbeddingError> {
    let returned = vectors.len();
    let mut vectors = vectors.into_iter();
    match (vectors.next(), vectors.next()) {
        (Some(vector), None) => Ok(vector),
        _ => Err(unavailable(Box::new(NotOneVector { returned }))),
    }
}

fn unavailable(source: BoxError) -> EmbeddingError {
    EmbeddingError::ModelUnavailable { source }
}

#[derive(Debug, thiserror::Error)]
#[error("Ollama returned {returned} embedding vectors for one input")]
struct NotOneVector {
    returned: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("Ollama returned an embedding vector with no values")]
struct EmptyVector;

#[derive(Debug, thiserror::Error)]
#[error("Ollama answered that the input exceeds the context length, although truncation was on")]
struct UnexpectedContextLengthError;

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::Mutex;

    use super::*;

    /// A request the double received.
    #[derive(Clone, Debug, PartialEq)]
    struct Request {
        inputs: Vec<String>,
        truncate: bool,
    }

    /// Stands in for Ollama. Inputs longer than `context_length` bytes exceed
    /// the context length; truncating keeps their first `context_length`
    /// bytes. The vector of a text is its length, repeated `dimension` times.
    struct TestEndpoint {
        dimension: usize,
        context_length: usize,
        unavailable: bool,
        requests: Mutex<Vec<Request>>,
    }

    impl TestEndpoint {
        fn new(dimension: usize, context_length: usize) -> Self {
            Self {
                dimension,
                context_length,
                unavailable: false,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }

        fn vector(&self, length: usize) -> EmbeddingVector {
            vec![length as f32; self.dimension]
        }
    }

    #[async_trait]
    impl EmbedEndpoint for TestEndpoint {
        async fn embed(
            &self,
            inputs: &[&str],
            truncate: bool,
        ) -> Result<Vec<EmbeddingVector>, EmbedFailure> {
            self.requests.lock().unwrap().push(Request {
                inputs: inputs.iter().map(|input| (*input).to_owned()).collect(),
                truncate,
            });
            if self.unavailable {
                return Err(EmbedFailure::Unavailable("Ollama is not running".into()));
            }
            let too_long = inputs.iter().any(|input| input.len() > self.context_length);
            if too_long && !truncate {
                return Err(EmbedFailure::ExceedsContextLength);
            }
            Ok(inputs
                .iter()
                .map(|input| self.vector(input.len().min(self.context_length)))
                .collect())
        }
    }

    async fn prepared(
        endpoint: Arc<TestEndpoint>,
    ) -> Result<Arc<dyn PreparedModel>, EmbeddingError> {
        let preparation = OllamaPreparation::new(Box::new(move || Ok(endpoint.clone())));
        preparation.prepare().await
    }

    #[tokio::test]
    async fn learns_the_dimension_by_embedding_a_short_text() {
        let endpoint = Arc::new(TestEndpoint::new(3, 100));

        let model = prepared(endpoint.clone()).await.unwrap();

        assert_eq!(model.dimension(), 3);
        assert_eq!(
            endpoint.requests(),
            vec![Request {
                inputs: vec![DIMENSION_PROBE.to_owned()],
                truncate: true,
            }]
        );
    }

    #[tokio::test]
    async fn reports_why_ollama_cannot_be_used_when_preparing() {
        let endpoint = Arc::new(TestEndpoint {
            unavailable: true,
            ..TestEndpoint::new(3, 100)
        });

        let error = prepared(endpoint).await.err().unwrap();

        assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
        assert_eq!(error.source().unwrap().to_string(), "Ollama is not running");
    }

    #[tokio::test]
    async fn reports_why_it_cannot_connect_to_ollama() {
        let preparation = OllamaPreparation::new(Box::new(|| Err("no TLS roots".into())));

        let error = preparation.prepare().await.err().unwrap();

        assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
        assert_eq!(error.source().unwrap().to_string(), "no TLS roots");
    }

    #[tokio::test]
    async fn embeds_chunks_that_fit_in_one_request_without_truncation() {
        let endpoint = Arc::new(TestEndpoint::new(2, 5));
        let model = prepared(endpoint.clone()).await.unwrap();

        let embeddings = model.embed_chunks(&["ab", "abcd"]).await.unwrap();

        assert_eq!(
            embeddings,
            vec![
                ChunkEmbedding {
                    vector: vec![2.0, 2.0],
                    covers_whole_chunk: true,
                },
                ChunkEmbedding {
                    vector: vec![4.0, 4.0],
                    covers_whole_chunk: true,
                },
            ]
        );
        assert_eq!(
            endpoint.requests()[1..],
            [Request {
                inputs: vec!["ab".to_owned(), "abcd".to_owned()],
                truncate: false,
            }]
        );
    }

    #[tokio::test]
    async fn embeds_the_beginning_of_a_chunk_that_exceeds_the_context_length() {
        let endpoint = Arc::new(TestEndpoint::new(2, 5));
        let model = prepared(endpoint.clone()).await.unwrap();

        let embeddings = model.embed_chunks(&["ab", "abcdefgh"]).await.unwrap();

        assert_eq!(
            embeddings,
            vec![
                ChunkEmbedding {
                    vector: vec![2.0, 2.0],
                    covers_whole_chunk: true,
                },
                ChunkEmbedding {
                    vector: vec![5.0, 5.0],
                    covers_whole_chunk: false,
                },
            ]
        );
        let request = |inputs: &[&str], truncate| Request {
            inputs: inputs.iter().map(|input| (*input).to_owned()).collect(),
            truncate,
        };
        assert_eq!(
            endpoint.requests()[1..],
            [
                request(&["ab", "abcdefgh"], false),
                request(&["ab"], false),
                request(&["abcdefgh"], false),
                request(&["abcdefgh"], true),
            ]
        );
    }

    #[tokio::test]
    async fn embeds_a_query_that_fits_the_context_length() {
        let endpoint = Arc::new(TestEndpoint::new(2, 5));
        let model = prepared(endpoint.clone()).await.unwrap();

        assert_eq!(model.embed_query("abc").await.unwrap(), vec![3.0, 3.0]);
        assert_eq!(
            endpoint.requests()[1..],
            [Request {
                inputs: vec!["abc".to_owned()],
                truncate: false,
            }]
        );
    }

    #[tokio::test]
    async fn refuses_a_query_that_exceeds_the_context_length_without_embedding_its_beginning() {
        let endpoint = Arc::new(TestEndpoint::new(2, 5));
        let model = prepared(endpoint.clone()).await.unwrap();

        let result = model.embed_query("abcdefgh").await;

        assert!(matches!(
            result,
            Err(EmbeddingError::QueryExceedsContextLength)
        ));
        assert!(
            endpoint
                .requests()
                .iter()
                .all(|request| !request.truncate || request.inputs == [DIMENSION_PROBE])
        );
    }

    #[tokio::test]
    async fn reports_why_ollama_cannot_be_used_when_embedding() {
        let endpoint = Arc::new(TestEndpoint::new(2, 5));
        let model = prepared(endpoint.clone()).await.unwrap();
        let unavailable = Arc::new(TestEndpoint {
            unavailable: true,
            ..TestEndpoint::new(2, 5)
        });
        let model_after_ollama_stopped = OllamaModel {
            endpoint: unavailable,
            dimension: model.dimension(),
        };

        let chunks = model_after_ollama_stopped
            .embed_chunks(&["a"])
            .await
            .unwrap_err();
        let query = model_after_ollama_stopped
            .embed_query("a")
            .await
            .unwrap_err();

        for error in [chunks, query] {
            assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
            assert_eq!(error.source().unwrap().to_string(), "Ollama is not running");
        }
    }

    /// Answers the requests with the planned answers, in order, whatever the
    /// inputs.
    struct PlannedEndpoint {
        answers: Mutex<Vec<Result<Vec<EmbeddingVector>, EmbedFailure>>>,
    }

    impl PlannedEndpoint {
        fn new(mut answers: Vec<Result<Vec<EmbeddingVector>, EmbedFailure>>) -> Arc<Self> {
            answers.reverse();
            Arc::new(Self {
                answers: Mutex::new(answers),
            })
        }
    }

    #[async_trait]
    impl EmbedEndpoint for PlannedEndpoint {
        async fn embed(
            &self,
            _inputs: &[&str],
            _truncate: bool,
        ) -> Result<Vec<EmbeddingVector>, EmbedFailure> {
            self.answers.lock().unwrap().pop().unwrap()
        }
    }

    fn assert_unavailable_because(error: EmbeddingError, reason: &str) {
        assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
        assert_eq!(error.source().unwrap().to_string(), reason);
    }

    #[tokio::test]
    async fn reports_an_answer_without_exactly_one_vector_for_one_input() {
        for vectors in [vec![], vec![vec![1.0, 2.0], vec![3.0, 4.0]]] {
            let reason = format!(
                "Ollama returned {} embedding vectors for one input",
                vectors.len()
            );
            let endpoint = PlannedEndpoint::new(vec![
                Ok(vectors.clone()),
                Ok(vectors.clone()),
                Err(EmbedFailure::ExceedsContextLength),
                Ok(vectors.clone()),
            ]);
            let preparation = OllamaPreparation::new(Box::new({
                let endpoint = endpoint.clone();
                move || Ok(endpoint.clone())
            }));
            let model = OllamaModel {
                endpoint,
                dimension: 2,
            };

            let probe = preparation.prepare().await.err().unwrap();
            let query = model.embed_query("q").await.unwrap_err();
            let one_chunk = model.embed_chunks(&["a"]).await.unwrap_err();

            assert_unavailable_because(probe, &reason);
            assert_unavailable_because(query, &reason);
            assert_unavailable_because(one_chunk, &reason);
        }
    }

    #[tokio::test]
    async fn reports_a_probe_answer_with_an_empty_vector() {
        let endpoint = PlannedEndpoint::new(vec![Ok(vec![vec![]])]);
        let preparation = OllamaPreparation::new(Box::new(move || Ok(endpoint.clone())));

        let error = preparation.prepare().await.err().unwrap();

        assert_unavailable_because(error, "Ollama returned an embedding vector with no values");
    }
}
