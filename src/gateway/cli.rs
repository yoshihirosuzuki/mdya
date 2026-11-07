//! The command line: reads the arguments, checks what the arguments alone can
//! tell, asks for confirmation, and writes results, messages, and errors.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::collection::{CollectionChange, CollectionError};
use crate::config::ConfigError;
use crate::format::Format;
use crate::ingest::IngestError;
use crate::query::{ChunkRangeRequest, QueryError, SearchMode, SearchUnit};
use crate::types::{CollectionName, CollectionType, Key, TypeAttributes};

use super::error_report::{ErrorReport, GatewayError};
use super::formatter::{self, Message};
use super::log_progress::Console;
use super::operations::{Operations, SearchLimit, version};

mod args;
mod run;

/// Reads the command line arguments (the first one is the program name) and
/// checks what they alone can tell, before any setting is read. `format`
/// tells whether a given file type exists; it is passed in, created from
/// `Factories`, so that tests can replace it. Whether a confirmation can be
/// asked is checked separately, by [`check_confirmation`].
pub(crate) fn read_args(args: Vec<OsString>, format: &dyn Format) -> Result<Invocation, CliError> {
    args::read_args(args, format)
}

/// What the arguments ask for.
#[derive(Debug)]
pub(crate) struct Invocation {
    pub(crate) options: GlobalOptions,
    pub(crate) action: Action,
}

/// The options that every command takes, given before the command.
#[derive(Debug, Default)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LogFormat {
    #[default]
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
pub(crate) struct CliOperation(Operation);

/// The help to show, as clap made it. Only this submodule reads it.
#[derive(Debug)]
pub(crate) struct CliHelp(String);

/// A command line operation, with its arguments checked as far as the
/// arguments alone can tell.
#[derive(Debug)]
enum Operation {
    CreateCollection {
        name: CollectionName,
        collection_type: CollectionType,
        description: Option<String>,
        attributes: TypeAttributes,
    },
    DeleteCollection {
        name: CollectionName,
        yes: bool,
        optimize: bool,
    },
    ListCollections {
        output: Output,
    },
    ChangeCollection {
        name: CollectionName,
        change: CollectionChange,
    },
    Update {
        collection: CollectionName,
        overwrite: bool,
        optimize: bool,
    },
    UpdateAll {
        overwrite: bool,
        optimize: bool,
    },
    Put {
        collection: CollectionName,
        key: Key,
        media_type: String,
        content: ContentSource,
        overwrite: bool,
        optimize: bool,
    },
    DeleteDocument {
        collection: CollectionName,
        key: Key,
        yes: bool,
        optimize: bool,
    },
    Search {
        query: String,
        mode: SearchMode,
        collections: Vec<CollectionName>,
        limit: SearchLimit,
        unit: SearchUnit,
        output: Output,
    },
    Get {
        collection: CollectionName,
        key: Key,
        range: ChunkRangeRequest,
        /// `--no-size-limit`.
        size_limit_off: bool,
    },
    Status {
        output: Output,
    },
    ListSettings {
        output: Output,
    },
    SetSetting {
        name: String,
        value: String,
    },
    ResetSetting {
        name: String,
    },
    RebuildEmbeddings {
        force: bool,
        optimize: bool,
    },
    Optimize,
}

/// The output format of an operation that returns a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    Human,
    Json,
}

/// Where the content of a document to put is read from.
#[derive(Debug, PartialEq, Eq)]
enum ContentSource {
    File(PathBuf),
    Stdin,
}

/// Whether stdin, stdout, and stderr are terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Terminals {
    pub(crate) stdin: bool,
    pub(crate) stdout: bool,
    pub(crate) stderr: bool,
}

