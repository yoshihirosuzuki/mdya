//! Tracing-subscriber initialization.
//!
//! Resolution rules:
//! - Level: `--log-level` > `RUST_LOG` env > `warn`. `RUST_LOG` follows
//!   the standard `tracing-subscriber` convention. The `warn` default
//!   keeps dependency INFO chatter (e.g. lance dataset-load events that
//!   carry a misleading `status="error"`) off the default console; opt
//!   in with `--log-level info` / `RUST_LOG=info`.
//! - Format: `compact` (default) / `pretty` / `json`.
//! - ANSI colors: `--no-color` flag > `NO_COLOR` env (Unix convention) > TTY
//!   auto-detect on stderr.
//! - All output goes to stderr (stdout is reserved for data / MCP JSON-RPC).
//!
//! `mdya update-all` / `mdya vector use` render an `indicatif` progress
//! bar on stderr. The fmt layer writes through
//! [`log_writer::ProgressAwareStderr`], which suspends that bar around
//! each event so a `tracing` line never lands mid-redraw. The bar is a
//! plain `MultiProgress` owned by the command (see `update_all`); tracing
//! does not render it.

use std::io::{self, IsTerminal};

use clap::ValueEnum;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::log_writer::ProgressAwareStderr;

/// Default level when neither `--log-level` nor `RUST_LOG` is set. `warn`
/// silences mdya's own INFO breadcrumbs and dependency INFO alike; the
/// human-facing success / progress output is emitted outside `tracing`.
const DEFAULT_LEVEL: &str = "warn";

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
    let registry = tracing_subscriber::registry().with(filter);
    let writer = ProgressAwareStderr;
    match opts.format {
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
    Ok(())
}

fn resolve_filter(flag: Option<&str>) -> Result<EnvFilter, tracing_subscriber::filter::ParseError> {
    resolve_filter_from(flag, std::env::var("RUST_LOG").ok())
}

/// Pure core of [`resolve_filter`] with the `RUST_LOG` value injected, so
/// the precedence (`flag` > env > [`DEFAULT_LEVEL`]) is unit-testable
/// without mutating process-wide env (`set_var` is `unsafe` in Edition
/// 2024 because it races other threads in the test harness).
fn resolve_filter_from(
    flag: Option<&str>,
    env_value: Option<String>,
) -> Result<EnvFilter, tracing_subscriber::filter::ParseError> {
    if let Some(level) = flag {
        // clap derive ValueEnum strings are already lowercase; EnvFilter is
        // case-insensitive but we pass them through verbatim.
        return EnvFilter::try_new(level);
    }
    if let Some(value) = env_value.filter(|v| !v.is_empty()) {
        return EnvFilter::try_new(value);
    }
    Ok(EnvFilter::new(DEFAULT_LEVEL))
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
    fn explicit_flag_wins_over_env() {
        let filter = resolve_filter_from(Some("error"), Some("info".to_string())).expect("parse");
        assert!(format!("{filter}").contains("error"));
    }

    #[test]
    fn env_used_when_flag_absent() {
        let filter = resolve_filter_from(None, Some("info".to_string())).expect("parse");
        assert!(format!("{filter}").contains("info"));
    }

    #[test]
    fn default_is_warn_without_flag_or_env() {
        let filter = resolve_filter_from(None, None).expect("parse");
        assert!(format!("{filter}").contains("warn"));
    }

    #[test]
    fn empty_env_falls_back_to_default() {
        let filter = resolve_filter_from(None, Some(String::new())).expect("parse");
        assert!(format!("{filter}").contains("warn"));
    }
}
