//! Makes the characters mdya writes harmless to terminals: replaces control
//! characters with visible ones in human-readable output, and escapes U+007F
//! and U+0080 to U+009F in JSON output.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;

use tokio::io::AsyncWrite;

#[cfg(test)]
pub(crate) mod test_double;

/// What the other submodules use from this one. A trait, so that their tests
/// can replace it.
pub(crate) trait Escape: Send + Sync {
    /// Replaces the control characters (Unicode general category Cc) in a
    /// string from a source or from the user with visible ones. The caller
    /// passes only such strings, not the line breaks it adds itself.
    fn replace_controls<'a>(&self, text: &'a str) -> Cow<'a, str>;

    /// Wraps a writer that JSON is written to, so that U+007F and U+0080 to
    /// U+009F are written as JSON escapes. The caller flushes only after a
    /// whole JSON value; a character split across a flush is not escaped.
    fn json_writer(&self, inner: Box<dyn Write + Send>) -> Box<dyn Write + Send>;

    /// The same as [`Escape::json_writer`], for an asynchronous writer.
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
    /// Appends `input`, escaped, to `out`. Keeps a split character's first
    /// bytes for the next call.
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>);

    /// Appends the bytes kept from the last call, unchanged, to `out`.
    fn finish(self: Box<Self>, out: &mut Vec<u8>);
}

/// Creates the real one.
pub(crate) fn escape() -> Arc<dyn Escape> {
    todo!()
}
