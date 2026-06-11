//! `mdya vector use <model>` — destructive embedding-model switch.
//!
//! Switching the embedding model invalidates every stored vector: a new
//! model may emit a different `vector_dim`, and even at the same dim the
//! vectors are model-specific. The command therefore drops the `chunks`
//! table, recreates it with the new model/dim pinned into
//! `Schema::metadata`, and re-embeds every collection from
//! disk via the existing `update_all_collections` path — one uniform
//! code path regardless of whether the dim changed. The `sources` table
//! (full document text) is model-independent and left untouched.
//!
//! Because the operation is destructive it requires confirmation: an
//! interactive `[y/N]` prompt on a TTY, skippable with `--yes`. On a
//! non-interactive stdin without `--yes` it refuses (exit 1) rather than
//! silently destroying the index.

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

use crate::config;
use crate::embedding::{EmbedError, Embedder, ModelCache, ModelCacheError, build_embedder};
use crate::ingest::{IngestError, IngestProgress, UpdateSummary, update_all_collections};
use crate::store::{self, CHUNKS_TABLE_NAME, chunks_schema};

use super::update_all::{IndicatifProgress, expand_collection_paths, resolve_embed_parallelism};

#[derive(Debug, Error)]
pub enum VectorUseError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),

    #[error(transparent)]
    ModelCache(#[from] ModelCacheError),

    #[error(transparent)]
    Embedding(#[from] EmbedError),

    #[error(transparent)]
    Ingest(#[from] IngestError),

    #[error("connect the index db: {0}")]
    Store(#[source] anyhow::Error),

    #[error("recreate the chunks table: {0}")]
    Lance(#[source] lancedb::Error),

    #[error("new model vector dim {0} does not fit in i32")]
    DimOverflow(usize),

    /// `--yes` was not given and stdin is not a TTY, so the destructive
    /// switch cannot be confirmed. Refuse rather than proceed.
    #[error("refusing to run a destructive model switch without --yes on a non-interactive stdin")]
    NonInteractive,

    #[error("read confirmation from stdin: {0}")]
    Stdin(#[source] std::io::Error),

    /// At least one file failed to re-embed. The switch (config + table
    /// recreate) already happened; `mdya update-all` resumes the rest.
    /// Mirrors `UpdateAllError::FilesFailed` so `main` propagates exit 1.
    #[error("{0} file(s) failed during re-embed — check warnings above for details")]
    FilesFailed(u64),
}

/// `mdya vector use <model>` entry point. Called from `cli::Cli::run`.
///
/// Order is chosen so a typo or unreachable backend never reaches the
/// destructive step: the no-op short-circuit and `build_embedder`
/// validation both run before any prompt or write. For an
/// `ollama:<model>` target `build_embedder` probes the live endpoint, so
/// the new `vector_dim` is known before the table is touched.
pub(crate) async fn run(
    config_dir_flag: Option<&Path>,
    model_cache_dir_flag: Option<&Path>,
    model: &str,
    assume_yes: bool,
) -> Result<(), VectorUseError> {
    let base = config::resolve_config_dir(config_dir_flag)?;
    let cfg = config::load(&base.join("config.yml"))?;
    if cfg.embedding.model == model {
        eprintln!("Already using '{model}'; nothing to do.");
        return Ok(());
    }
    let cache = ModelCache::new(&config::resolve_model_cache_dir(model_cache_dir_flag)?)?;
    let embedder = build_embedder(model, &cache).await?;
    if !user_confirms(
        assume_yes,
        &cfg.embedding.model,
        model,
        embedder.dim(),
        cfg.collections.len(),
    )? {
        eprintln!("Aborted, no changes made.");
        return Ok(());
    }
    // Read the parallelism for the progress UI before `cfg` moves into
    // `switch_model` (which owns the rewrite). stdout carries the
    // machine-facing summary, mirroring `mdya update-all`; the no-op /
    // abort notices above go to stderr.
    let parallelism = resolve_embed_parallelism(&cfg.runtime);
    let progress: Arc<dyn IngestProgress> =
        Arc::new(IndicatifProgress::with_parallelism(parallelism));
    let summary = switch_model(&base, cfg, embedder, progress, parallelism).await?;
    println!("{}", format_switch_summary(model, &summary));
    if summary.failed > 0 {
        return Err(VectorUseError::FilesFailed(summary.failed));
    }
    Ok(())
}

/// Rewrite the declared model in `cfg`, recreate the `chunks` table with
/// the new metadata pin, and re-embed every collection from disk.
///
/// Takes `cfg` by value (already loaded by `run`) so the whole switch
/// works off a single config snapshot — no second `config::load` that
/// could diverge from the one used for the confirmation prompt.
///
/// Reuses `update_all_collections` for the re-embed: the freshly
/// recreated `chunks` table is empty, so every file is treated as new
/// (`Action::New`) and re-embedded with the supplied `embedder`. The
/// `runtime.embed_parallelism` bound and the memory guard apply exactly
/// as they do for `mdya update-all`.
///
/// Caller contract: `embedder.model_id()` must differ from
/// `cfg.embedding.model` — `run` owns the no-op short-circuit and the
/// destructive confirmation. `pub` so integration tests can inject a
/// stand-in `Embedder` (same seam as `update_all_collections`).
pub async fn switch_model(
    base: &Path,
    mut cfg: config::Config,
    embedder: Arc<dyn Embedder>,
    progress: Arc<dyn IngestProgress>,
    parallelism: usize,
) -> Result<UpdateSummary, VectorUseError> {
    cfg.embedding.model = embedder.model_id().to_string();
    config::save(&base.join("config.yml"), &cfg)?;
    // `chunks_schema` takes the dim as i32; `embedder.dim()` is usize, so
    // guard the (practically impossible) overflow rather than wrap silently.
    let new_dim =
        i32::try_from(embedder.dim()).map_err(|_| VectorUseError::DimOverflow(embedder.dim()))?;
    recreate_chunks_table(base, new_dim, embedder.model_id()).await?;
    let collections = expand_collection_paths(&cfg);
    let summary = update_all_collections(&collections, base, embedder, progress, parallelism)
        .await
        .map_err(VectorUseError::Ingest)?;
    Ok(summary)
}

/// Drop the `chunks` table (if present) and recreate it empty with the
/// new model/dim pinned into `Schema::metadata`. The existence guard is
/// required because LanceDB's file backend returns `TableNotFound` when
/// dropping a non-existent table.
///
/// `model` flows only into the Arrow `Schema::metadata` HashMap (via
/// `chunks_schema`), never into a SQL predicate, so it needs no
/// `sql_escape`.
async fn recreate_chunks_table(
    base: &Path,
    vector_dim: i32,
    model: &str,
) -> Result<(), VectorUseError> {
    let db = store::connect(base.join("index"))
        .await
        .map_err(VectorUseError::Store)?;
    let names = db
        .table_names()
        .execute()
        .await
        .map_err(VectorUseError::Lance)?;
    if names.iter().any(|n| n == CHUNKS_TABLE_NAME) {
        db.drop_table(CHUNKS_TABLE_NAME, &[])
            .await
            .map_err(VectorUseError::Lance)?;
    }
    db.create_empty_table(
        CHUNKS_TABLE_NAME,
        Arc::new(chunks_schema(vector_dim, model)),
    )
    .execute()
    .await
    .map_err(VectorUseError::Lance)?;
    Ok(())
}

/// Resolve whether the destructive switch may proceed: `--yes` bypasses
/// the prompt; otherwise a TTY gets an interactive `[y/N]` and a
/// non-interactive stdin is refused (never destroy data unprompted).
fn user_confirms(
    assume_yes: bool,
    current_model: &str,
    new_model: &str,
    new_dim: usize,
    n_collections: usize,
) -> Result<bool, VectorUseError> {
    if assume_yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        return Err(VectorUseError::NonInteractive);
    }
    prompt_and_read(current_model, new_model, new_dim, n_collections)
}

fn prompt_and_read(
    current_model: &str,
    new_model: &str,
    new_dim: usize,
    n_collections: usize,
) -> Result<bool, VectorUseError> {
    eprintln!("This will switch the embedding model:");
    eprintln!("  {current_model} -> {new_model} (dim {new_dim})");
    eprintln!(
        "The chunks index will be DROPPED and {n_collections} collection(s) re-embedded from scratch."
    );
    eprint!("Proceed? [y/N]: ");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(VectorUseError::Stdin)?;
    Ok(parse_confirmation(&line))
}

/// Accept only an explicit `y` / `yes` (case-insensitive). Everything
/// else — including the empty default — declines, matching the `[y/N]`
/// prompt's capitalised `N`.
fn parse_confirmation(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn format_switch_summary(model: &str, s: &UpdateSummary) -> String {
    // After a recreate every walked file is re-embedded as new, so
    // `total` is the document count. `removed` counts orphaned `sources`
    // rows whose file disappeared since the last ingest — reported for
    // parity with `mdya update-all` so that cleanup is not silent.
    let total = s.new + s.updated + s.skipped + s.failed;
    format!(
        "Switched embedding model to '{model}'. Re-embedded {total} document(s) (removed: {}, failed: {}).",
        s.removed, s.failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_confirmation_accepts_y_and_yes_case_insensitively() {
        for ok in ["y", "Y", "yes", "YES", "Yes", " y \n", "yes\n"] {
            assert!(parse_confirmation(ok), "{ok:?} should confirm");
        }
    }

    #[test]
    fn parse_confirmation_rejects_empty_and_anything_else() {
        for no in ["", "\n", "n", "N", "no", "nope", "ye", "yess", "1", "ok"] {
            assert!(!parse_confirmation(no), "{no:?} should decline");
        }
    }

    #[test]
    fn format_switch_summary_reports_total_removed_and_failed() {
        let s = UpdateSummary {
            new: 7,
            updated: 0,
            skipped: 0,
            removed: 2,
            failed: 1,
        };
        let out = format_switch_summary("ollama:nomic-embed-text", &s);
        // total = new + updated + skipped + failed = 8; removed is reported
        // separately (it does not count toward the document total).
        assert_eq!(
            out,
            "Switched embedding model to 'ollama:nomic-embed-text'. Re-embedded 8 document(s) (removed: 2, failed: 1)."
        );
    }
}
