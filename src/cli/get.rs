//! `mdya get <collection> <path> [--chunk <N> [--chunk-end <M>]] [-f]`
//! dispatcher. Prints either the faithful full document text (no `--chunk`)
//! or the original text of chunks N through M (`--chunk N`, optionally
//! `--chunk-end M`) to stdout verbatim — no envelope, no trailing newline
//! added — so the output pipes / redirects as the exact stored bytes. Output
//! over `get.cli_max_bytes` errors out unless `-f` / `--no-size-limit` is
//! given (a redirect / pipe is a legitimate large-output use). Errors
//! (unknown collection, document or chunk not found, invalid chunk range,
//! outdated index, too large) propagate to `main` and exit 1, matching the
//! search subcommands.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Result, anyhow};

use crate::config;
use crate::get::{
    ChunkSpan, GetError, check_size_limit, configured_size_limit, get_chunks, get_document,
};

pub async fn run(
    config_dir: Option<&Path>,
    collection: &str,
    path: &str,
    chunk: Option<u32>,
    chunk_end: Option<u32>,
    no_size_limit: bool,
) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let span = ChunkSpan::from_request(chunk, chunk_end).map_err(cli_range_error)?;
    let (content, label) = match span {
        Some(span) => (
            get_chunks(&cfg_dir, collection, path, span).await?,
            if span.first() == span.last() {
                "chunk"
            } else {
                "chunk range"
            },
        ),
        None => (get_document(&cfg_dir, collection, path).await?, "document"),
    };
    enforce_cli_cap(&cfg_dir, &content, label, no_size_limit)?;
    let mut stdout = io::stdout().lock();
    // `write_all`, not `println!`: emit the content exactly as stored so
    // a document with (or without) a trailing newline round-trips faithfully.
    stdout.write_all(content.as_bytes())?;
    Ok(())
}

/// Enforce the CLI output cap (`get.cli_max_bytes`, `0` = disabled) on what
/// is about to be printed, unless `no_size_limit` bypasses it. The content is
/// always read in full before the cap is applied: the cap bounds what is
/// *emitted* (an accidental terminal flood), not what the index reads.
/// Over-cap output is rendered as a human `Error:` line naming what was
/// read (`label`), the MiB sizes, and the override hint — the byte fields
/// come from the channel-neutral [`GetError::ContentTooLarge`], which the MCP
/// path surfaces without the CLI-only flag suggestion.
fn enforce_cli_cap(
    config_dir: &Path,
    content: &str,
    label: &str,
    no_size_limit: bool,
) -> Result<()> {
    let limit = if no_size_limit {
        None
    } else {
        let cfg = config::load(&config_dir.join("config.yml"))?;
        configured_size_limit(cfg.get.cli_max_bytes)
    };
    check_size_limit(content, limit).map_err(|err| match err {
        GetError::ContentTooLarge {
            size_bytes,
            limit_bytes,
        } => anyhow!(
            "{label} too large: {} > {} (use --no-size-limit to override)",
            format_mib(size_bytes),
            format_mib(limit_bytes)
        ),
        other => anyhow::Error::new(other),
    })
}

/// Re-render an invalid chunk range in the CLI's flag names. The library
/// message names the MCP parameters (`chunk` / `chunk_end`), which a CLI
/// user never typed.
fn cli_range_error(err: GetError) -> anyhow::Error {
    match err {
        GetError::ChunkEndBeforeChunk { chunk, chunk_end } => {
            anyhow!("--chunk-end ({chunk_end}) must be >= --chunk ({chunk})")
        }
        other => anyhow::Error::new(other),
    }
}

/// Render a byte count as MiB with one decimal for the too-large message.
/// Caps cluster around the 1 MiB default, so MiB is the natural unit for a
/// human; machine consumers read exact bytes from the MCP `details` instead.
fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mib_renders_one_decimal() {
        assert_eq!(format_mib(1024 * 1024), "1.0 MiB");
        // 2.5 MiB — the example from the user-facing error message.
        assert_eq!(format_mib(2_621_440), "2.5 MiB");
    }
}
