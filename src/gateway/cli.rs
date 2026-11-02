//! The command line: reads the arguments, checks what the arguments alone can
//! tell, asks for confirmation, and writes results, messages, and errors.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::collection::CollectionError;
use crate::config::ConfigError;
use crate::format::Format;
use crate::ingest::IngestError;
use crate::query::QueryError;
use crate::types::{CollectionName, CollectionType};

use super::error_report::{ErrorReport, GatewayError};
use super::escape::Escape;
use super::log_progress::Console;
use super::operations::Operations;

/// Reads the command line arguments (the first one is the program name) and
/// checks what they alone can tell, before any setting is read. `format`
/// tells whether a given file type exists; it is passed in, created from
/// `Factories`, so that tests can replace it. Whether a confirmation can be
/// asked is checked separately, by [`check_confirmation`].
pub(crate) fn read_args(args: Vec<OsString>, format: &dyn Format) -> Result<Invocation, CliError> {
    let _ = (args, format);
    todo!()
}

/// What the arguments ask for.
#[derive(Debug)]
pub(crate) struct Invocation {
    pub(crate) options: GlobalOptions,
    pub(crate) action: Action,
}

/// The options that every command takes.
#[derive(Debug)]
pub(crate) struct GlobalOptions {
    /// `--log-level`, not yet checked.
    pub(crate) log_level: Option<String>,
    /// `--log-format`; `Compact` when not given.
    pub(crate) log_format: LogFormat,
    /// Whether `--no-color` was given.
    pub(crate) no_color: bool,
    /// `--data-dir`, before `~` and relative paths are resolved.
    pub(crate) data_dir: Option<PathBuf>,
    /// `--model-cache-dir`, before `~` and relative paths are resolved.
    pub(crate) model_cache_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogFormat {
    Compact,
    Pretty,
    Json,
}

#[derive(Debug)]
pub(crate) enum Action {
    Help(CliHelp),
    Version,
    Mcp(McpTransport),
    Operation(CliOperation),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum McpTransport {
    Stdio,
    /// `host` is an IPv4 address, an IPv6 address without the brackets, or a
    /// host name (for example `127.0.0.1`, `::1`, or `localhost`).
    Http {
        host: String,
        port: u16,
    },
}

/// A command line operation and its arguments. Only this submodule reads it.
#[derive(Debug)]
pub(crate) struct CliOperation(());

/// Which command's help to show. Only this submodule reads it.
#[derive(Debug)]
pub(crate) struct CliHelp(());

/// Whether stdin, stdout, and stderr are terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Terminals {
    pub(crate) stdin: bool,
    pub(crate) stdout: bool,
    pub(crate) stderr: bool,
}

/// Checks, before any setting is read, that the operation can ask for
/// confirmation: it fails when the operation asks, no `--yes` was given, and
/// there is no terminal to ask on.
pub(crate) fn check_confirmation(
    operation: &CliOperation,
    terminals: Terminals,
) -> Result<(), CliError> {
    let _ = (operation, terminals);
    todo!()
}

/// What the command line writes with, shared by everything it writes.
pub(crate) struct CliOutput {
    pub(crate) console: Arc<dyn Console>,
    pub(crate) escape: Arc<dyn Escape>,
    pub(crate) error_report: Arc<dyn ErrorReport>,
    pub(crate) stdout_color: bool,
    pub(crate) stderr_color: bool,
}

/// The streams an operation reads the confirmation from and writes its
/// result to.
pub(crate) struct CliStreams {
    pub(crate) stdin: Box<dyn BufRead + Send>,
    pub(crate) stdout: Box<dyn Write + Send>,
    pub(crate) terminals: Terminals,
}

/// Carries out a command line operation and writes its result, messages,
/// and errors. `size_limit` is the command line's output size limit in bytes
/// (`None` applies none). Returns the exit code.
pub(crate) async fn run_operation(
    operation: CliOperation,
    operations: Arc<dyn Operations>,
    size_limit: Option<u64>,
    streams: CliStreams,
    output: &CliOutput,
) -> u8 {
    let _ = (operation, operations, size_limit, streams, output);
    todo!()
}

/// Writes the help to stdout. Returns the exit code.
pub(crate) fn show_help(help: CliHelp, stdout: Box<dyn Write + Send>, output: &CliOutput) -> u8 {
    let _ = (help, stdout, output);
    todo!()
}

/// Writes mdya's version to stdout. Returns the exit code.
pub(crate) fn show_version(stdout: Box<dyn Write + Send>, output: &CliOutput) -> u8 {
    let _ = (stdout, output);
    todo!()
}

/// Writes the explanation of an error found outside an operation (while
/// assembling the components, or by the MCP server) to stderr. Returns the
/// exit code.
pub(crate) fn report_error(error: &GatewayError, output: &CliOutput) -> u8 {
    let _ = (error, output);
    todo!()
}

/// Writes the explanation of an unexpected failure to stderr. Returns the
/// exit code.
pub(crate) fn report_panic(output: &CliOutput) -> u8 {
    let _ = output;
    todo!()
}

/// Writes the message that tells where the MCP server (HTTP) listens.
pub(crate) fn report_listening(addrs: &[SocketAddr], output: &CliOutput) {
    let _ = (addrs, output);
    todo!()
}

/// An error that only the command line finds.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    /// `detail` says what is wrong with the arguments.
    #[error("the arguments are not valid: {detail}")]
    Usage { detail: String },
    #[error("nothing to change was given")]
    NothingToChange,
    /// `value` is what the user gave.
    #[error("the file type is not supported: {value}")]
    UnsupportedFileType { value: String },
    #[error("the collection name is not valid")]
    Collection {
        name: CollectionName,
        source: CollectionError,
    },
    #[error("the type-specific attributes are not valid")]
    Ingest {
        collection_type: CollectionType,
        source: IngestError,
    },
    #[error("the search request is not valid")]
    Query(#[source] QueryError),
    #[error("the setting is not valid")]
    Config(#[source] ConfigError),
    #[error("a confirmation is needed, but there is no terminal and no --yes")]
    ConfirmationUnavailable,
}
