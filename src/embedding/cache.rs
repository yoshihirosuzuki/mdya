//! Model file cache backed by `hf-hub`.
//!
//! Wraps the upstream `hf_hub::api::tokio::Api` with an mdya-specific cache
//! root resolved from `--model-cache-dir` (default `~/.mdya-models/`).
//! Environment variables that affect the upstream cache resolution
//! (`HF_HOME`, `XDG_CACHE_HOME`, …) are not touched: mdya keeps its on-disk
//! cache independent from the user's other Hugging Face tooling.
//!
//! The API is async because `hf-hub` performs network I/O on a tokio runtime.
//! Callers that own an embedder (sync) lift the async cache call across the
//! boundary inside their constructor; the resulting embedder is then fully
//! sync for the steady-state `embed_queries` / `embed_passages` calls.

use std::path::{Path, PathBuf};

use hf_hub::api::tokio::{Api, ApiBuilder, ApiError};
use hf_hub::{Repo, RepoType};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelCacheError {
    #[error("hf-hub init: {0}")]
    Init(#[from] ApiError),

    #[error("fetch {filename}: {source}")]
    Fetch {
        filename: String,
        #[source]
        source: ApiError,
    },
}

/// An explicit Hugging Face token from the `HF_TOKEN` environment variable, if
/// set and non-empty. Returns `None` otherwise so hf-hub keeps its default
/// token-file discovery (`hf auth login`).
fn explicit_hf_token() -> Option<String> {
    std::env::var("HF_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Single entry point for fetching files from one or more pinned model
/// revisions. Owns the `hf_hub::Api` configured with a custom cache root.
pub struct ModelCache {
    api: Api,
}

impl ModelCache {
    /// Initialize a cache rooted at `cache_dir` (the value resolved from
    /// `--model-cache-dir`, default `~/.mdya-models/`). The directory
    /// is created lazily on the first download by hf-hub; ensuring it exists
    /// ahead of time is the caller's responsibility if they need predictable
    /// behavior on first launch.
    ///
    /// Gated models (e.g. `google/embeddinggemma-300m`) require a Hugging Face
    /// access token. An explicit `HF_TOKEN` environment variable takes
    /// precedence; otherwise hf-hub falls back to the standard token file
    /// written by `hf auth login`. Ungated models (the default `ruri-v3-30m`)
    /// need no token.
    pub fn new(cache_dir: &Path) -> Result<Self, ModelCacheError> {
        let mut builder = ApiBuilder::new().with_cache_dir(cache_dir.to_path_buf());
        if let Some(token) = explicit_hf_token() {
            builder = builder.with_token(Some(token));
        }
        let api = builder.build()?;
        Ok(Self { api })
    }

    /// Download (or hit cache) `filename` from `model_id` at the given
    /// `revision` (commit hash, branch, or tag). Returns the absolute on-disk
    /// path safe to read concurrently.
    pub async fn fetch_model_file(
        &self,
        model_id: &str,
        revision: &str,
        filename: &str,
    ) -> Result<PathBuf, ModelCacheError> {
        let repo = self.api.repo(Repo::with_revision(
            model_id.to_owned(),
            RepoType::Model,
            revision.to_owned(),
        ));
        repo.get(filename)
            .await
            .map_err(|source| ModelCacheError::Fetch {
                filename: filename.to_owned(),
                source,
            })
    }
}
