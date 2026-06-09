//! `mdya search` CLI dispatcher for FTS, Vector, and Hybrid modes.
//!
//! The schema-metadata mismatch warning is emitted by
//! `SearchEngine::open` via `tracing::warn!`, which lands on the same
//! `stderr` the global tracing subscriber writes to. The check site is
//! shared across all three modes (`fts` / `vector` / `hybrid`); the
//! `HashMap::get(&key)` check costs nothing measurable, so every
//! `SearchEngine::open` runs it unconditionally.

use std::io;
use std::path::Path;

use anyhow::Result;

use crate::config;
use crate::embedding::{ModelCache, build_embedder};
use crate::search::{
    SearchEngine, SearchLevel, SearchRequest, SearchResponse, print_human, print_json, print_md,
    print_xml,
};

use super::dim::resolve_declared_dim;
use super::{OutputFormat, SearchArgs, SearchCommand, color_enabled};

/// Move `SearchArgs` into the engine-facing `SearchRequest` and pull
/// out the chosen output format. `--chunks` is the CLI ergonomic
/// boolean; here it becomes the internal [`SearchLevel`] enum so the
/// engine and the renderers see the same type the MCP layer does. The
/// 3 mode dispatchers below share this rather than open-coding the
/// field copy (the third one would have been the Rule-of-Three smell).
fn args_into_request(args: SearchArgs) -> (SearchRequest, OutputFormat) {
    let SearchArgs {
        query,
        collections,
        limit,
        chunks,
        format,
    } = args;
    let level = if chunks {
        SearchLevel::Chunk
    } else {
        SearchLevel::Doc
    };
    (
        SearchRequest {
            query,
            collections,
            limit,
            level,
        },
        format,
    )
}

pub async fn run(
    config_dir: Option<&Path>,
    model_cache_dir: Option<&Path>,
    cmd: SearchCommand,
    no_color_flag: bool,
) -> Result<()> {
    match cmd {
        // FTS does not load an embedder, so the model-cache directory is
        // irrelevant on this arm.
        SearchCommand::Fts(args) => run_fts(config_dir, args, no_color_flag).await,
        SearchCommand::Vector(args) => {
            run_vector(config_dir, model_cache_dir, args, no_color_flag).await
        }
        SearchCommand::Hybrid(args) => {
            run_hybrid(config_dir, model_cache_dir, args, no_color_flag).await
        }
    }
}

/// Dispatch one rendered response to stdout. Shared by all three modes so
/// the `OutputFormat` arms live in a single place instead of being copied
/// into `run_fts` / `run_vector` / `run_hybrid`. `use_color` is consumed
/// only by `print_human`; the machine formats ignore it.
fn render<W: io::Write>(
    writer: &mut W,
    format: OutputFormat,
    resp: &SearchResponse,
    use_color: bool,
) -> io::Result<()> {
    match format {
        OutputFormat::Human => print_human(writer, resp, use_color),
        OutputFormat::Json => print_json(writer, resp),
        OutputFormat::Md => print_md(writer, resp),
        OutputFormat::Xml => print_xml(writer, resp),
    }
}

async fn run_fts(config_dir: Option<&Path>, args: SearchArgs, no_color_flag: bool) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let cfg = config::load(&cfg_dir.join("config.yml"))?;
    // FTS uses no vectors, so this resolves the pin dim without loading an
    // embedder — for `ollama:` models it reads the table's stored dim rather
    // than probing the server, keeping FTS offline.
    let dim = resolve_declared_dim(&cfg_dir, &cfg.embedding.model).await?;
    let engine = SearchEngine::open(&cfg_dir, &cfg.embedding.model, dim).await?;
    let (req, format) = args_into_request(args);
    // FTS does not load an embedder, so `validate_request` is folded
    // into `SearchEngine::fts` itself — there is no expensive setup to
    // short-circuit on a typo, unlike `run_vector` / `run_hybrid`.
    let resp = engine.fts(&req).await?;
    let mut stdout = io::stdout().lock();
    render(&mut stdout, format, &resp, color_enabled(no_color_flag))?;
    Ok(())
}

async fn run_vector(
    config_dir: Option<&Path>,
    model_cache_dir: Option<&Path>,
    args: SearchArgs,
    no_color_flag: bool,
) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let cfg = config::load(&cfg_dir.join("config.yml"))?;
    let dim = resolve_declared_dim(&cfg_dir, &cfg.embedding.model).await?;
    let engine = SearchEngine::open(&cfg_dir, &cfg.embedding.model, dim).await?;
    let (req, format) = args_into_request(args);
    // Reject bad requests (empty query / limit 0 / unknown collection)
    // before loading the embedder so an obvious typo does not trigger
    // the ~140 MB `cl-nagoya/ruri-v3-30m` download (or an Ollama round-trip).
    engine.validate_request(&req)?;
    let cache = ModelCache::new(&config::resolve_model_cache_dir(model_cache_dir)?)?;
    let embedder = build_embedder(&cfg.embedding.model, &cache).await?;
    let resp = engine.vector(&req, embedder.as_ref()).await?;
    let mut stdout = io::stdout().lock();
    render(&mut stdout, format, &resp, color_enabled(no_color_flag))?;
    Ok(())
}

async fn run_hybrid(
    config_dir: Option<&Path>,
    model_cache_dir: Option<&Path>,
    args: SearchArgs,
    no_color_flag: bool,
) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let cfg = config::load(&cfg_dir.join("config.yml"))?;
    let dim = resolve_declared_dim(&cfg_dir, &cfg.embedding.model).await?;
    let engine = SearchEngine::open(&cfg_dir, &cfg.embedding.model, dim).await?;
    let (req, format) = args_into_request(args);
    // Reject bad requests (empty query / limit 0 / unknown collection)
    // before loading the embedder so an obvious typo does not trigger
    // the ~140 MB `cl-nagoya/ruri-v3-30m` download (or an Ollama round-trip).
    engine.validate_request(&req)?;
    let cache = ModelCache::new(&config::resolve_model_cache_dir(model_cache_dir)?)?;
    let embedder = build_embedder(&cfg.embedding.model, &cache).await?;
    let resp = engine.hybrid(&req, embedder.as_ref()).await?;
    let mut stdout = io::stdout().lock();
    render(&mut stdout, format, &resp, color_enabled(no_color_flag))?;
    Ok(())
}
