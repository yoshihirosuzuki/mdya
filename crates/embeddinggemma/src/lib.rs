//! On-device `EmbeddingGemma` (`google/embeddinggemma-300m`) sentence
//! embeddings on candle.
//!
//! EmbeddingGemma is a Gemma3-based **bidirectional** sentence encoder: the
//! transformer's last hidden state is mean-pooled, passed through two dense
//! projection layers, and L2-normalized to a 768-dim vector. This crate
//! implements that pipeline on candle with no network or Hugging Face token
//! handling — the caller supplies on-disk file paths (the embedding host owns
//! download, caching, and gated-repo authentication).
//!
//! The Gemma3 encoder and rotary embedding are adapted from Hugging Face's
//! text-embeddings-inference (Apache-2.0); see the crate `NOTICE`.

mod embedder;
mod model;
mod rotary;

pub use embedder::{EmbeddingGemma, Error, ModelFiles};
