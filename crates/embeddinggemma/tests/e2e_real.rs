//! Real-model end-to-end check (weights required), `#[ignore]`d so the default
//! test run stays offline.
//!
//! Point `EMBEDDINGGEMMA_DIR` at a directory containing `config.json`,
//! `tokenizer.json`, `model.safetensors`, `2_Dense/model.safetensors`, and
//! `3_Dense/model.safetensors`, then run:
//!
//! ```text
//! EMBEDDINGGEMMA_DIR=/path cargo test -p embeddinggemma --test e2e_real -- --ignored
//! ```
//!
//! This is a functional check (output dimension, unit L2 norm, and query↔
//! document retrieval ordering with EmbeddingGemma's task prompts), not a
//! bit-exact comparison against a reference embedding.

use std::path::PathBuf;

use embeddinggemma::{EmbeddingGemma, ModelFiles};

/// Cosine similarity of two L2-normalized vectors (a dot product).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[test]
#[ignore = "requires EmbeddingGemma weights; set EMBEDDINGGEMMA_DIR"]
fn embeds_with_expected_dim_norm_and_retrieval_order() {
    let dir = PathBuf::from(
        std::env::var("EMBEDDINGGEMMA_DIR").expect("set EMBEDDINGGEMMA_DIR to the model directory"),
    );
    let config = dir.join("config.json");
    let tokenizer = dir.join("tokenizer.json");
    let weights = dir.join("model.safetensors");
    let dense2 = dir.join("2_Dense/model.safetensors");
    let dense3 = dir.join("3_Dense/model.safetensors");

    let model = EmbeddingGemma::load(&ModelFiles {
        config: config.as_path(),
        tokenizer: tokenizer.as_path(),
        weights: weights.as_path(),
        dense2: dense2.as_path(),
        dense3: dense3.as_path(),
    })
    .expect("load embeddinggemma");

    // EmbeddingGemma uses distinct query and document task prompts (verified
    // from the model's config_sentence_transformers.json).
    let query = "task: search result | query: what color is the daytime sky?";
    let relevant =
        "title: none | text: The sky looks blue because of Rayleigh scattering of sunlight.";
    let irrelevant = "title: none | text: Preheat the oven to 180C before baking the cookies.";

    let out = model.embed(&[query, relevant, irrelevant]).expect("embed");

    assert_eq!(out.len(), 3);
    for v in &out {
        assert_eq!(v.len(), 768, "output dimension must be 768");
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "vectors must be L2-normalized, got {norm}"
        );
    }

    let sim_relevant = cosine(&out[0], &out[1]);
    let sim_irrelevant = cosine(&out[0], &out[2]);
    assert!(
        sim_relevant > sim_irrelevant,
        "relevant document ({sim_relevant}) should outrank the irrelevant one ({sim_irrelevant})"
    );
}
