//! A test double for [`Escape`], for the tests of the submodules that use it.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::borrow::Cow;
use std::io::Write;
use std::sync::Mutex;

use tokio::io::AsyncWrite;

use super::{Escape, JsonEscape};

/// Marks what it replaces instead of replacing it, so that a test can see
/// which strings went through [`Escape::replace_controls`]: `text` becomes
/// `«text»`. The JSON writers and the escaper pass bytes through unchanged.
/// Records every call.
#[derive(Default)]
pub(crate) struct TestEscape {
    calls: Mutex<Vec<EscapeCall>>,
}

/// A call the double received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EscapeCall {
    ReplaceControls(String),
    JsonWriter,
    AsyncJsonWriter,
    JsonEscaper,
}

impl TestEscape {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The calls received so far, in order.
    pub(crate) fn calls(&self) -> Vec<EscapeCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: EscapeCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Escape for TestEscape {
    fn replace_controls<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.record(EscapeCall::ReplaceControls(text.to_owned()));
        Cow::Owned(format!("«{text}»"))
    }

    fn json_writer(&self, inner: Box<dyn Write + Send>) -> Box<dyn Write + Send> {
        self.record(EscapeCall::JsonWriter);
        inner
    }

    fn async_json_writer(
        &self,
        inner: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Box<dyn AsyncWrite + Send + Unpin> {
        self.record(EscapeCall::AsyncJsonWriter);
        inner
    }

    fn json_escaper(&self) -> Box<dyn JsonEscape> {
        self.record(EscapeCall::JsonEscaper);
        Box::new(PassThrough)
    }
}

struct PassThrough;

impl JsonEscape for PassThrough {
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(input);
    }

    fn finish(self: Box<Self>, _out: &mut Vec<u8>) {}
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    #[test]
    fn replace_controls_marks_the_text_and_records_it() {
        let escape = TestEscape::new();

        let replaced = escape.replace_controls("a\nb");

        assert_eq!(replaced, "«a\nb»");
        assert_eq!(
            escape.calls(),
            vec![EscapeCall::ReplaceControls("a\nb".to_owned())]
        );
    }

    #[test]
    fn json_writer_passes_bytes_through_and_records_the_call() {
        let escape = TestEscape::new();
        let output = CapturedOutput::new();

        let mut writer = escape.json_writer(Box::new(output.clone()));
        writer.write_all("\u{7f}{}".as_bytes()).unwrap();

        assert_eq!(output.text(), "\u{7f}{}");
        assert_eq!(escape.calls(), vec![EscapeCall::JsonWriter]);
    }

    #[tokio::test]
    async fn async_json_writer_passes_bytes_through_and_records_the_call() {
        let escape = TestEscape::new();
        let (writer_end, mut reader_end) = tokio::io::duplex(64);

        let mut writer = escape.async_json_writer(Box::new(writer_end));
        writer.write_all(b"{}").await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader_end, &mut read)
            .await
            .unwrap();

        assert_eq!(read, b"{}");
        assert_eq!(escape.calls(), vec![EscapeCall::AsyncJsonWriter]);
    }

    #[test]
    fn json_escaper_passes_pieces_through_and_records_the_call() {
        let escape = TestEscape::new();

        let mut escaper = escape.json_escaper();
        let mut out = Vec::new();
        escaper.push(b"{\"a\":", &mut out);
        escaper.push(b"1}", &mut out);
        escaper.finish(&mut out);

        assert_eq!(out, b"{\"a\":1}");
        assert_eq!(escape.calls(), vec![EscapeCall::JsonEscaper]);
    }
}
