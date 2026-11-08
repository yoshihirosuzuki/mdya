//! Assembles the components and starts the command line operation or the MCP
//! server: the body of `run` and `run_with`.
//!
//! `run_with` reads the arguments, checks what they alone can tell, decides
//! the current values of the settings that the arguments and the environment
//! variables give, assembles the components, and has `cli` carry out the
//! operation or `mcp` serve. An unexpected failure that reaches it, from
//! reading the arguments to the end of the operation or of the MCP server,
//! is caught here: it writes what it was doing and the explanation that asks
//! for a bug report, and returns 101.

use std::ffi::{OsStr, OsString};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::config::{ConfigError, Settings};
use crate::format::{Extracted, FileType, Format, FormatError};
use crate::path::{MakePathAbsoluteError, make_path_absolute};
use crate::watchdog::WatchdogSettings;

use super::cli::{
    self, Action, CliOperation, CliOutput, CliStreams, Invocation, McpTransport, Terminals,
};
use super::error_report::{self, GatewayError};
use super::formatter::{self, FailurePlace};
use super::log_progress::{self, InvalidLogLevel, MeasureWidth, SharedStderr};
use super::mcp::{self, McpServerParts};
use super::{Factories, Io, LogSettings};

mod assembly;
mod mcp_stdio;
mod process;
mod stdout_pipe;
#[cfg(test)]
mod tests;

pub(super) use process::run;

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
    /// The runtime for the asynchronous work could not be created, or a
    /// thread that the MCP server (stdio) reads or writes on could not be
    /// started.
    #[error("mdya cannot get the system resources it needs to run")]
    ResourcesUnavailable(#[source] std::io::Error),
}

/// The settings and index directory when `--data-dir` is not given.
const DEFAULT_DATA_DIR: &str = "~/.mdya";

/// The embedding model cache directory when `--model-cache-dir` is not given.
const DEFAULT_MODEL_CACHE_DIR: &str = "~/.mdya-models";

/// The exit code when the memory watchdog stops mdya.
const MEMORY_LIMIT_EXIT_CODE: i32 = 137;

/// The body of [`super::run_with`].
pub(super) fn run_with(
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    io: Io,
    factories: Factories,
) -> ExitCode {
    let Io {
        stdin,
        stdout,
        stderr,
        stdin_is_terminal,
        stdout_is_terminal,
        stderr_is_terminal,
    } = io;
    let env = Environment(env);
    let terminals = Terminals {
        stdin: stdin_is_terminal,
        stdout: stdout_is_terminal,
        stderr: stderr_is_terminal,
    };
    let stderr = log_progress::shared_stderr(stderr, progress_terminal(&env, terminals.stderr));
    let output = CliOutput {
        console: Arc::clone(&stderr) as _,
        error_report: error_report::error_report(),
        stderr_color: stderr_color(&args, &env, terminals.stderr),
    };
    let place = Mutex::new(FailurePlace::ReadingArguments);
    let started = std::panic::catch_unwind(AssertUnwindSafe(|| {
        start(Start {
            args,
            env: &env,
            streams: CliStreams {
                stdin,
                stdout,
                terminals,
            },
            factories,
            stderr: &stderr,
            output: &output,
            place: &place,
        })
    }));
    let code = started.unwrap_or_else(|_| {
        let place = *place.lock().unwrap_or_else(PoisonError::into_inner);
        output
            .console
            .write_stderr(&formatter::human_failure_place(place));
        cli::report_panic(&output)
    });
    ExitCode::from(code)
}

/// The environment variables that `run_with` received.
struct Environment(Vec<(OsString, OsString)>);

impl Environment {
    /// The value of the variable `name`. Windows does not tell the case of
    /// the names apart.
    fn get(&self, name: &str) -> Option<&OsStr> {
        self.0
            .iter()
            .rev()
            .find(|(key, _)| {
                if cfg!(windows) {
                    key.eq_ignore_ascii_case(name)
                } else {
                    key == name
                }
            })
            .map(|(_, value)| value.as_os_str())
    }
}

/// Whether to color what is written to stderr: only on a terminal, and
/// neither with `--no-color` nor with `NO_COLOR` set. `--no-color` is looked
/// for in the arguments themselves, so that an error in reading them is not
/// colored either.
fn stderr_color(args: &[OsString], env: &Environment, terminal: bool) -> bool {
    let no_color = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--no-color");
    terminal && !no_color && env.get("NO_COLOR").is_none()
}

/// How to measure the terminal that stderr is, where the progress can be
/// drawn again in place: stderr is a terminal, `TERM` is not `dumb`, and,
/// outside Windows, `TERM` is set. `None` elsewhere.
fn progress_terminal(env: &Environment, stderr_terminal: bool) -> Option<MeasureWidth> {
    let term = env.get("TERM");
    let drawable =
        stderr_terminal && term != Some(OsStr::new("dumb")) && (cfg!(windows) || term.is_some());
    drawable.then(|| Arc::new(log_progress::stderr_width) as MeasureWidth)
}

