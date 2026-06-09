//! LanceDB storage layer. Houses the connect helper and the shared
//! `chunks` table schema used by `mdya init` (table creation) and the
//! ingest writer (Arrow batch construction).

pub mod lance_lm;
pub mod metadata_check;
mod schema;

pub use schema::{
    CHUNKS_TABLE_NAME, COL_BODY, COL_CHUNK_SEQUENCE, COL_COLLECTION, COL_CONTENT, COL_EMBEDDING,
    COL_PATH, COL_SOURCE_HASH, SOURCES_TABLE_NAME, chunks_schema, sources_schema,
};

use anyhow::{Context, Result};
use lancedb::Connection;
use std::path::Path;

/// Open (or create) a LanceDB database at `path`. The directory is created
/// on demand; `path` may point to a non-existent directory. Non-UTF-8 paths
/// are rejected up front (lancedb's connect API takes &str, so silent loss
/// would otherwise occur on Windows paths with non-ASCII components).
pub async fn connect(path: impl AsRef<Path>) -> Result<Connection> {
    let path = path.as_ref();
    let path_str = path
        .to_str()
        .with_context(|| format!("database path is not valid UTF-8: {path:?}"))?;
    Ok(lancedb::connect(path_str).execute().await?)
}
