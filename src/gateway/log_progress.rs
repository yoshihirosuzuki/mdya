//! Writes logs, messages, and progress to stderr, so that a log or a message
//! written while a progress bar is on screen does not break the bar.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::ffi::OsStr;
use std::io::Write;
use std::sync::Arc;

use super::LogSettings;
use super::escape::Escape;
use super::operations::GatewayProgressSink;

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
pub(crate) struct IgnoredDirective(pub(crate) String);

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
/// warning once the log receiver is set.
pub(crate) fn log_filter(
    flag: Option<&str>,
    env: Option<&OsStr>,
) -> Result<(LogFilter, Vec<IgnoredDirective>), InvalidLogLevel> {
    let _ = (flag, env);
    todo!()
}

/// stderr, shared by logs, messages, and progress. The real [`Console`].
pub(crate) struct SharedStderr {}

impl Console for SharedStderr {
    fn write_stderr(&self, text: &str) {
        let _ = text;
        todo!()
    }

    fn start_progress(&self, kind: ProgressKind) -> Arc<dyn ProgressDisplay> {
        let _ = kind;
        todo!()
    }
}

/// Wraps the stderr that `run_with` received. `can_draw_bar` is whether a
/// progress bar can be drawn there; otherwise progress is a line every ten
/// seconds.
pub(crate) fn shared_stderr(
    stderr: Box<dyn Write + Send>,
    can_draw_bar: bool,
    escape: Arc<dyn Escape>,
) -> Arc<SharedStderr> {
    let _ = (stderr, can_draw_bar, escape);
    todo!()
}

/// Sets the process-wide log receiver. The real
/// [`super::Factories::set_log_receiver`].
pub(crate) fn set_log_receiver(settings: LogSettings) {
    let _ = settings;
    todo!()
}
