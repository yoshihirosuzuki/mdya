//! The component that talks to the user: the command line, the MCP server,
//! stdout, and environment variables.
//!
//! `print!` and `println!` are allowed anywhere in this module.
//! `std::io::stdout` and the environment variable functions stay disallowed
//! here as well (clippy.toml); allow `clippy::disallowed_methods` on each
//! function that needs one of them.
//!
//! The work is split into submodules that use each other only through the
//! functions and types they declare for that, so that each can be built and
//! tested on its own: `entry` assembles the components and picks between the
//! command line and the MCP server, `operations` calls the components for each
//! operation, `cli` and `mcp` talk to the user, `write_lock` keeps one write
//! operation at a time, `log_progress` writes logs and progress to stderr,
//! with what the libraries write to stdout as warnings in the logs,
//! `formatter` makes what is shown the user, for each output format, keep
//! the rules on characters and numbers, and `error_report` turns every error
//! into an exit code, an MCP error's code and details, and what its
//! explanation holds.

#![allow(clippy::print_stdout)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::collection::Collections;
use crate::config::Config;
use crate::embedding::{Embedding, EmbeddingSettings};
use crate::format::Format;
use crate::index::Index;
use crate::ingest::Ingest;
use crate::query::Query;
use crate::types::{CollectionType, DirLookup};
use crate::watchdog::WatchdogSettings;

mod cli;
mod entry;
mod error_report;
mod formatter;
mod log_progress;
mod mcp;
mod operations;
#[cfg(test)]
mod test_double;
mod write_lock;

/// Runs mdya with the process's arguments, environment variables, stdin,
/// stdout, and stderr, and the real components. Called from the executable's
/// `main`.
///
/// # Safety
///
/// No other thread may be running in the process when it is called: it sets
/// environment variables on the process (with `std::env::set_var`) before it
/// starts any thread of its own, which is sound only while no other thread
/// can read or set them.
pub unsafe fn run() -> ExitCode {
    // SAFETY: the caller keeps to the same condition.
    unsafe { entry::run() }
}

/// Runs mdya with the given arguments (the first one is the program name),
/// environment variables, input and output, and functions that create the
/// components. Tests call it with test doubles; [`run`] calls it with the real
/// ones.
pub fn run_with(
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    io: Io,
    factories: Factories,
) -> ExitCode {
    entry::run_with(args, env, io, factories)
}

/// Where mdya reads and writes. [`run`] passes the process's stdin, stdout,
/// and stderr.
pub struct Io {
    /// Where the answers to confirmations are read from.
    pub stdin: Box<dyn BufRead + Send>,
    /// Where the results of operations are written.
    pub stdout: Box<dyn Write + Send>,
    /// Where messages, errors, and logs are written.
    pub stderr: Box<dyn Write + Send>,
    pub stdin_is_terminal: bool,
    pub stdout_is_terminal: bool,
    pub stderr_is_terminal: bool,
}

/// How logs are written. `run_with` hands it to
/// [`Factories::set_log_receiver`]; only this crate reads it.
pub struct LogSettings {
    /// Which logs to write, already checked.
    pub(crate) filter: log_progress::LogFilter,
    pub(crate) format: cli::LogFormat,
    /// Whether to color the logs.
    pub(crate) color: bool,
    /// Where to write them: stderr, shared with messages and progress so that
    /// a log written while the one-line progress display is on screen does
    /// not break it.
    pub(crate) stderr: Arc<log_progress::SharedStderr>,
}

/// The functions that create the components. Tests replace them with ones
/// that return test doubles.
#[expect(
    clippy::type_complexity,
    reason = "each field spells out the function it holds, so that a test can see what to pass"
)]
pub struct Factories {
    /// Sets the receiver of the logs. The real one sets the process-wide
    /// receiver, which can be set only once per process, so tests replace it.
    pub set_log_receiver: Box<dyn Fn(LogSettings) + Send + Sync>,
    /// Sets the given environment variables on the process. The real one sets
    /// them; tests replace it with one that sets nothing, because setting
    /// environment variables is unsound while other threads run.
    pub set_process_environment: Box<dyn Fn(Vec<(OsString, OsString)>) + Send + Sync>,
    pub config: Box<dyn Fn(PathBuf) -> Arc<dyn Config> + Send + Sync>,
    /// Starts the memory watchdog. The watchdog runs while the returned value
    /// is kept.
    pub start_watchdog: Box<dyn Fn(WatchdogSettings) -> Box<dyn Send> + Send + Sync>,
    pub embedding: Box<dyn Fn(EmbeddingSettings) -> Arc<dyn Embedding> + Send + Sync>,
    pub index: Box<dyn Fn(PathBuf, Arc<dyn Embedding>) -> Arc<dyn Index> + Send + Sync>,
    pub collections: Box<dyn Fn(PathBuf, Arc<dyn Index>) -> Arc<dyn Collections> + Send + Sync>,
    pub query: Box<dyn Fn(Arc<dyn Embedding>, Arc<dyn Index>) -> Arc<dyn Query> + Send + Sync>,
    pub format: Box<dyn Fn() -> Arc<dyn Format> + Send + Sync>,
    /// Creates the ingest implementation for each collection type.
    pub ingests: Box<
        dyn Fn(Arc<dyn Format>, Arc<dyn Index>) -> BTreeMap<CollectionType, Arc<dyn Ingest>>
            + Send
            + Sync,
    >,
    /// Looks up the home directory and the working directory, to make the
    /// settings and index directory, the embedding model cache directory, and
    /// the places in type-specific attributes absolute. Tests replace it with
    /// one that gives fixed answers.
    pub dirs: DirLookup,
}
