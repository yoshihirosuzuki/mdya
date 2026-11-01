//! Tests that fetch the real models from Hugging Face into a temporary
//! directory and run them. They are ignored by default, because they need the
//! network and fetch a few hundred MB. Run them by hand, in release mode so
//! that the models run at their real speed:
//! `cargo test --release --lib real_model -- --ignored --test-threads=1 --skip measure_chunks_per_call`.
//! The all-MiniLM-L6-v2 test also needs Ollama running at its default
//! endpoint with `all-minilm` pulled, to compare with another implementation
//! of the same model; it was checked with Ollama 0.40.13.
//!
//! `measure_chunks_per_call` prints the measurements behind the models'
//! chunks per call, and checks nothing. It reads this process's memory with
//! `ps`, so it runs on macOS and Linux:
//! `cargo test --release --lib measure_chunks_per_call -- --ignored --nocapture`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::embedding::{Embedding, EmbeddingError, EmbeddingSettings, create};
use crate::types::EmbeddingVector;

fn embedding(model: &str, cache_dir: &Path) -> Arc<dyn Embedding> {
    create(EmbeddingSettings {
        model: model.to_owned(),
        model_cache_dir: cache_dir.to_owned(),
        hf_token: None,
        ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
    })
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

/// Checks that a query is closer to the text that answers it than to an
/// unrelated one, and that the vectors have length 1.
async fn assert_relates(embedding: &dyn Embedding, query: &str, answer: &str, unrelated: &str) {
    let query = embedding.embed_query(query).await.unwrap();
    let chunks = embedding.embed_chunks(&[answer, unrelated]).await.unwrap();
    let to_answer = cosine_similarity(&query, &chunks[0].vector);
    let to_unrelated = cosine_similarity(&query, &chunks[1].vector);
    assert!(
        to_answer > to_unrelated + 0.1,
        "{to_answer} is not clearly above {to_unrelated}"
    );
    for vector in [&query, &chunks[0].vector] {
        let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((length - 1.0).abs() < 1e-4, "length {length}");
    }
}

/// Checks that a chunk longer than the context length is embedded from its
/// beginning and marked so, and that such a query is refused.
async fn assert_long_text_handling(embedding: &dyn Embedding, long: &str) {
    let chunks = embedding.embed_chunks(&["short", long]).await.unwrap();
    assert!(chunks[0].covers_whole_chunk);
    assert!(!chunks[1].covers_whole_chunk);
    assert!(matches!(
        embedding.embed_query(long).await,
        Err(EmbeddingError::QueryExceedsContextLength)
    ));
}

#[tokio::test]
#[ignore = "needs the network: fetches cl-nagoya/ruri-v3-30m"]
async fn ruri_v3_30m_relates_texts_and_handles_long_ones() {
    let cache = tempfile::tempdir().unwrap();
    let embedding = embedding("cl-nagoya/ruri-v3-30m", cache.path());

    assert_eq!(embedding.model().await.unwrap().dimension, 256);
    assert_relates(
        embedding.as_ref(),
        "瑠璃色はどんな色？",
        "瑠璃色は、紫みを帯びた濃い青である。",
        "プルリクエストを出す前に、手元で検査を通す。",
    )
    .await;
    // Far more than 8192 tokens.
    assert_long_text_handling(
        embedding.as_ref(),
        &"瑠璃色は紫みを帯びた濃い青である。".repeat(2000),
    )
    .await;
}

#[tokio::test]
#[ignore = "needs the network (fetches sentence-transformers/all-MiniLM-L6-v2) and Ollama with all-minilm"]
async fn all_minilm_l6_v2_matches_ollama_and_handles_long_texts() {
    let cache = tempfile::tempdir().unwrap();
    let embedding = embedding("sentence-transformers/all-MiniLM-L6-v2", cache.path());
    let texts = [
        "What color is lapis lazuli?",
        "Lapis lazuli is a deep blue stone.",
        "Run the checks before opening a pull request.",
    ];

    let ours = embedding.embed_chunks(&texts).await.unwrap();
    let ollamas = ollama_vectors("all-minilm", &texts).await;

    assert_eq!(embedding.model().await.unwrap().dimension, 384);
    for (ours, theirs) in ours.iter().zip(&ollamas) {
        let similarity = cosine_similarity(&ours.vector, theirs);
        assert!(
            similarity > 0.99,
            "similarity to Ollama's vector: {similarity}"
        );
    }
    assert_relates(embedding.as_ref(), texts[0], texts[1], texts[2]).await;
    // Far more than 256 tokens.
    assert_long_text_handling(embedding.as_ref(), &"lapis lazuli is blue ".repeat(300)).await;
}

/// The vectors that Ollama makes for `texts` with `model`.
async fn ollama_vectors(model: &str, texts: &[&str]) -> Vec<EmbeddingVector> {
    #[derive(serde::Deserialize)]
    struct Answer {
        embeddings: Vec<EmbeddingVector>,
    }
    let answer: Answer = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post("http://127.0.0.1:11434/api/embed")
        .json(&serde_json::json!({"model": model, "input": texts}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    answer.embeddings
}

/// Prints, for each number of chunks per call, the time of one call, the
/// time per chunk, and the growth of this process's resident memory during
/// the call, for choosing the models' chunks per call. Chunks are about 700
/// characters.
#[tokio::test]
#[ignore = "needs the network; measures, and checks nothing"]
async fn measure_chunks_per_call() {
    let cache = tempfile::tempdir().unwrap();
    for (model, text) in [
        (
            "cl-nagoya/ruri-v3-30m",
            "瑠璃色は紫みを帯びた濃い青である。".repeat(41),
        ),
        (
            "sentence-transformers/all-MiniLM-L6-v2",
            "Lapis lazuli is a deep blue stone used since antiquity. ".repeat(12),
        ),
    ] {
        let embedding = embedding(model, cache.path());
        embedding.model().await.unwrap();
        for count in [1, 4, 8, 16, 32, 64] {
            let texts: Vec<String> = (0..count).map(|i| format!("{i} {text}")).collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            let before = resident_kib();
            let done = Arc::new(AtomicBool::new(false));
            let sampler = std::thread::spawn({
                let done = done.clone();
                move || {
                    let mut peak = 0;
                    while !done.load(Ordering::SeqCst) {
                        peak = peak.max(resident_kib());
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    peak
                }
            });
            let started = Instant::now();
            embedding.embed_chunks(&texts).await.unwrap();
            let elapsed = started.elapsed();
            done.store(true, Ordering::SeqCst);
            let peak = sampler.join().unwrap();
            eprintln!(
                "{model} chunks={count} call={:.3}s per_chunk={:.1}ms memory_growth={}MiB",
                elapsed.as_secs_f64(),
                elapsed.as_secs_f64() * 1000.0 / count as f64,
                peak.saturating_sub(before) / 1024
            );
        }
    }
}

/// This process's resident memory, in KiB, from `ps`.
fn resident_kib() -> u64 {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}