/// What [`start`] works with.
struct Start<'a> {
    args: Vec<OsString>,
    env: &'a Environment,
    streams: CliStreams,
    factories: Factories,
    stderr: &'a Arc<SharedStderr>,
    output: &'a CliOutput,
    /// What it is doing, for an unexpected failure.
    place: &'a Mutex<FailurePlace<'static>>,
}

/// What the arguments ask for, once they ask for the components.
enum Task {
    Cli(CliOperation),
    Mcp(McpTransport),
}

impl Task {
    /// The commands that name it.
    fn command(&self) -> &'static str {
        match self {
            Task::Cli(operation) => operation.command(),
            Task::Mcp(_) => "mdya mcp",
        }
    }
}

/// Reads the arguments and checks what they alone can tell, then sets the
/// process's environment variables and the log receiver, and starts the
/// runtime that assembles the components and runs the task. Returns the
/// exit code.
fn start(start: Start<'_>) -> u8 {
    let Start {
        args,
        env,
        streams,
        factories,
        stderr,
        output,
        place,
    } = start;
    let factories = Arc::new(factories);
    let format = Arc::new(LazyFormat {
        factories: Arc::clone(&factories),
        made: OnceLock::new(),
    });
    let Invocation { options, action } = match cli::read_args(args, format.as_ref()) {
        Ok(invocation) => invocation,
        Err(error) => return report(GatewayError::Cli(error), output),
    };
    let task = match action {
        Action::Help(help) => return cli::show_help(help, streams.stdout, output),
        Action::Version => return cli::show_version(streams.stdout, output),
        Action::Operation(operation) => {
            if let Err(error) = cli::check_confirmation(&operation, streams.terminals) {
                return report(GatewayError::Cli(error), output);
            }
            Task::Cli(operation)
        }
        Action::Mcp(transport) => Task::Mcp(transport),
    };
    set(place, FailurePlace::Preparing(task.command()));
    let (filter, ignored) =
        match log_progress::log_filter(options.log_level.as_deref(), env.get("RUST_LOG")) {
            Ok(filter) => filter,
            Err(error) => {
                return report(
                    GatewayError::Entry(EntryError::InvalidLogLevel(error)),
                    output,
                );
            }
        };
    let given_data_dir = options.data_dir.as_deref();
    let data_dir = match make_path_absolute(
        given_data_dir.unwrap_or(Path::new(DEFAULT_DATA_DIR)),
        &factories.dirs,
    ) {
        Ok(data_dir) => data_dir,
        Err(error) => {
            return report(
                GatewayError::Entry(EntryError::DataDirUndetermined(error)),
                output,
            );
        }
    };
    // Before any other thread starts: the runtime and the log receiver come
    // after this.
    (factories.set_process_environment)(crate::index::process_environment(&data_dir));
    (factories.set_log_receiver)(LogSettings {
        filter,
        format: options.log_format,
        color: output.stderr_color,
        stderr: Arc::clone(stderr),
    });
    for ignored in ignored {
        tracing::warn!(
            directive = %ignored.directive,
            detail = %ignored.detail,
            "a directive in RUST_LOG is not valid, so it was ignored"
        );
    }
    // An error here is a resource that the system did not give, such as the
    // driver that waits for events. A worker thread that cannot be started
    // is not an error but a panic in tokio, which is caught as an unexpected
    // failure, as any other panic here is.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return report(
                GatewayError::Entry(EntryError::ResourcesUnavailable(error)),
                output,
            );
        }
    };
    let code = runtime.block_on(assemble_and_run(Assemble {
        task,
        streams,
        factories,
        format,
        data_dir,
        model_cache_dir: options.model_cache_dir,
        hf_token: hf_token(env),
        output,
        place,
    }));
    // Whatever the components left running is not waited for: the
    // operation, or the MCP server, has ended.
    runtime.shutdown_background();
    code
}

/// What [`assemble_and_run`] works with.
struct Assemble<'a> {
    task: Task,
    streams: CliStreams,
    factories: Arc<Factories>,
    /// Created already if reading the arguments used it.
    format: Arc<LazyFormat>,
    data_dir: PathBuf,
    /// `--model-cache-dir`, as given.
    model_cache_dir: Option<PathBuf>,
    hf_token: Option<String>,
    output: &'a CliOutput,
    place: &'a Mutex<FailurePlace<'static>>,
}

