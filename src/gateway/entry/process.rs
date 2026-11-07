//! What only `run` does, for the process as a whole, around `run_with`:
//! replaces the panic hook, puts a pipe on the process's stdout, and creates
//! the real components.

use std::any::Any;
use std::backtrace::{Backtrace, BacktraceStatus};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, BufReader, IsTerminal, PipeReader, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::format::Format;
use crate::index::Index;
use crate::ingest::Ingest;
use crate::types::{CollectionType, DirLookup};

use super::super::formatter::{self, PanicClue};
use super::super::log_progress::{self, StdoutLogging};
use super::super::operations::version;
use super::super::{Factories, Io};
use super::stdout_pipe::{self, AttachedPipe};

/// The body of [`super::super::run`].
///
/// # Safety
///
/// No other thread may be running in the process when it is called, as it
/// sets the process's environment variables before it starts any thread of
/// its own.
pub(in crate::gateway) unsafe fn run() -> ExitCode {
    // Before any other thread starts, and before the pipe, so that whether
    // stdout is a terminal is the answer for the user's stdout.
    replace_panic_hook();
    let (stdin_is_terminal, stdout_is_terminal, stderr_is_terminal) = terminals();
    let (stdout, pipe) = match stdout_pipe::attach() {
        Ok(AttachedPipe { stdout, reader }) => (stdout, Pipe::Attached(reader)),
        Err(error) => (process_stdout(), Pipe::Failed(error)),
    };
    let attached = matches!(pipe, Pipe::Attached(_));
    let logging = Arc::new(Mutex::new(None));
    let io = Io {
        stdin: Box::new(BufReader::new(io::stdin())),
        stdout,
        stderr: Box::new(io::stderr()),
        stdin_is_terminal,
        stdout_is_terminal,
        stderr_is_terminal,
    };
    let factories = real_factories(pipe, Arc::clone(&logging));
    let code = super::run_with(std::env::args_os().collect(), environment(), io, factories);
    if attached {
        finish_logging(&logging);
    }
    code
}

/// Replaces Rust's default panic hook with one that writes the version, the
/// location, the message, and the backtrace, with their control characters
/// and the characters that change the direction of text replaced.
fn replace_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        });
        let backtrace = Backtrace::force_capture();
        let backtrace =
            (backtrace.status() == BacktraceStatus::Captured).then(|| backtrace.to_string());
        let version = version();
        let text = formatter::human_panic(&PanicClue {
            version: &version.version,
            location: location.as_deref(),
            message: panic_message(info.payload()),
            backtrace: backtrace.as_deref(),
        });
        // Written at once, so that what other threads write does not split it.
        let _ = io::stderr().lock().write_all(text.as_bytes());
    }));
}

/// The message that a panic was given, as `panic!` and the standard library
/// give one.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("Box<dyn Any>")
}

/// Whether stdin, stdout, and stderr are terminals.
#[expect(
    clippy::disallowed_methods,
    reason = "whether the process's stdout is a terminal decides the colors and the progress"
)]
fn terminals() -> (bool, bool, bool) {
    (
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
    )
}

/// The process's stdout, for when no pipe could be put on it.
#[expect(
    clippy::disallowed_methods,
    reason = "mdya writes its results to the process's stdout when no pipe could be put on it"
)]
fn process_stdout() -> Box<dyn Write + Send> {
    Box::new(io::stdout())
}

/// The process's environment variables.
#[expect(
    clippy::disallowed_methods,
    reason = "`run` reads the environment variables and passes them to `run_with`"
)]
fn environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os().collect()
}

/// Whether the pipe went on the process's stdout.
enum Pipe {
    /// Its reading end.
    Attached(PipeReader),
    Failed(io::Error),
}

