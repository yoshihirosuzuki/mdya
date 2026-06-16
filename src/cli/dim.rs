//! Resolve the vector dimension declared by `config.yml`'s `embedding.model`
//! for the schema-metadata pin.
//!
//! The default `cl-nagoya/ruri-v3-30m` has a compile-time dimension; an
//! `ollama:<model>` dimension is model-dependent and learned by probing the
//! endpoint. To keep `mdya search` offline-capable — FTS in particular never
//! needs the server — the probe is avoided whenever the `chunks` table already
//! pins a dim: only the first `mdya init` (table absent) actually probes.

use std::path::Path;

use thiserror::Error;

use crate::embedding::{EmbedError, Embedder, OLLAMA_PREFIX, OllamaEmbedder, on_device_dim};
use crate::store::{self, CHUNKS_TABLE_NAME, metadata_check::METADATA_KEY_VECTOR_DIM};

#[derive(Debug, Error)]
pub enum DimError {
    #[error(transparent)]
    Embed(#[from] EmbedError),

    #[error("read the vector_dim pin from the chunks table: {0}")]
    Lance(#[from] lancedb::Error),

    #[error("connect the index db to read the vector_dim pin: {0}")]
    Store(#[from] anyhow::Error),

    #[error("ollama vector dim {0} does not fit in i32")]
    DimOverflow(usize),
}

/// Resolve the dim to pass to the schema-metadata pin check / table creation.
/// Returns the on-device preset's compile-time dim for an on-device model, the
/// pinned dim already stored in the `chunks` table for an `ollama:` model, or
/// the probed dim when no table exists yet. An unrecognized `embedding.model`
/// is rejected earlier at config load (`validate_embedding_model`), so only
/// validated models reach this resolver.
pub async fn resolve_declared_dim(
    config_dir: &Path,
    model: &str,
    ollama_endpoint: &str,
) -> Result<i32, DimError> {
    if !model.starts_with(OLLAMA_PREFIX) {
        let dim = on_device_dim(model)
            .ok_or_else(|| EmbedError::UnsupportedModel(model.to_string()))?;
        return Ok(i32::try_from(dim).expect("on-device preset dim fits in i32"));
    }
    if let Some(dim) = read_pinned_vector_dim(config_dir).await? {
        return Ok(dim);
    }
    // Table absent (first `mdya init`): probe the live endpoint.
    let embedder = OllamaEmbedder::new(model, ollama_endpoint).await?;
    i32::try_from(embedder.dim()).map_err(|_| DimError::DimOverflow(embedder.dim()))
}

/// Read the `vector_dim` pin from the `chunks` table's schema metadata, or
/// `None` when the table (or the key) does not exist yet.
async fn read_pinned_vector_dim(config_dir: &Path) -> Result<Option<i32>, DimError> {
    let conn = store::connect(config_dir.join("index")).await?;
    let names = conn.table_names().execute().await?;
    if !names.iter().any(|n| n == CHUNKS_TABLE_NAME) {
        return Ok(None);
    }
    let table = conn.open_table(CHUNKS_TABLE_NAME).execute().await?;
    let schema = table.schema().await?;
    Ok(schema
        .metadata()
        .get(METADATA_KEY_VECTOR_DIM)
        .and_then(|v| v.parse::<i32>().ok()))
}
