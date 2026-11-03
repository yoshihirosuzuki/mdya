//! The pretty log format, written by `gateway` itself: tracing-subscriber's
//! pretty format writes the values of a log on its own, without the
//! replacement.

use std::fmt;

use nu_ansi_term::{Color, Style};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

/// Writes a log over several lines, as tracing-subscriber's pretty format
/// lays it out: the time, the level, the target, and the values; where the
/// log was made; and the spans it is in, with their values. The values are
/// written by the log receiver's `FormatFields`.
pub(super) struct PrettyFormat<T> {
    timer: T,
}

impl<T> PrettyFormat<T> {
    pub(super) fn new(timer: T) -> Self {
        Self { timer }
    }
}

impl<S, N, T> FormatEvent<S, N> for PrettyFormat<T>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'w> FormatFields<'w> + 'static,
    T: FormatTime,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        let styles = Styles::new(writer.has_ansi_escapes(), *meta.level());
        writer.write_str("  ")?;
        self.write_time(&mut writer, styles.quiet)?;
        let level = format!("{:>5}", meta.level().as_str());
        write!(
            writer,
            "{} {}: ",
            styles.level.paint(level),
            styles.target.paint(meta.target())
        )?;
        ctx.format_fields(writer.by_ref(), event)?;
        writer.write_char('\n')?;
        write_place(&mut writer, &styles, meta)?;
        write_spans(ctx, &mut writer, &styles, event)?;
        writer.write_char('\n')
    }
}

impl<T: FormatTime> PrettyFormat<T> {
    fn write_time(&self, writer: &mut Writer<'_>, style: Style) -> fmt::Result {
        write!(writer, "{}", style.prefix())?;
        if self.timer.format_time(writer).is_err() {
            writer.write_str("<unknown time>")?;
        }
        write!(writer, "{} ", style.suffix())
    }
}

/// The colors and the styles of the parts, all plain without colors.
struct Styles {
    level: Style,
    target: Style,
    /// For the time and the words `at`, `in`, and `with`.
    quiet: Style,
    span_name: Style,
}

impl Styles {
    /// The colors of tracing-subscriber's pretty format.
    fn new(colored: bool, level: Level) -> Self {
        if !colored {
            let plain = Style::new();
            return Self {
                level: plain,
                target: plain,
                quiet: plain,
                span_name: plain,
            };
        }
        let color = Style::new().fg(level_color(level));
        Self {
            level: color,
            target: color.bold(),
            quiet: Style::new().dimmed().italic(),
            span_name: Style::new().bold(),
        }
    }
}

fn level_color(level: Level) -> Color {
    match level {
        Level::TRACE => Color::Purple,
        Level::DEBUG => Color::Blue,
        Level::INFO => Color::Green,
        Level::WARN => Color::Yellow,
        Level::ERROR => Color::Red,
    }
}

/// Writes the file and the line where the log was made, if it has them.
fn write_place(writer: &mut Writer<'_>, styles: &Styles, meta: &Metadata<'_>) -> fmt::Result {
    let Some(file) = meta.file() else {
        return Ok(());
    };
    write!(writer, "    {} {file}", styles.quiet.paint("at"))?;
    if let Some(line) = meta.line() {
        write!(writer, ":{line}")?;
    }
    writer.write_char('\n')
}

/// Writes the spans the log is in, from the innermost, with their values.
fn write_spans<S, N>(
    ctx: &FmtContext<'_, S, N>,
    writer: &mut Writer<'_>,
    styles: &Styles,
    event: &Event<'_>,
) -> fmt::Result
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'w> FormatFields<'w> + 'static,
{
    let innermost = event
        .parent()
        .and_then(|id| ctx.span(id))
        .or_else(|| ctx.lookup_current());
    for span in innermost.into_iter().flat_map(|span| span.scope()) {
        let meta = span.metadata();
        let name = styles.span_name.paint(meta.name());
        write!(
            writer,
            "    {} {}::{name}",
            styles.quiet.paint("in"),
            meta.target()
        )?;
        let extensions = span.extensions();
        if let Some(fields) = extensions.get::<FormattedFields<N>>()
            && !fields.is_empty()
        {
            write!(writer, " {} {fields}", styles.quiet.paint("with"))?;
        }
        writer.write_char('\n')?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracing::{info_span, warn};
    use tracing_log::log;

    use super::*;
    use crate::gateway::escape::test_double::{EscapeCall, TestEscape};
    use crate::gateway::log_progress::fields::{EscapedFields, Layout};
    use crate::gateway::test_double::captured::CapturedOutput;

    struct FixedTime;

    impl FormatTime for FixedTime {
        fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
            w.write_str("2026-11-02T09:00:00.000000Z")
        }
    }

    /// What the pretty format writes for the logs that `make_logs` makes.
    fn written(ansi: bool, escape: Arc<TestEscape>, make_logs: impl FnOnce()) -> String {
        let output = CapturedOutput::new();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(ansi)
            .event_format(PrettyFormat::new(FixedTime))
            .fmt_fields(EscapedFields::new(escape, Layout::Pretty))
            .finish();
        tracing::subscriber::with_default(subscriber, make_logs);
        output.text()
    }

    fn pretty(make_logs: impl FnOnce()) -> String {
        written(false, Arc::new(TestEscape::new()), make_logs)
    }

    const TARGET: &str = "mdya::gateway::log_progress::pretty::tests";

    #[test]
    fn a_log_is_written_over_lines_with_the_values_through_the_replacement() {
        let (text, line) = (pretty(|| warn!(key = "a", "skipped")), line!());

        assert_eq!(
            text,
            format!(
                "  2026-11-02T09:00:00.000000Z  WARN {TARGET}: «skipped», key: «\"a\"»\n    at {}:{line}\n\n",
                file!()
            )
        );
    }

    #[test]
    fn the_spans_are_written_from_the_innermost_with_their_values() {
        let text = pretty(|| {
            let _outer = info_span!("update", collection = "notes").entered();
            let _inner = info_span!("register").entered();
            warn!("skipped");
        });

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[2..],
            [
                format!("    in {TARGET}::register"),
                format!("    in {TARGET}::update with collection: «\"notes\"»"),
                String::new(),
            ]
        );
    }

    #[test]
    fn a_log_of_the_log_crate_is_written_with_its_own_target_and_place() {
        // What `LogTracer` does with a record, without setting the
        // process-wide receiver of the `log` crate.
        let text = pretty(|| {
            tracing_log::format_trace(
                &log::Record::builder()
                    .target("lance::io")
                    .level(log::Level::Warn)
                    .file(Some("src/io.rs"))
                    .line(Some(7))
                    .args(format_args!("opened"))
                    .build(),
            )
            .unwrap();
        });

        assert_eq!(
            text,
            "  2026-11-02T09:00:00.000000Z  WARN lance::io: «opened»\n    at src/io.rs:7\n\n"
        );
    }

    #[test]
    fn with_colors_only_the_values_go_through_the_replacement() {
        let escape = Arc::new(TestEscape::new());

        let text = written(true, Arc::clone(&escape), || warn!(key = "a", "skipped"));

        assert!(text.contains("\u{1b}[33m"), "{text:?}");
        assert_eq!(
            escape.calls(),
            [
                EscapeCall::ReplaceControls("skipped".to_owned()),
                EscapeCall::ReplaceControls("\"a\"".to_owned()),
            ]
        );
    }
}