/// The real components, and the real functions that set the log receiver
/// and the process's environment variables. Setting the log receiver also
/// starts writing what is read from the pipe to the logs, as the logs can
/// take it only from then on.
fn real_factories(pipe: Pipe, logging: Arc<Mutex<Option<StdoutLogging>>>) -> Factories {
    let pipe = Mutex::new(Some(pipe));
    Factories {
        set_log_receiver: Box::new(move |settings| {
            log_progress::set_log_receiver(settings);
            if let Some(pipe) = lock(&pipe).take() {
                log_libraries_stdout(pipe, &logging);
            }
        }),
        set_process_environment: Box::new(set_process_environment),
        config: Box::new(crate::config::create),
        start_watchdog: Box::new(|settings| Box::new(crate::watchdog::start(settings))),
        embedding: Box::new(crate::embedding::create),
        index: Box::new(crate::index::create),
        collections: Box::new(crate::collection::create),
        query: Box::new(crate::query::create),
        format: Box::new(crate::format::create),
        ingests: Box::new(ingests),
        dirs: DirLookup {
            home_dir: Arc::new(std::env::home_dir),
            current_dir: Arc::new(std::env::current_dir),
        },
    }
}

/// Starts writing what is read from the pipe to the logs, or warns that it
/// cannot.
fn log_libraries_stdout(pipe: Pipe, logging: &Mutex<Option<StdoutLogging>>) {
    let reader = match pipe {
        Pipe::Attached(reader) => reader,
        Pipe::Failed(error) => {
            tracing::warn!(
                %error,
                "no pipe could be put on stdout, so what the libraries write to stdout goes to stdout"
            );
            return;
        }
    };
    match log_progress::log_stdout(reader) {
        Ok(started) => *lock(logging) = Some(started),
        // The reading end is dropped, so that a write to stdout fails at
        // once instead of waiting for a reader that never comes.
        Err(error) => tracing::warn!(
            %error,
            "what the libraries write to stdout cannot be read, so their writes to stdout fail"
        ),
    }
}

/// Sets the given environment variables on the process.
#[expect(
    clippy::disallowed_methods,
    reason = "the one place that sets environment variables, which `run_with` calls before any \
              other thread starts"
)]
fn set_process_environment(variables: Vec<(OsString, OsString)>) {
    for (name, value) in variables {
        // SAFETY: no other thread is running. `run` is called when none is
        // (its `# Safety`), starts none before `run_with`, and `run_with`
        // calls this before it starts any: the runtime and the log receiver
        // come after it.
        unsafe { std::env::set_var(name, value) };
    }
}

/// The ingest implementation for each collection type.
fn ingests(
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
) -> BTreeMap<CollectionType, Arc<dyn Ingest>> {
    BTreeMap::from([
        (
            CollectionType("directory".to_owned()),
            crate::file_ingest::create(Arc::clone(&format), Arc::clone(&index)),
        ),
        (
            CollectionType("archive".to_owned()),
            crate::archive_ingest::create(Arc::clone(&format), Arc::clone(&index)),
        ),
        (
            CollectionType("raw".to_owned()),
            crate::raw_ingest::create(format, index),
        ),
    ])
}

/// Waits until what the libraries wrote to stdout is in the logs, for at
/// most [`LOGGING_FINISH_LIMIT`]. The pipe's writing end closes once the null
/// device takes its place on stdout; then what reads the pipe comes to its
/// end.
fn finish_logging(logging: &Mutex<Option<StdoutLogging>>) {
    // Without the null device in its place, the writing end stays open, and
    // waiting would never end.
    if stdout_pipe::detach().is_err() {
        return;
    }
    if let Some(logging) = lock(logging).take() {
        let _ = logging.finish_within(LOGGING_FINISH_LIMIT);
    }
}

/// How long `run` waits, at its end, for what the libraries wrote to stdout
/// to be in the logs: writing the logs may wait on a stderr that nobody
/// reads, and mdya ends anyway.
const LOGGING_FINISH_LIMIT: Duration = Duration::from_secs(5);

/// Locks `mutex`, which holds no state that a panic could leave half made.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