/// Reads the settings, starts the memory watchdog, assembles the other
/// components, and runs the task. Returns the exit code.
async fn assemble_and_run(assemble: Assemble<'_>) -> u8 {
    let Assemble {
        task,
        streams,
        factories,
        format,
        data_dir,
        model_cache_dir,
        hf_token,
        output,
        place,
    } = assemble;
    let config = (factories.config)(data_dir.clone());
    let settings = match config.current().await {
        Ok(settings) => settings,
        Err(error) => return report(GatewayError::Entry(EntryError::Config(error)), output),
    };
    // Kept until the task ends: the watchdog runs while it is kept.
    let _watchdog = (factories.start_watchdog)(watchdog_settings(&settings, output));
    let operations = assembly::operations(assembly::Parts {
        factories: &factories,
        config,
        settings: &settings,
        // Created now if reading the arguments did not, so that the
        // components get it, and a panic in creating it, once.
        format: Arc::clone(format.made()),
        data_dir,
        model_cache_dir,
        hf_token,
    });
    set(place, FailurePlace::Running(task.command()));
    match task {
        Task::Cli(operation) => {
            let size_limit = size_limit(settings.get_cli_limit_bytes);
            cli::run_operation(operation, operations, size_limit, streams, output).await
        }
        Task::Mcp(transport) => {
            let parts = McpServerParts {
                operations,
                show_version: settings.mcp_show_version,
                size_limit: size_limit(settings.get_mcp_limit_bytes),
                error_report: Arc::clone(&output.error_report),
                console: Arc::clone(&output.console),
            };
            serve(transport, parts, streams, output).await
        }
    }
}

/// Runs the MCP server until it ends: over stdio, until the client closes
/// the connection or it cannot be read; over HTTP, until the process ends.
/// Returns the exit code.
async fn serve(
    transport: McpTransport,
    parts: McpServerParts,
    streams: CliStreams,
    output: &CliOutput,
) -> u8 {
    match transport {
        McpTransport::Stdio => match mcp_stdio::serve(parts, streams.stdin, streams.stdout).await {
            Ok(()) => 0,
            Err(error) => report(error, output),
        },
        McpTransport::Http { host, port } => match mcp::listen_http(&host, port).await {
            Ok(listener) => {
                cli::report_listening(listener.local_addrs(), output);
                listener.serve(parts).await;
                0
            }
            Err(error) => report(GatewayError::Mcp(error), output),
        },
    }
}

/// What the memory watchdog is started with: the memory limit, and the
/// explanation it writes to stderr when the limit is exceeded.
fn watchdog_settings(settings: &Settings, output: &CliOutput) -> WatchdogSettings {
    let limit = settings.resources_memory_limit_mb;
    let explanation = output.error_report.memory_limit_explanation(limit);
    WatchdogSettings {
        memory_limit_mb: limit,
        message: format!(
            "{}\n",
            formatter::human_explanation(&explanation, output.stderr_color)
        ),
        exit_code: MEMORY_LIMIT_EXIT_CODE,
    }
}

/// An output size limit setting as the limit to apply: 0 applies none.
fn size_limit(bytes: u64) -> Option<u64> {
    (bytes != 0).then_some(bytes)
}

/// `HF_TOKEN`, when it is set to a value that is not empty. A value that is
/// not valid Unicode cannot be a token, and is ignored.
fn hf_token(env: &Environment) -> Option<String> {
    env.get("HF_TOKEN")
        .and_then(OsStr::to_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

/// Writes the explanation of `error` to stderr. Returns the exit code.
fn report(error: GatewayError, output: &CliOutput) -> u8 {
    cli::report_error(&error, output)
}

/// Records what `run_with` is doing now.
fn set(place: &Mutex<FailurePlace<'static>>, now: FailurePlace<'static>) {
    *place.lock().unwrap_or_else(PoisonError::into_inner) = now;
}

/// `format` for reading the arguments, created by `Factories` when one of
/// its functions is first called. Reading the arguments calls one only to
/// check the file type given for a document to put, so that showing the
/// version or the help creates no component. The assembly creates it if it
/// is not yet, and hands the one created to the components.
struct LazyFormat {
    factories: Arc<Factories>,
    made: OnceLock<Arc<dyn Format>>,
}

impl LazyFormat {
    fn made(&self) -> &Arc<dyn Format> {
        self.made.get_or_init(|| (self.factories.format)())
    }
}

impl Format for LazyFormat {
    fn file_type_from_extension(&self, extension: &str) -> Option<FileType> {
        self.made().file_type_from_extension(extension)
    }

    fn file_type_from_mime_or_alias(&self, value: &str) -> Option<FileType> {
        self.made().file_type_from_mime_or_alias(value)
    }

    fn extract(&self, content: &[u8], file_type: &FileType) -> Result<Extracted, FormatError> {
        self.made().extract(content, file_type)
    }
}
