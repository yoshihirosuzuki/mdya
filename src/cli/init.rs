//! `mdya init` implementation. Idempotent: existing `config.yml` and the
//! `chunks` table are left intact.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancedb::Connection;
use thiserror::Error;
use tracing::info;

use crate::config;
use crate::store::lance_lm::{LanceLmError, ensure_lindera_ipadic_config};
use crate::store::{CHUNKS_TABLE_NAME, SOURCES_TABLE_NAME, chunks_schema, sources_schema};

use super::dim::{DimError, resolve_declared_dim};

#[derive(Debug, Error)]
pub enum InitError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error("create directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("LanceDB path {path} is not valid UTF-8")]
    LancedbPathNotUtf8 { path: PathBuf },

    #[error("LanceDB connect: {0}")]
    LancedbConnect(#[source] lancedb::Error),

    #[error("list LanceDB tables: {0}")]
    LancedbList(#[source] lancedb::Error),

    #[error("create chunks table: {0}")]
    CreateChunksTable(#[source] lancedb::Error),

    #[error("create sources table: {0}")]
    CreateSourcesTable(#[source] lancedb::Error),

    #[error(transparent)]
    LanceLm(#[from] LanceLmError),

    #[error("resolve declared vector dim: {0}")]
    Dim(#[from] DimError),
}

pub async fn run(config_dir_flag: Option<&Path>) -> Result<(), InitError> {
    let base = config::resolve_config_dir(config_dir_flag)?;
    ensure_dirs(&base)?;
    ensure_config_yml(&base)?;
    // Pre-warm the lindera/ipadic config.yml so the first `mdya
    // update-all` does not pay the FTS-tokenizer bootstrap cost. The
    // helper is idempotent and `update-all` calls it again — running it
    // here is symmetric with `ensure_chunks_table` and keeps `mdya init`
    // self-contained.
    ensure_lindera_ipadic_config(&base)?;
    let db = open_index_db(&base).await?;
    ensure_chunks_table(&db, &base).await?;
    ensure_sources_table(&db).await?;
    info!(path = %base.display(), "mdya init complete");
    Ok(())
}

fn ensure_dirs(base: &Path) -> Result<(), InitError> {
    // The embedding-model cache lives under `--model-cache-dir` (default
    // `~/.mdya-models/`), not under `<config_dir>/models/`, so `mdya init`
    // does not materialize `models/`. The cache directory is created by
    // hf-hub on the first download (not by `ModelCache::new`), so
    // pre-warming the resolved path is unnecessary for `init` itself.
    //
    // `lance-models/` sits next to `index/` so the layout under
    // `~/.mdya/` is self-describing. `ensure_lindera_ipadic_config` also
    // `create_dir_all`s its subpaths, but listing the directory
    // explicitly keeps the init contract readable.
    for sub in [
        base.to_path_buf(),
        base.join("index"),
        base.join("lance-models"),
    ] {
        std::fs::create_dir_all(&sub).map_err(|source| InitError::Mkdir {
            path: sub.clone(),
            source,
        })?;
    }
    Ok(())
}

fn ensure_config_yml(base: &Path) -> Result<(), InitError> {
    let path = base.join("config.yml");
    if path.exists() {
        return Ok(());
    }
    config::save(&path, &config::Config::init_template())?;
    Ok(())
}

async fn open_index_db(base: &Path) -> Result<Connection, InitError> {
    let index_dir = base.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| InitError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    lancedb::connect(index_str)
        .execute()
        .await
        .map_err(InitError::LancedbConnect)
}

async fn table_exists(db: &Connection, name: &str) -> Result<bool, InitError> {
    let existing = db
        .table_names()
        .execute()
        .await
        .map_err(InitError::LancedbList)?;
    Ok(existing.iter().any(|n| n == name))
}

async fn ensure_chunks_table(db: &Connection, base: &Path) -> Result<(), InitError> {
    if table_exists(db, CHUNKS_TABLE_NAME).await? {
        return Ok(());
    }
    // Re-read the config to pin the declared embedding model into the
    // table's Arrow `Schema::metadata` at create time. The config has
    // just been ensured by `ensure_config_yml`, so this read is guaranteed
    // to see the final declared value (either pre-existing user edits or
    // the freshly-written init template).
    let cfg = config::load(&base.join("config.yml"))?;
    // The default `cl-nagoya/ruri-v3-30m` resolves to a compile-time dim; an
    // `ollama:<model>` resolves by probing the endpoint (the table does not
    // exist yet here), so `mdya init` with an Ollama model requires the server
    // to be reachable.
    let dim =
        resolve_declared_dim(base, &cfg.embedding.model, &cfg.embedding.ollama.endpoint).await?;
    let schema = chunks_schema(dim, &cfg.embedding.model);
    db.create_empty_table(CHUNKS_TABLE_NAME, Arc::new(schema))
        .execute()
        .await
        .map_err(InitError::CreateChunksTable)?;
    Ok(())
}

/// Create the full-document `sources` table idempotently. Unlike `chunks`
/// it carries no schema-metadata pin (no embedding lives here), so
/// creation needs only the static `sources_schema()`.
async fn ensure_sources_table(db: &Connection) -> Result<(), InitError> {
    if table_exists(db, SOURCES_TABLE_NAME).await? {
        return Ok(());
    }
    db.create_empty_table(SOURCES_TABLE_NAME, Arc::new(sources_schema()))
        .execute()
        .await
        .map_err(InitError::CreateSourcesTable)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn init_materializes_dirs_chunks_table_and_lindera_config() {
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path().to_path_buf();

        run(Some(&base)).await.expect("init");

        assert!(base.join("config.yml").is_file());
        assert!(base.join("index").is_dir());
        // `<config_dir>/models/` is not created by `mdya init`; the
        // embedding-model cache lives under `--model-cache-dir` (default
        // `~/.mdya-models/`) and is materialized lazily by hf-hub on the
        // first model download.
        assert!(
            !base.join("models").exists(),
            "init must not create <config_dir>/models/ anymore",
        );
        assert!(base.join("lance-models").is_dir());
        assert!(
            base.join("lance-models")
                .join("lindera")
                .join("ipadic")
                .join("config.yml")
                .is_file()
        );

        // chunks + sources tables are materialized inside the lance directory.
        let chunks_dir = base.join("index").join("chunks.lance");
        assert!(
            chunks_dir.exists(),
            "chunks table dir not created at {}",
            chunks_dir.display()
        );
        let sources_dir = base.join("index").join("sources.lance");
        assert!(
            sources_dir.exists(),
            "sources table dir not created at {}",
            sources_dir.display()
        );
    }

    #[tokio::test]
    async fn init_is_idempotent() {
        let tmp = TempDir::new().expect("tempdir");
        let base = tmp.path().to_path_buf();

        run(Some(&base)).await.expect("first init");
        let before = std::fs::read(base.join("config.yml")).expect("read");
        // overwrite check: tamper with the file, second init must leave it alone.
        std::fs::write(base.join("config.yml"), b"tampered: true\n").expect("write");
        run(Some(&base)).await.expect("second init");
        let after = std::fs::read(base.join("config.yml")).expect("read");
        assert_eq!(after, b"tampered: true\n");
        let _ = before; // suppress unused warning while documenting intent.
    }
}