/// Checks, before any setting is read, that the operation can ask for
/// confirmation: it fails when the operation asks, no `--yes` was given, and
/// there is no terminal to ask on. The answer is read from stdin, so it is
/// stdin that has to be a terminal.
pub(crate) fn check_confirmation(
    operation: &CliOperation,
    terminals: Terminals,
) -> Result<(), CliError> {
    let asks = matches!(
        operation.0,
        Operation::DeleteCollection { yes: false, .. }
            | Operation::DeleteDocument { yes: false, .. }
    );
    if asks && !terminals.stdin {
        return Err(CliError::ConfirmationUnavailable);
    }
    Ok(())
}

/// What the command line writes with, shared by everything it writes. The
/// results on stdout have no colors; `stderr_color` tells whether the
/// explanations of errors on stderr have them.
pub(crate) struct CliOutput {
    pub(crate) console: Arc<dyn Console>,
    pub(crate) error_report: Arc<dyn ErrorReport>,
    pub(crate) stderr_color: bool,
}

/// The exit code of an unexpected failure.
const UNEXPECTED_FAILURE: u8 = 101;

/// The streams an operation reads the confirmation from and writes its
/// result to.
pub(crate) struct CliStreams {
    pub(crate) stdin: Box<dyn BufRead + Send>,
    pub(crate) stdout: Box<dyn Write + Send>,
    pub(crate) terminals: Terminals,
}

/// Carries out a command line operation and writes its result, messages,
/// and errors. `size_limit` is the command line's output size limit in bytes
/// (`None` applies none). Returns the exit code. When what reads stdout has
/// closed it, nothing more is written and the exit code is 0; any other
/// failure to write the result is an error.
pub(crate) async fn run_operation(
    operation: CliOperation,
    operations: Arc<dyn Operations>,
    size_limit: Option<u64>,
    streams: CliStreams,
    output: &CliOutput,
) -> u8 {
    let run = run::Run {
        operations,
        output,
        stdin: streams.stdin,
        stdout: streams.stdout,
        size_limit,
    };
    match run.operation(operation.0).await {
        Ok(code) => code,
        Err(stop) => run::stopped(stop, output),
    }
}

/// Writes the help to stdout. Returns the exit code.
pub(crate) fn show_help(
    help: CliHelp,
    mut stdout: Box<dyn Write + Send>,
    output: &CliOutput,
) -> u8 {
    match run::write_stdout(&mut stdout, &formatter::human_help(&help.0)) {
        Ok(()) => 0,
        Err(stop) => run::stopped(stop, output),
    }
}

/// Writes mdya's version to stdout. Returns the exit code.
pub(crate) fn show_version(mut stdout: Box<dyn Write + Send>, output: &CliOutput) -> u8 {
    match run::write_stdout(&mut stdout, &formatter::human_version(&version())) {
        Ok(()) => 0,
        Err(stop) => run::stopped(stop, output),
    }
}

/// Writes the explanation of an error found outside an operation (while
/// assembling the components, or by the MCP server) to stderr. Returns the
/// exit code.
pub(crate) fn report_error(error: &GatewayError, output: &CliOutput) -> u8 {
    run::report(error, output)
}

/// Writes the explanation of an unexpected failure, which asks for a bug
/// report, to stderr. Returns the exit code, 101.
pub(crate) fn report_panic(output: &CliOutput) -> u8 {
    let explanation = output.error_report.panic_explanation();
    output.console.write_stderr(&format!(
        "{}\n",
        formatter::human_explanation(&explanation, output.stderr_color)
    ));
    UNEXPECTED_FAILURE
}

/// Writes the messages that tell where the MCP server (HTTP) listens, one
/// for each address.
pub(crate) fn report_listening(addrs: &[SocketAddr], output: &CliOutput) {
    for addr in addrs {
        output
            .console
            .write_stderr(&formatter::human_message(&Message::Listening(*addr)));
    }
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
    /// The result of an operation that succeeded could not be written to
    /// stdout, for a reason other than what reads it having closed it.
    #[error("the result could not be written to stdout")]
    StdoutUnwritable { source: std::io::Error },
}
