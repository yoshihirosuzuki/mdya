//! Tests against a real Ollama. They are ignored by default, because they need
//! Ollama running at its default endpoint with `all-minilm` pulled
//! (`ollama pull all-minilm`). Run them by hand with
//! `cargo test --lib real_ollama -- --ignored`. The expected answers were
//! checked with Ollama 0.40.13.

use std::error::Error as _;
use std::sync::Arc;

use crate::embedding::{Embedding, EmbeddingError, EmbeddingSettings, create};

const ENDPOINT: &str = "http://127.0.0.1:11434";

fn embedding(model: &str) -> Arc<dyn Embedding> {
    create(EmbeddingSettings {
        model: model.to_owned(),
        // Ollama keeps its own models; `model_cache_dir` is not called.
        model_cache_dir: Arc::new(|| panic!("model_cache_dir was called")),
        hf_token: None,
        ollama_endpoint: ENDPOINT.to_owned(),
    })
}

/// Far longer than all-minilm's context length of 256 tokens.
fn long_text() -> String {
    "word ".repeat(2000)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

#[tokio::test]
#[ignore = "needs Ollama at http://127.0.0.1:11434 with all-minilm pulled"]
async fn learns_the_dimension_and_tells_long_chunks_and_queries_apart() {
    let embedding = embedding("ollama:all-minilm");
    let long = long_text();

    let model = embedding.model().await.unwrap();
    let chunks = embedding
        .embed_chunks(&["Why is the sky blue?", &long])
        .await
        .unwrap();
    let query = embedding.embed_query("Why is the sky blue?").await.unwrap();
    let long_query = embedding.embed_query(&long).await;

    assert_eq!(model.dimension, 384);
    assert!(chunks[0].covers_whole_chunk);
    assert!(!chunks[1].covers_whole_chunk);
    assert_eq!(chunks[1].vector.len(), 384);
    // The same text makes the same vector as a chunk and as a query, since
    // Ollama gets both the same way. Embedding it with another input in the
    // same request may change the last digits, so the vectors are compared
    // by direction.
    assert!(cosine_similarity(&chunks[0].vector, &query) > 0.999);
    assert!(matches!(
        long_query,
        Err(EmbeddingError::QueryExceedsContextLength)
    ));
}

#[tokio::test]
#[ignore = "needs Ollama at http://127.0.0.1:11434"]
async fn reports_a_model_that_ollama_does_not_have() {
    let embedding = embedding("ollama:no-such-model-for-mdya-tests");

    let error = embedding.model().await.unwrap_err();

    assert!(matches!(error, EmbeddingError::ModelUnavailable { .. }));
    assert_eq!(
        error.source().unwrap().to_string(),
        "Ollama answered 404 Not Found: model 'no-such-model-for-mdya-tests' not found"
    );
}
