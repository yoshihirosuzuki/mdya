//! On-device `EmbeddingGemma` (`google/embeddinggemma-300m`) sentence
//! embeddings on candle.
//!
//! EmbeddingGemma is a Gemma3-based **bidirectional** sentence encoder: the
//! transformer's last hidden state is mean-pooled, passed through two dense
//! projection layers, and L2-normalized to a 768-dim vector. This module
//! implements that pipeline on candle with no network or Hugging Face token
//! handling — the caller supplies on-disk file paths (the embedding host owns
//! download, caching, and gated-repo authentication).
//!
//! The Gemma3 encoder and rotary embedding are adapted from Hugging Face's
//! text-embeddings-inference (Apache-2.0); see the root `NOTICE`.

// Inlined from a standalone `embeddinggemma` crate so mdya is self-contained for
// crates.io publishing (a path-only dependency cannot be published). The subtree
// is kept self-contained so it can be extracted back into a crate later: the
// only change required is replacing intra-subtree `super::` paths with `crate::`.
mod embedder;
mod model;
mod rotary;

#[cfg(test)]
mod e2e_real;

pub use embedder::{EmbeddingGemma, ModelFiles};
