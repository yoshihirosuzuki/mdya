//! Assembles the components and starts the command line operation or the MCP
//! server: the body of `run` and `run_with`.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the rest of `gateway` does not use these yet")
)]

use crate::config::ConfigError;
use crate::path::MakePathAbsoluteError;

use super::log_progress::InvalidLogLevel;

/// An error found while assembling the components.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EntryError {
    /// `--log-level` was not valid.
    #[error(transparent)]
    InvalidLogLevel(InvalidLogLevel),
    #[error("the settings cannot be read")]
    Config(#[source] ConfigError),
    /// The settings and index directory (`--data-dir`) could not be made
    /// absolute, before anything else is assembled.
    #[error("the settings and index directory cannot be determined")]
    DataDirUndetermined(#[source] MakePathAbsoluteError),
}
