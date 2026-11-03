//! The log receiver: picks the logs by the filter and writes them to the
//! shared stderr in the chosen log format.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing::Subscriber;
use tracing_subscriber::fmt::time::SystemTime;
use tracing_subscriber::fmt::{self, MakeWriter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use super::super::LogSettings;
use super::super::cli::LogFormat;
use super::super::escape::Escape;
use super::fields::{EscapedFields, Layout};
use super::lock;
use super::pretty::PrettyFormat;
use super::screen::Screen;

/// Sets the receiver for the whole process, and, as the receiver of the
/// `log` crate, `LogTracer`, which hands its records to the receiver.
pub(super) fn set_log_receiver(settings: LogSettings) {
    // `run` sets the receiver once per process. If another receiver was set
    // before, the logs of `tracing` go on to that one; if only another
    // receiver of the `log` crate was, the logs of `log` go on to that one.
    let _ = log_receiver(settings).try_init();
}

/// The receiver, without setting it.
fn log_receiver(settings: LogSettings) -> Box<dyn Subscriber + Send + Sync> {
    let screen = Arc::clone(&settings.stderr.screen);
    let escape = Arc::clone(&settings.stderr.escape);
    let registry = tracing_subscriber::registry().with(settings.filter.env_filter());
    let plain = || LogWriter {
        screen: Arc::clone(&screen),
        json_escape: None,
    };
    match settings.format {
        LogFormat::Compact => Box::new(
            registry.with(
                fmt::layer()
                    .compact()
                    .with_ansi(settings.color)
                    .fmt_fields(EscapedFields::new(escape, Layout::Compact))
                    .with_writer(plain()),
            ),
        ),
        LogFormat::Pretty => Box::new(
            registry.with(
                fmt::layer()
                    .with_ansi(settings.color)
                    .event_format(PrettyFormat::new(SystemTime))
                    .fmt_fields(EscapedFields::new(escape, Layout::Pretty))
                    .with_writer(plain()),
            ),
        ),
        LogFormat::Json => Box::new(registry.with(
            fmt::layer().json().with_ansi(false).with_writer(LogWriter {
                screen: Arc::clone(&screen),
                json_escape: Some(escape),
            }),
        )),
    }
}

/// Makes the writer of each log: the shared stderr, behind the JSON escapes
/// in the JSON log format. tracing-subscriber writes a whole log, ending in a
/// line break, at once and drops the writer without flushing it; dropping the
/// JSON escapes writes what they hold, and they hold nothing after a line
/// break.
struct LogWriter {
    screen: Arc<Mutex<Screen>>,
    json_escape: Option<Arc<dyn Escape>>,
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = Box<dyn Write + Send>;

    fn make_writer(&'a self) -> Self::Writer {
        let line = Box::new(LogLine(Arc::clone(&self.screen)));
        match &self.json_escape {
            Some(escape) => escape.json_writer(line),
            None => line,
        }
    }
}

/// The writer of one log. tracing-subscriber writes a whole log at once.
struct LogLine(Arc<Mutex<Screen>>);

impl Write for LogLine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock(&self.0).write(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracing::{debug, info, warn};

    use super::*;
    use crate::gateway::cli::LogFormat;
    use crate::gateway::escape::test_double::{EscapeCall, TestEscape};
    use crate::gateway::log_progress::lock;
    use crate::gateway::log_progress::{SharedStderr, log_filter, shared_stderr};
    use crate::gateway::test_double::captured::CapturedOutput;

    struct Receiver {
        output: CapturedOutput,
        escape: Arc<TestEscape>,
        stderr: Arc<SharedStderr>,
    }

    fn receiver() -> Receiver {
        let output = CapturedOutput::new();
        let escape = Arc::new(TestEscape::new());
        let stderr = shared_stderr(Box::new(output.clone()), None, escape.clone());
        Receiver {
            output,
            escape,
            stderr,
        }
    }

    impl Receiver {
        /// What the receiver writes for the logs that `make_logs` makes.
        fn written(
            &self,
            format: LogFormat,
            color: bool,
            level: &str,
            make_logs: impl FnOnce(),
        ) -> String {
            let (filter, _) = log_filter(Some(level), None).unwrap();
            let settings = LogSettings {
                filter,
                format,
                color,
                stderr: Arc::clone(&self.stderr),
            };
            tracing::subscriber::with_default(log_receiver(settings), make_logs);
            self.output.text()
        }
    }

    #[test]
    fn the_compact_format_writes_a_line_with_the_values_through_the_replacement() {
        let text = receiver().written(LogFormat::Compact, false, "warn", || warn!("skipped"));

        let line = text.strip_suffix('\n').unwrap();
        assert!(
            line.ends_with(" WARN mdya::gateway::log_progress::receiver::tests: «skipped»"),
            "{text:?}"
        );
    }

    #[test]
    fn the_logs_below_the_level_are_not_written() {
        let text = receiver().written(LogFormat::Compact, false, "warn", || info!("opened"));

        assert_eq!(text, "");
    }

    #[test]
    fn a_directive_for_a_target_is_used() {
        let text = receiver().written(LogFormat::Compact, false, "warn,mdya=debug", || {
            debug!("opened")
        });

        assert!(text.contains("«opened»"), "{text:?}");
    }

    /// A record of the `log` crate, at warn, from `target`, handed to the
    /// receiver as `LogTracer` would.
    fn log_record(target: &str) {
        tracing_log::format_trace(
            &tracing_log::log::Record::builder()
                .target(target)
                .level(tracing_log::log::Level::Warn)
                .args(format_args!("opened"))
                .build(),
        )
        .unwrap();
    }

    #[test]
    fn a_log_of_the_log_crate_is_picked_by_its_own_target() {
        let text = receiver().written(LogFormat::Compact, false, "warn,lance=off", || {
            log_record("lance::io");
            log_record("datafusion::sql");
        });

        assert!(!text.contains("lance::io"), "{text:?}");
        assert!(text.contains("datafusion::sql: «opened»"), "{text:?}");
    }

    #[test]
    fn the_pretty_format_writes_the_place_on_its_own_line() {
        let text = receiver().written(LogFormat::Pretty, false, "warn", || warn!("skipped"));

        assert!(
            text.contains(&format!("«skipped»\n    at {}:", file!())),
            "{text:?}"
        );
    }

    #[test]
    fn the_json_format_writes_through_the_json_escapes() {
        let receiver = receiver();

        let text = receiver.written(LogFormat::Json, false, "warn", || {
            warn!(key = "a", "skipped")
        });

        let log: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(log["fields"]["message"], "skipped");
        assert_eq!(log["fields"]["key"], "a");
        assert!(receiver.escape.calls().contains(&EscapeCall::JsonWriter));
        assert!(
            !receiver
                .escape
                .calls()
                .iter()
                .any(|call| matches!(call, EscapeCall::ReplaceControls(_)))
        );
    }

    #[test]
    fn colors_are_written_only_when_asked() {
        let colored = receiver().written(LogFormat::Compact, true, "warn", || warn!("skipped"));
        let plain = receiver().written(LogFormat::Compact, false, "warn", || warn!("skipped"));

        assert!(colored.contains('\u{1b}'), "{colored:?}");
        assert!(!plain.contains('\u{1b}'), "{plain:?}");
    }

    #[test]
    fn a_log_is_written_above_the_progress_display() {
        let receiver = receiver();
        lock(&receiver.stderr.screen)
            .draw_progress("3/10".to_owned())
            .unwrap();

        let text = receiver.written(LogFormat::Compact, false, "warn", || warn!("skipped"));

        assert!(text.starts_with("\r3/10\r    \r"), "{text:?}");
        assert!(text.ends_with("«skipped»\n\r3/10"), "{text:?}");
    }
}
