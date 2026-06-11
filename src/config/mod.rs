//! Configuration layer for `~/.mdya/config.yml`.
//!
//! Public API:
//! - [`Config`] is the typed root of `config.yml`.
//! - [`load`] / [`save`] are atomic file-level operations.
//! - [`resolve_config_dir`] applies the `--config-dir` precedence chain.
//! - [`resolve_model_cache_dir`] resolves the embedding-model cache directory,
//!   kept separate from the config directory so the two can be moved or
//!   read-only-mounted independently.
//! - [`ConfigError`] is the lib-layer error type; the binary lifts it via
//!   `anyhow::Error` and prints it as a chain.

mod error;
mod paths;
mod schema;
mod store;

pub use error::{ConfigError, InvalidPathKind};
pub use paths::{expand_tilde, resolve_config_dir, resolve_model_cache_dir};
pub use schema::{
    CollectionEntry, Config, DEFAULT_MEMORY_LIMIT_MB, EmbeddingConfig, MAX_EMBED_PARALLELISM,
    RuntimeConfig, validate_collection_name,
};
pub use store::{load, save};
