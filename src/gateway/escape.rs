//! Keeps the text mdya writes from controlling the terminal or from showing
//! as something it is not. In human-readable output, replaces control
//! characters and the characters that change the direction of text with
//! visible ones; in JSON output, escapes U+007F, U+0080 to U+009F, and the
//! characters that change the direction of text. The replacing is the
//! formatter's character rule; the formatter is to take over the rest as the
//! other submodules move to it.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the rest of `gateway` does not use these yet")
)]

use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;

use tokio::io::AsyncWrite;

use super::formatter;

mod json;
#[cfg(test)]
pub(crate) mod test_double;

/// What the other submodules use from this one. A trait, so that their tests
/// can replace it.
pub(crate) trait Escape: Send + Sync {
    /// Replaces the control characters (Unicode general category Cc) and the
    /// characters that change the direction of text (U+202A to U+202E and
    /// U+2066 to U+2069) in a string that comes from ingested data or from
    /// the user with visible ones: `\n`, `\t`, and `\r` for line feed, tab,
    /// and carriage return, and `\u{1b}`, `\u{202e}`, and the like for the
    /// others. The caller passes only such strings, not the line breaks it
    /// adds itself.
    fn replace_controls<'a>(&self, text: &'a str) -> Cow<'a, str>;

    /// Wraps a writer that JSON is written to, so that U+007F, U+0080 to
    /// U+009F, and the characters that change the direction of text are
    /// written as JSON escapes. The caller flushes only after a whole JSON
    /// value; a character split across a flush is not escaped. A write is
    /// accepted before it reaches the inner writer; if the inner writer
    /// fails, a later `write` or `flush` tries the bytes again and returns
    /// the error if it fails again.
    fn json_writer(&self, inner: Box<dyn Write + Send>) -> Box<dyn Write + Send>;

    /// The same as [`Escape::json_writer`], for an asynchronous writer, except
    /// that the bytes of a write reach the inner writer only at the next
    /// write, flush, or shutdown, and are lost if the writer is dropped
    /// before that. So the caller flushes after each whole JSON value.
    fn async_json_writer(
        &self,
        inner: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Box<dyn AsyncWrite + Send + Unpin>;

    /// Escapes JSON that arrives in pieces, such as a response body.
    fn json_escaper(&self) -> Box<dyn JsonEscape>;
}

/// Escapes JSON that arrives in pieces. A character split between two pieces
/// is escaped all the same.
pub(crate) trait JsonEscape: Send {
    /// Appends `input`, escaped, to `out`. Keeps up to two trailing bytes
    /// that may begin a character to escape until the next call.
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>);

    /// Appends the bytes kept from the last call, if any, unchanged, to `out`.
    fn finish(self: Box<Self>, out: &mut Vec<u8>);
}

/// Creates the real one.
pub(crate) fn escape() -> Arc<dyn Escape> {
    Arc::new(Escaping)
}

struct Escaping;

impl Escape for Escaping {
    fn replace_controls<'a>(&self, text: &'a str) -> Cow<'a, str> {
        formatter::replace_controls(text)
    }

    fn json_writer(&self, inner: Box<dyn Write + Send>) -> Box<dyn Write + Send> {
        Box::new(json::JsonEscapeWriter::new(inner))
    }

    fn async_json_writer(
        &self,
        inner: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Box<dyn AsyncWrite + Send + Unpin> {
        Box::new(json::AsyncJsonEscapeWriter::new(inner))
    }

    fn json_escaper(&self) -> Box<dyn JsonEscape> {
        Box::new(json::JsonEscaper::new())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    #[test]
    fn the_real_one_replaces_control_characters() {
        assert_eq!(escape().replace_controls("a\u{1b}b"), "a\\u{1b}b");
    }

    #[test]
    fn the_real_json_writer_escapes() {
        let output = CapturedOutput::new();

        let mut writer = escape().json_writer(Box::new(output.clone()));
        writer.write_all("\"\u{7f}\"".as_bytes()).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), br#""\u007f""#);
    }

    #[tokio::test]
    async fn the_real_async_json_writer_escapes() {
        let (writer_end, mut reader_end) = tokio::io::duplex(64);

        let mut writer = escape().async_json_writer(Box::new(writer_end));
        writer.write_all("\"\u{9b}\"".as_bytes()).await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        reader_end.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, br#""\u009b""#);
    }

    #[test]
    fn the_real_json_escaper_escapes_across_pieces() {
        let bytes = "\"\u{80}\"".as_bytes();

        let mut escaper = escape().json_escaper();
        let mut out = Vec::new();
        escaper.push(&bytes[..2], &mut out);
        escaper.push(&bytes[2..], &mut out);
        escaper.finish(&mut out);

        assert_eq!(out, br#""\u0080""#);
    }
}
