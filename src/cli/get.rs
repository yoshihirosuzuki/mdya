//! `mdya get <collection> <path> [--chunk <N>]` dispatcher.
//! Prints either the faithful full document text (no `--chunk`) or one
//! chunk's body (`--chunk N`) to stdout verbatim — no envelope, no
//! trailing newline added — so the output pipes / redirects as the exact
//! stored bytes. Errors (unknown collection, document or chunk not found)
//! propagate to `main` and exit 1, matching the search subcommands.

use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;

use crate::config;
use crate::get::{get_chunk, get_document};

pub async fn run(
    config_dir: Option<&Path>,
    collection: &str,
    path: &str,
    chunk: Option<u32>,
) -> Result<()> {
    let cfg_dir = config::resolve_config_dir(config_dir)?;
    let content = match chunk {
        Some(seq) => get_chunk(&cfg_dir, collection, path, seq).await?,
        None => get_document(&cfg_dir, collection, path).await?,
    };
    let mut stdout = io::stdout().lock();
    // `write_all`, not `println!`: emit the content exactly as stored so
    // a document with (or without) a trailing newline round-trips faithfully.
    stdout.write_all(content.as_bytes())?;
    Ok(())
}
