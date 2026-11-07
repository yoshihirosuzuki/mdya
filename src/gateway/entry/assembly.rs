//! Creates the components from the current settings and the operations over
//! them.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{Config, Settings};
use crate::embedding::{Embedding, EmbeddingSettings};
use crate::format::Format;
use crate::path::make_path_absolute;
use crate::types::{BoxError, DirLookup};

use super::super::Factories;
use super::super::operations::{self, ModelCacheDirUndetermined, OperationParts, Operations};
use super::super::write_lock;
use super::DEFAULT_MODEL_CACHE_DIR;

/// What the components are created from.
pub(super) struct Parts<'a> {
    pub(super) factories: &'a Arc<Factories>,
    pub(super) config: Arc<dyn Config>,
    /// The current settings, read from `config`.
    pub(super) settings: &'a Settings,
    /// The one that reading the arguments used, if it did.
    pub(super) format: Arc<dyn Format>,
    /// The settings and index directory, absolute.
    pub(super) data_dir: PathBuf,
    /// `--model-cache-dir`, as given.
    pub(super) model_cache_dir: Option<PathBuf>,
    pub(super) hf_token: Option<String>,
}

/// Creates `embedding`, `index`, `collection`, `query`, and the ingest
/// implementations, and the operations over them and `config`. Creating them
/// cannot fail: each reads or writes only when one of its functions is first
/// called.
pub(super) fn operations(parts: Parts<'_>) -> Arc<dyn Operations> {
    let Parts {
        factories,
        config,
        settings,
        format,
        data_dir,
        model_cache_dir,
        hf_token,
    } = parts;
    let model_cache_dir = model_cache_dir_fn(model_cache_dir, factories.dirs.clone());
    let create_embedding: Arc<dyn Fn(EmbeddingSettings) -> Arc<dyn Embedding> + Send + Sync> = {
        let factories = Arc::clone(factories);
        Arc::new(move |settings| (factories.embedding)(settings))
    };
    let embedding = create_embedding(EmbeddingSettings {
        model: settings.embedding_model.clone(),
        model_cache_dir: boxed_error(Arc::clone(&model_cache_dir)),
        hf_token: hf_token.clone(),
        ollama_endpoint: settings.embedding_ollama_endpoint.clone(),
    });
    let index = (factories.index)(data_dir.clone(), Arc::clone(&embedding));
    let collections = (factories.collections)(data_dir.clone(), Arc::clone(&index));
    let query = (factories.query)(Arc::clone(&embedding), Arc::clone(&index));
    let ingests = (factories.ingests)(format, Arc::clone(&index));
    operations::operations(OperationParts {
        config,
        embedding,
        index,
        collections,
        query,
        ingests,
        create_embedding,
        model_cache_dir,
        hf_token,
        dirs: factories.dirs.clone(),
        parallelism: settings.parallelism(),
        write_lock: write_lock::write_lock(data_dir),
    })
}

/// The embedding model cache directory, made absolute each time it is
/// asked for, so that the MCP server resolves a relative path from the
/// working directory of the moment.
type ModelCacheDir = Arc<dyn Fn() -> Result<PathBuf, ModelCacheDirUndetermined> + Send + Sync>;

/// The function that gives the embedding model cache directory: the given
/// one, or the default, made absolute.
fn model_cache_dir_fn(given: Option<PathBuf>, dirs: DirLookup) -> ModelCacheDir {
    let path = given.unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL_CACHE_DIR));
    Arc::new(move || {
        make_path_absolute(&path, &dirs).map_err(|source| ModelCacheDirUndetermined { source })
    })
}

/// `model_cache_dir` with its error boxed, as `embedding` takes it.
fn boxed_error(
    model_cache_dir: ModelCacheDir,
) -> Arc<dyn Fn() -> Result<PathBuf, BoxError> + Send + Sync> {
    Arc::new(move || model_cache_dir().map_err(|error| Box::new(error) as BoxError))
}
