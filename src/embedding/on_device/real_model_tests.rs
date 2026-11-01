//! Tests that fetch the real models from Hugging Face and run them. They are
//! ignored by default, because they need the network and fetch up to 1.2 GB
//! per model. Run them by hand, in release mode so that the models run at
//! their real speed:
//! `cargo test --release --lib real_model -- --ignored --test-threads=1 --skip measure_chunks_per_call`.
//!
//! Two values can be given when the tests are built, as environment variables
//! of the `cargo test` command:
//!
//! - `MDYA_TEST_HF_TOKEN_FILE`: a file holding a Hugging Face token, for
//!   google/embeddinggemma-300m, whose files need permission and a token.
//!   Keep the file outside the repository, so that it is never committed.
//!   Without it, that test fails to fetch the model.
//! - `MDYA_TEST_MODEL_CACHE_DIR`: the directory to fetch the models to, kept
//!   between runs. Without it, each test fetches to a new temporary
//!   directory.
//!
//! The all-MiniLM-L6-v2 and embeddinggemma-300m tests also need Ollama running
//! at its default endpoint with `all-minilm` and `embeddinggemma` pulled, to
//! compare with another implementation of the same model; they were checked
//! with Ollama 0.40.13.
//!
//! `measure_chunks_per_call` prints the measurements behind the models'
//! chunks per call, and checks nothing. It reads this process's memory with
//! `ps`, so it runs on macOS and Linux:
//! `cargo test --release --lib measure_chunks_per_call -- --ignored --nocapture`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::embedding::{Embedding, EmbeddingError, EmbeddingSettings, create};
use crate::types::EmbeddingVector;

