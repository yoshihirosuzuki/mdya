//! Tracing-subscriber initialization.
//!
//! Resolution rules:
//! - Level: `--log-level` > `RUST_LOG` env > `info`. `RUST_LOG` follows
//!   the standard `tracing-subscriber` convention.
//! - Format: `compact` (default) / `pretty` / `json`.
//! - ANSI colors: `--no-color` flag > `NO_COLOR` env (Unix convention) > TTY
//!   auto-detect on stderr.
//! - All output goes to stderr (stdout is reserved for data / MCP JSON-RPC).
//!
//! `mdya update-all` renders an `indicatif` progress bar on stderr.
//! To prevent `tracing::info!` events from clobbering the bar mid-
//! redraw, the fmt layer writes through `IndicatifLayer`'s stderr-
//! writer wrapper when stderr is a TTY. In non-TTY contexts the layer
//! is skipped entirely: `IndicatifLayer::on_new_span` injects an
//! `IndicatifSpanContext` into every span (not opt-in via
//! `IndicatifSpanExt`), which would hijack every dependency-crate
//! `#[instrument]` span and, with busy lance reads exceeding the
//! default `max_progress_bars=7`, panic via `pb_manager`'s
//! `debug_assert!` on a hidden footer.

use std::io::{self, IsTerminal};

use clap::ValueEnum;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Compact,
    Pretty,
    Json,
}

/// Inputs the binary passes to [`init`]. Constructed from the parsed global
/// flags so this module never touches `clap` directly.
pub struct TracingOptions<'a> {
    pub level_flag: Option<&'a str>,
    pub format: LogFormat,
    pub no_color: bool,
}

/// Install the global `tracing` subscriber. Returns `Err` only if the level
/// directive (`--log-level` value or env value) fails to parse; in that case
/// the binary should surface the error and exit non-zero rather than fall
/// back silently.
pub fn init(opts: TracingOptions<'_>) -> Result<(), tracing_subscriber::filter::ParseError> {
    let filter = resolve_filter(opts.level_flag)?;
    let ansi = resolve_ansi(opts.no_color);
    if io::stderr().is_terminal() {
        init_for_tty(filter, ansi, opts.format);
    } else {
        init_for_non_tty(filter, ansi, opts.format);
    }
    Ok(())
}

/// TTY path: install `IndicatifLayer` and route the fmt writer through
/// `get_stderr_writer()` so `update-all`'s progress bar survives
/// concurrent `info!` emission.
///
/// Note: the `LogFormat` match below intentionally mirrors the one in
/// [`init_for_non_tty`]. Sharing a helper is awkward because the writer
/// types diverge (`IndicatifLayer`'s wrapper vs `fn() -> io::Stderr`);
/// new `LogFormat` variants must be added to both functions.
fn init_for_tty(filter: EnvFilter, ansi: bool, format: LogFormat) {
    let indicatif_layer = IndicatifLayer::new();
    let writer = indicatif_layer.get_stderr_writer();
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(indicatif_layer);
    match format {
        LogFormat::Compact => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).compact())
                .try_init();
        }
        LogFormat::Pretty => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).pretty())
                .try_init();
        }
        LogFormat::Json => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).json())
                .try_init();
        }
    }
}

/// Non-TTY path: skip `IndicatifLayer` entirely. Even with the bar
/// auto-hidden by `indicatif`, the layer still hijacks every
/// dependency-crate `#[instrument]` span (see module-level doc) and
/// trips `tracing-indicatif`'s `debug_assert!` when active spans
/// exceed `max_progress_bars`.
///
/// The `LogFormat` match below mirrors [`init_for_tty`]; keep the
/// two in sync when adding variants.
fn init_for_non_tty(filter: EnvFilter, ansi: bool, format: LogFormat) {
    let registry = tracing_subscriber::registry().with(filter);
    let writer = io::stderr as fn() -> io::Stderr;
    match format {
        LogFormat::Compact => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).compact())
                .try_init();
        }
        LogFormat::Pretty => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).pretty())
                .try_init();
        }
        LogFormat::Json => {
            let _ = registry
                .with(fmt::layer().with_writer(writer).with_ansi(ansi).json())
                .try_init();
        }
    }
}

fn resolve_filter(flag: Option<&str>) -> Result<EnvFilter, tracing_subscriber::filter::ParseError> {
    if let Some(level) = flag {
        return EnvFilter::try_new(level);
    }
    if let Ok(value) = std::env::var("RUST_LOG")
        && !value.is_empty()
    {
        return EnvFilter::try_new(value);
    }
    Ok(EnvFilter::new("info"))
}

fn resolve_ansi(no_color: bool) -> bool {
    if no_color {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_disabled_by_explicit_flag() {
        assert!(!resolve_ansi(true));
    }

    #[test]
    fn filter_with_explicit_flag_returns_that_level() {
        // Avoids touching process-wide env (set_var became `unsafe` in Edition
        // 2024 precisely because it races with other threads in the test
        // harness). Asserting "flag wins over env" by reading env would
        // require serializing tests; instead we verify the flag path on its
        // own — `resolve_filter` returns early on `Some(_)` and never reads
        // env in that branch.
        let filter = resolve_filter(Some("warn")).expect("parse");
        assert!(format!("{filter}").contains("warn"));
    }
}
