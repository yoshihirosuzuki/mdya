//! Writes the values of a log in the log formats that are not JSON, with the
//! control characters and the characters that change the direction of text
//! in each value replaced.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use nu_ansi_term::Style;
use tracing::field::{Field, Visit};
use tracing::span;
use tracing_subscriber::field::{RecordFields, VisitOutput};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FormatFields, FormattedFields};

use super::super::escape::Escape;

/// How the values are laid out.
#[derive(Clone, Copy, Debug)]
pub(super) enum Layout {
    /// `message name=value name=value`, as tracing-subscriber's compact
    /// format lays them out.
    Compact,
    /// `message, name: value, name: value`, as its pretty format does.
    Pretty,
}

impl Layout {
    fn separator(self) -> &'static str {
        match self {
            Self::Compact => " ",
            Self::Pretty => ", ",
        }
    }
}

/// The `FormatFields` of the log formats that are not JSON.
pub(super) struct EscapedFields {
    escape: Arc<dyn Escape>,
    layout: Layout,
}

impl EscapedFields {
    pub(super) fn new(escape: Arc<dyn Escape>, layout: Layout) -> Self {
        Self { escape, layout }
    }

    /// The visitor that writes to `writer`, which holds values already if
    /// `is_empty` is false.
    fn visitor<'a>(&self, writer: Writer<'a>, is_empty: bool) -> EscapingVisitor<'a> {
        EscapingVisitor {
            writer,
            escape: Arc::clone(&self.escape),
            layout: self.layout,
            is_empty,
            result: Ok(()),
        }
    }
}

impl<'writer> FormatFields<'writer> for EscapedFields {
    fn format_fields<R: RecordFields>(&self, writer: Writer<'writer>, fields: R) -> fmt::Result {
        let mut visitor = self.visitor(writer, true);
        fields.record(&mut visitor);
        visitor.finish()
    }

    /// Adds the values recorded on a span after it was made, after a
    /// separator of the layout, as tracing-subscriber's pretty fields do; the
    /// default would put a space before them in either layout.
    fn add_fields(
        &self,
        current: &'writer mut FormattedFields<Self>,
        fields: &span::Record<'_>,
    ) -> fmt::Result {
        let is_empty = current.is_empty();
        let mut visitor = self.visitor(current.as_writer(), is_empty);
        fields.record(&mut visitor);
        visitor.finish()
    }
}

/// Writes the values of one log or one span, as tracing-subscriber's
/// `DefaultVisitor` and `PrettyVisitor` do, except that each value goes
/// through the replacement.
pub(super) struct EscapingVisitor<'a> {
    writer: Writer<'a>,
    escape: Arc<dyn Escape>,
    layout: Layout,
    /// Whether no value has been written yet.
    is_empty: bool,
    result: fmt::Result,
}

impl EscapingVisitor<'_> {
    /// Writes the message, or a value after its name. `text` is the value as
    /// text, before the replacement.
    fn write(&mut self, name: &str, text: &str) {
        if self.result.is_ok() {
            self.result = self.write_separated(name, text);
        }
    }

    fn write_separated(&mut self, name: &str, text: &str) -> fmt::Result {
        if !std::mem::replace(&mut self.is_empty, false) {
            self.writer.write_str(self.layout.separator())?;
        }
        if name != "message" {
            self.write_name(name.strip_prefix("r#").unwrap_or(name))?;
        }
        let replaced = self.escape.replace_controls(text);
        self.writer.write_str(&replaced)
    }

    fn write_name(&mut self, name: &str) -> fmt::Result {
        let style = |style: Style| match self.writer.has_ansi_escapes() {
            true => style,
            false => Style::new(),
        };
        match self.layout {
            Layout::Compact => {
                let (italic, dimmed) = (style(Style::new().italic()), style(Style::new().dimmed()));
                write!(self.writer, "{}{}", italic.paint(name), dimmed.paint("="))
            }
            Layout::Pretty => write!(self.writer, "{}: ", style(Style::new().bold()).paint(name)),
        }
    }
}

impl Visit for EscapingVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.write(field.name(), value),
            _ => self.record_debug(field, &value),
        }
    }

    fn record_error(&mut self, field: &Field, value: &(dyn Error + 'static)) {
        let mut text = value.to_string();
        let sources: Vec<String> = std::iter::successors(value.source(), |&error| error.source())
            .map(ToString::to_string)
            .collect();
        if !sources.is_empty() {
            let (name, sources) = (field.name(), sources.join(", "));
            text.push_str(&match self.layout {
                Layout::Compact => format!(" {name}.sources=[{sources}]"),
                Layout::Pretty => format!(", {name}.sources: [{sources}]"),
            });
        }
        self.write(field.name(), &text);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // tracing-log puts the target, the module path, the file, and the
        // line of a record of the `log` crate in values named `log.…`; the
        // formats write those where they write the ones of tracing's logs.
        if !field.name().starts_with("log.") {
            self.write(field.name(), &format!("{value:?}"));
        }
    }
}