/// The model cache directory of a test, and the temporary directory to keep
/// alive while the test runs, if the cache is one.
struct Cache {
    dir: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

impl Cache {
    fn new() -> Self {
        match option_env!("MDYA_TEST_MODEL_CACHE_DIR") {
            Some(dir) => Self {
                dir: PathBuf::from(dir),
                _temporary: None,
            },
            None => {
                let temporary = tempfile::tempdir().unwrap();
                Self {
                    dir: temporary.path().to_owned(),
                    _temporary: Some(temporary),
                }
            }
        }
    }
}

/// The Hugging Face token in the file given when the tests were built.
fn hf_token() -> Option<String> {
    option_env!("MDYA_TEST_HF_TOKEN_FILE")
        .map(|path| std::fs::read_to_string(path).unwrap().trim().to_owned())
}

fn embedding(model: &str, cache: &Cache) -> Arc<dyn Embedding> {
    create(EmbeddingSettings {
        model: model.to_owned(),
        model_cache_dir: cache.dir.clone(),
        hf_token: hf_token(),
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
    let cache = Cache::new();
    let embedding = embedding("cl-nagoya/ruri-v3-30m", &cache);

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
    let cache = Cache::new();
    let embedding = embedding("sentence-transformers/all-MiniLM-L6-v2", &cache);
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

#[tokio::test]
#[ignore = "needs the network (fetches google/embeddinggemma-300m, with a token) and Ollama with embeddinggemma"]
async fn embeddinggemma_300m_matches_ollama_and_handles_long_texts() {
    let cache = Cache::new();
    let embedding = embedding("google/embeddinggemma-300m", &cache);
    let long = varied_text();
    let texts = [
        "What color is lapis lazuli?",
        "Lapis lazuli is a deep blue stone.",
        "Run the checks before opening a pull request.",
        &long,
    ];

    let ours = embedding.embed_chunks(&texts).await.unwrap();
    // Ollama adds no prompt, so it gets the texts with the document prompt.
    let prompted: Vec<String> = texts
        .iter()
        .map(|text| format!("title: none | text: {text}"))
        .collect();
    let prompted: Vec<&str> = prompted.iter().map(String::as_str).collect();
    let ollamas = ollama_vectors("embeddinggemma", &prompted).await;

    assert_eq!(embedding.model().await.unwrap().dimension, 768);
    assert!(ours.iter().all(|chunk| chunk.covers_whole_chunk));
    // Measured on 2026-11-01 against Ollama 0.40.13's `embeddinggemma`
    // (BF16): about 0.99998 for every text. With the sliding window taken as
    // 511 tokens on either side instead of 256, the long text gave 0.9895.
    for (ours, theirs) in ours.iter().zip(&ollamas) {
        let similarity = cosine_similarity(&ours.vector, theirs);
        assert!(
            similarity > 0.999,
            "similarity to Ollama's vector: {similarity}"
        );
    }
    assert_relates(embedding.as_ref(), texts[0], texts[1], texts[2]).await;
    // Far more than 2048 tokens.
    assert_long_text_handling(embedding.as_ref(), &"lapis lazuli is blue ".repeat(1000)).await;
}

/// About 4,300 characters of text that does not repeat a sentence: longer
/// than a token sees in embeddinggemma's sliding-window layers (256 tokens on
/// either side), and within its context length (2048 tokens), so that
/// comparing its vector checks those layers.
fn varied_text() -> String {
    let adjectives = ["red", "quiet", "old", "bright", "heavy", "narrow", "warm"];
    let nouns = [
        "bridge", "library", "garden", "engine", "river", "market", "tower", "violin", "harbor",
        "forest", "lamp",
    ];
    let places = [
        "station", "village", "castle", "museum", "valley", "factory", "island", "school",
        "temple", "bakery", "airport", "theater", "canyon",
    ];
    let verbs = ["painted", "repaired", "visited", "measured", "described"];
    (0..60)
        .map(|i| {
            format!(
                "{i}. The {} {} near the {} was {} by {} people in {}. ",
                adjectives[i % 7],
                nouns[i % 11],
                places[i % 13],
                verbs[i % 5],
                (i * 37) % 101,
                1900 + (i * 13) % 120
            )
        })
        .collect()
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

/// How many calls are measured for each number of chunks per call.
const CALLS_MEASURED: u32 = 3;

/// Prints, for each number of chunks per call, the mean time of a call and
/// of a chunk over `CALLS_MEASURED` calls, and the largest growth of this
/// process's resident memory during one of them, for choosing the models'
/// chunks per call. Chunks are about 700 characters.
#[tokio::test]
#[ignore = "needs the network; measures, and checks nothing"]
async fn measure_chunks_per_call() {
    let cache = Cache::new();
    for (model, text) in [
        (
            "cl-nagoya/ruri-v3-30m",
            "瑠璃色は紫みを帯びた濃い青である。".repeat(41),
        ),
        (
            "sentence-transformers/all-MiniLM-L6-v2",
            "Lapis lazuli is a deep blue stone used since antiquity. ".repeat(12),
        ),
        (
            "google/embeddinggemma-300m",
            "Lapis lazuli is a deep blue stone used since antiquity. ".repeat(12),
        ),
    ] {
        let embedding = embedding(model, &cache);
        embedding.model().await.unwrap();
        for count in [1, 4, 8, 16, 32, 64] {
            let texts: Vec<String> = (0..count).map(|i| format!("{i} {text}")).collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            let mut total = Duration::ZERO;
            let mut growth = 0;
            for _ in 0..CALLS_MEASURED {
                let (elapsed, grown) = measure_call(embedding.as_ref(), &texts).await;
                total += elapsed;
                growth = growth.max(grown);
            }
            let call = total.as_secs_f64() / f64::from(CALLS_MEASURED);
            eprintln!(
                "{model} chunks={count} call={call:.3}s per_chunk={:.1}ms memory_growth={}MiB",
                call * 1000.0 / count as f64,
                growth / 1024
            );
        }
    }
}

/// The time of one call that embeds `texts`, and the growth of this
/// process's resident memory during it, in KiB.
async fn measure_call(embedding: &dyn Embedding, texts: &[&str]) -> (Duration, u64) {
    let before = resident_kib();
    let done = Arc::new(AtomicBool::new(false));
    let sampler = std::thread::spawn({
        let done = done.clone();
        move || {
            let mut peak = 0;
            while !done.load(Ordering::SeqCst) {
                peak = peak.max(resident_kib());
                std::thread::sleep(Duration::from_millis(5));
            }
            peak
        }
    });
    let started = Instant::now();
    embedding.embed_chunks(texts).await.unwrap();
    let elapsed = started.elapsed();
    done.store(true, Ordering::SeqCst);
    let peak = sampler.join().unwrap();
    (elapsed, peak.saturating_sub(before))
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
