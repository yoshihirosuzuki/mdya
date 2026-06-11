//! Configuration-layer error type. Crosses the bin/lib boundary: `lib`
//! modules use a `thiserror` enum, the binary lifts it into `anyhow::Error`
//! for chain-format display.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("home directory could not be resolved (no $HOME / %USERPROFILE%)")]
    NoHomeDir,

    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    // NOTE: parse and serialize share the same `serde_saphyr::Error` type
    // but are split into two variants so the error chain can carry the file
    // path on the read side. `#[from]` is attached only to `SerializeYaml`;
    // `ParseYaml` is built via `map_err` so the offending path is preserved.
    #[error("parse YAML {path}: {source}")]
    ParseYaml {
        path: PathBuf,
        #[source]
        source: serde_saphyr::Error,
    },

    #[error("serialize YAML: {0}")]
    SerializeYaml(#[from] serde_saphyr::Error),

    #[error("collection '{name}' already exists in config.yml")]
    CollectionExists { name: String },

    #[error("collection path '{path}' {kind}")]
    InvalidCollectionPath {
        path: PathBuf,
        kind: InvalidPathKind,
    },

    #[error("collection name '{name}' is not valid: {reason}")]
    InvalidCollectionName { name: String, reason: &'static str },
}

#[derive(Debug, Error)]
pub enum InvalidPathKind {
    #[error("does not exist")]
    NotFound,

    #[error("is not a directory")]
    NotDirectory,

    #[error("has no usable basename (use --name to set the collection name explicitly)")]
    NoBasename,
}
