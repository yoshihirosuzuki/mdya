//! Writes logs, messages, and progress to stderr, so that a log or a message
//! written while the one-line progress display is on screen does not break
//! it. Also writes what the libraries that mdya uses write to the process's
//! stdout as warnings in the logs.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::LogSettings;
use super::operations::GatewayProgressSink;
use progress::StderrProgress;
use screen::Screen;

pub(crate) use stdout_pipe::StdoutLogging;

mod fields;
mod filter;
mod pretty;
mod progress;
mod progress_text;
mod receiver;
mod screen;
mod stdout_pipe;
#[cfg(test)]
pub(crate) mod test_double;

/// What `cli` uses from this one. A trait, so that the tests of `cli` can
/// replace it.
pub(crate) trait Console: Send + Sync {
    /// Writes a message or an explanation to stderr as it is: the caller has
    /// already replaced the control characters and the characters that change
    /// the direction of text, and added the line breaks.
    fn write_stderr(&self, text: &str);

    /// Starts showing the progress of one operation.
    fn start_progress(&self, kind: ProgressKind) -> Arc<dyn ProgressDisplay>;
}

/// The progress of one operation, on screen.
pub(crate) trait ProgressDisplay: GatewayProgressSink {
    /// Stops showing the progress. Called once, when the operation ends.
    fn finish(&self);
}

/// Which operation's progress is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgressKind {
    Update,
    RebuildEmbeddings,
    Optimize,
}

/// Which logs to write, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogFilter {
    /// The directives in the syntax of `--log-level` and `RUST_LOG`.
    directives: String,
}

/// A directive in `RUST_LOG` that was not valid and was ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IgnoredDirective {
    /// The directive, without the spaces around it.
    pub(crate) directive: String,
    /// What is wrong with it.
    pub(crate) detail: String,
}

/// `--log-level` was not valid.
#[derive(Debug, thiserror::Error)]
#[error("the log level is not valid")]
pub(crate) struct InvalidLogLevel {
    pub(crate) value: String,
    /// What is wrong with it.
    pub(crate) detail: String,
}

/// Decides which logs to write from `--log-level` (`flag`) and `RUST_LOG`
/// (`env`). An invalid `--log-level` is an error; the invalid directives of
/// `RUST_LOG` are ignored and returned, so that they can be logged as a
/// warning once the log receiver is set. Without `--log-level` and without a
/// valid directive in `RUST_LOG`, the logs from `warn` up are written.
///
/// The directives are separated by commas, and each is in the syntax of
/// `tracing-subscriber`'s `EnvFilter`, with spaces around it allowed. A
/// directive of one word must be a log level, so that a misspelt level is
/// not taken for the name of a target.
pub(crate) fn log_filter(
    flag: Option<&str>,
    env: Option<&OsStr>,
) -> Result<(LogFilter, Vec<IgnoredDirective>), InvalidLogLevel> {
    filter::log_filter(flag, env)
}

/// Measures the width of a terminal, in columns. `None` if it cannot.
pub(crate) type MeasureWidth = Arc<dyn Fn() -> Option<usize> + Send + Sync>;

/// Measures the width of the terminal that the process's stderr is. The real
/// [`MeasureWidth`].
pub(crate) fn stderr_width() -> Option<usize> {
    let (terminal_size::Width(columns), _) = terminal_size::terminal_size_of(std::io::stderr())?;
    Some(usize::from(columns))
}

/// stderr, shared by logs, messages, and progress. The real [`Console`].
pub(crate) struct SharedStderr {
    screen: Arc<Mutex<Screen>>,
    terminal: Option<MeasureWidth>,
}

impl Console for SharedStderr {
    fn write_stderr(&self, text: &str) {
        // A message that cannot be written to stderr has nowhere else to go.
        let _ = lock(&self.screen).write(text.as_bytes());
    }

    fn start_progress(&self, kind: ProgressKind) -> Arc<dyn ProgressDisplay> {
        let screen = Arc::clone(&self.screen);
        Arc::new(StderrProgress::start(screen, kind, self.terminal.clone()))
    }
}

/// Wraps the stderr that `run_with` received. `terminal` measures the width
/// of the terminal that stderr is, where the progress can be drawn again in
/// place on one line; `None` where it cannot, such as a file. Where the width
/// cannot be measured either, the progress is added line by line, at the
/// start and every ten seconds.
pub(crate) fn shared_stderr(
    stderr: Box<dyn Write + Send>,
    terminal: Option<MeasureWidth>,
) -> Arc<SharedStderr> {
    Arc::new(SharedStderr {
        screen: Arc::new(Mutex::new(Screen::new(stderr))),
        terminal,
    })
}

/// Locks `mutex`. A panic while it was locked leaves what it guards fit to
/// use: stderr to write to, and the progress to show.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sets the process-wide log receiver. The real
/// [`super::Factories::set_log_receiver`].
pub(crate) fn set_log_receiver(settings: LogSettings) {
    receiver::set_log_receiver(settings);
}

/// Starts writing what the libraries write to the process's stdout as
/// warnings in the logs, line by line, through the log receiver. `pipe` is
/// the reading end of the pipe on the process's stdout. Reading `pipe` never
/// waits on the logs: up to 256 lines of up to 4096 bytes each, as read, are
/// kept for the logs, a longer line is kept and written in parts, and the
/// lines read while 256 are kept are dropped, with a warning that says how
/// many. Runs on two threads of its own; an error if they cannot be started.
pub(crate) fn log_stdout(pipe: impl Read + Send + 'static) -> io::Result<StdoutLogging> {
    stdout_pipe::log_stdout(pipe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    #[test]
    fn a_message_is_written_to_stderr_as_it_is() {
        let output = CapturedOutput::new();
        let stderr = shared_stderr(Box::new(output.clone()), None);

        stderr.write_stderr("Updated notes.\n");

        assert_eq!(output.text(), "Updated notes.\n");
    }

    fn progress_shown(terminal: Option<MeasureWidth>) -> String {
        let output = CapturedOutput::new();
        let stderr = shared_stderr(Box::new(output.clone()), terminal);

        let display = stderr.start_progress(ProgressKind::Update);
        let shown = output.text();
        display.finish();
        shown
    }

    #[test]
    fn on_a_terminal_the_progress_is_drawn_in_place() {
        let shown = progress_shown(Some(Arc::new(|| Some(80))));

        assert!(
            shown.starts_with('\r') && !shown.contains('\n'),
            "{shown:?}"
        );
    }

    #[test]
    fn on_a_terminal_whose_width_cannot_be_measured_the_progress_is_added_line_by_line() {
        let shown = progress_shown(Some(Arc::new(|| None)));

        assert!(!shown.contains('\r') && shown.ends_with('\n'), "{shown:?}");
    }

    #[test]
    fn elsewhere_the_progress_is_added_line_by_line() {
        let shown = progress_shown(None);

        assert!(!shown.contains('\r') && shown.ends_with('\n'), "{shown:?}");
    }
}
