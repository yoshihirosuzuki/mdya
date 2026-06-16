//! `mdya get <collection> <path> [--chunk <N>] [-f]` dispatcher.
//! Prints either the faithful full document text (no `--chunk`) or one
//! chunk's body (`--chunk N`) to stdout verbatim — no envelope, no
//! trailing newline added — so the output pipes / redirects as the exact
//! stored bytes. A full document over `get.cli_max_bytes` errors out unless
//! `-f` / `--no-size-limit` is given (a redirect / pipe is a legitimate
//! large-output use); chunk reads are never size-checked. Errors (unknown
//! collection, document or chunk not found, too large) propagate to `main`
//! and exit 1, matching the search subcommands.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Result, anyhow};

use crate::config;
use crate::get::{GetError, check_size_limit, configured_size_limit, get_chunk, get_document};

pub async fn run(
    config_dir: Option<&Path>,
    collection: &str,
    path: &str,
    chunk: Option<u32>,
    no_size_limit: bool,
) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let content = match chunk {
        Some(seq) => get_chunk(&cfg_dir, collection, path, seq).await?,
        None => full_document_within_cap(&cfg_dir, collection, path, no_size_limit).await?,
    };
    let mut stdout = io::stdout().lock();
    // `write_all`, not `println!`: emit the content exactly as stored so
    // a document with (or without) a trailing newline round-trips faithfully.
    stdout.write_all(content.as_bytes())?;
    Ok(())
}

/// Fetch the faithful full document and enforce the CLI output cap
/// (`get.cli_max_bytes`, `0` = disabled), unless `no_size_limit` bypasses it.
/// The document is always read in full before the cap is applied: the cap
/// bounds what is *emitted* (an accidental terminal flood), not what the
/// index reads.
/// An over-cap document is rendered as a human `Error:` line carrying the
/// MiB sizes and the override hint — the byte fields come from the
/// channel-neutral [`GetError::DocumentTooLarge`], which the MCP path
/// surfaces without the CLI-only flag suggestion.
async fn full_document_within_cap(
    config_dir: &Path,
    collection: &str,
    path: &str,
    no_size_limit: bool,
) -> Result<String> {
    let content = get_document(config_dir, collection, path).await?;
    let limit = if no_size_limit {
        None
    } else {
        let cfg = config::load(&config_dir.join("config.yml"))?;
        configured_size_limit(cfg.get.cli_max_bytes)
    };
    check_size_limit(&content, limit).map_err(|err| match err {
        GetError::DocumentTooLarge {
            size_bytes,
            limit_bytes,
        } => anyhow!(
            "document too large: {} > {} (use --no-size-limit to override)",
            format_mib(size_bytes),
            format_mib(limit_bytes)
        ),
        other => anyhow::Error::new(other),
    })?;
    Ok(content)
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