impl VisitOutput<fmt::Result> for EscapingVisitor<'_> {
    fn finish(self) -> fmt::Result {
        self.result
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use tracing::{info_span, warn};

    use super::*;
    use crate::gateway::escape::test_double::{EscapeCall, TestEscape};
    use crate::gateway::test_double::captured::CapturedOutput;

    /// What the compact format writes, with these fields in `layout`, for
    /// the logs that `make_logs` makes, without the time, the level, and the
    /// target.
    fn written(
        layout: Layout,
        ansi: bool,
        escape: Arc<TestEscape>,
        make_logs: impl FnOnce(),
    ) -> String {
        let output = CapturedOutput::new();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(ansi)
            .without_time()
            .with_level(false)
            .with_target(false)
            .compact()
            .fmt_fields(EscapedFields::new(escape, layout))
            .finish();
        tracing::subscriber::with_default(subscriber, make_logs);
        output.text()
    }

    fn compact(make_logs: impl FnOnce()) -> String {
        written(
            Layout::Compact,
            false,
            Arc::new(TestEscape::new()),
            make_logs,
        )
    }

    #[test]
    fn the_message_goes_through_the_replacement() {
        assert_eq!(compact(|| warn!("ignoring {}", "a")), "«ignoring a»\n");
    }

    #[test]
    fn each_value_goes_through_the_replacement_after_its_name() {
        let text = compact(|| warn!(key = "notes/a.md", count = 3, "skipped"));

        assert_eq!(text, "«skipped» key=«\"notes/a.md\"» count=«3»\n");
    }

    #[test]
    fn a_value_written_with_display_goes_through_the_replacement_as_it_is() {
        let text = compact(|| warn!(path = %"a\nb", "skipped"));

        assert_eq!(text, "«skipped» path=«a\nb»\n");
    }

    #[test]
    fn a_raw_identifier_is_written_without_its_prefix() {
        let text = compact(|| warn!(r#type = "md", "skipped"));

        assert_eq!(text, "«skipped» type=«\"md\"»\n");
    }

    #[test]
    fn the_values_of_the_log_crate_are_skipped() {
        // tracing-log puts the target, the module path, the file, and the
        // line of a `log` record in values named `log.…`, and the formats
        // write them where they write those of tracing's own logs.
        let text = compact(|| warn!(log.target = "lance::io", "opened"));

        assert_eq!(text, "«opened»\n");
    }

    #[derive(Debug, thiserror::Error)]
    #[error("cannot read the index")]
    struct Outer(#[source] Inner);

    #[derive(Debug, thiserror::Error)]
    #[error("no such file")]
    struct Inner;

    #[test]
    fn an_error_is_written_with_its_sources() {
        let error = Outer(Inner);

        let text = compact(|| warn!(error = &error as &dyn Error, "failed"));

        assert_eq!(
            text,
            "«failed» error=«cannot read the index error.sources=[no such file]»\n"
        );
    }

    #[test]
    fn an_error_without_sources_is_written_alone() {
        let text = compact(|| warn!(error = &Inner as &dyn Error, "failed"));

        assert_eq!(text, "«failed» error=«no such file»\n");
    }

    #[test]
    fn the_values_of_a_span_go_through_the_replacement() {
        let text = compact(|| {
            let _span = info_span!("update", collection = "notes").entered();
            warn!("skipped");
        });

        assert_eq!(text, "update: «skipped» collection=«\"notes\"»\n");
    }

    /// What the compact format writes, with the fields in `layout`, for a
    /// span whose second value is recorded after the span was made.
    fn recorded_later(layout: Layout) -> String {
        written(layout, false, Arc::new(TestEscape::new()), || {
            let span = info_span!("update", a = 1, b = tracing::field::Empty).entered();
            span.record("b", 2);
            warn!("skipped");
        })
    }

    #[test]
    fn a_value_recorded_later_follows_the_others_as_they_are_laid_out() {
        assert_eq!(
            recorded_later(Layout::Compact),
            "update: «skipped» a=«1» b=«2»\n"
        );
        assert_eq!(
            recorded_later(Layout::Pretty),
            "update: «skipped» a: «1», b: «2»\n"
        );
    }

    #[test]
    fn the_pretty_layout_separates_the_values_with_commas_and_colons() {
        let text = written(Layout::Pretty, false, Arc::new(TestEscape::new()), || {
            warn!(key = "notes/a.md", count = 3, "skipped")
        });

        assert_eq!(text, "«skipped», key: «\"notes/a.md\"», count: «3»\n");
    }

    #[test]
    fn with_colors_only_the_values_go_through_the_replacement() {
        let escape = Arc::new(TestEscape::new());

        let text = written(Layout::Compact, true, Arc::clone(&escape), || {
            warn!(key = "a", "skipped")
        });

        assert!(text.contains('\u{1b}'), "{text:?}");
        assert_eq!(
            escape.calls(),
            [
                EscapeCall::ReplaceControls("skipped".to_owned()),
                EscapeCall::ReplaceControls("\"a\"".to_owned()),
            ]
        );
    }
}
