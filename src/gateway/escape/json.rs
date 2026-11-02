//! Escapes U+007F, U+0080 to U+009F, and the characters that change the
//! direction of text (U+202A to U+202E and U+2066 to U+2069) in JSON that
//! has already been written out, as `\u007f` and so on. JSON's own escaping
//! covers only U+0000 to U+001F, and RFC 8259 lets any character be escaped,
//! so the JSON reads the same.

use std::io::{self, Write};
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::AsyncWrite;

use super::JsonEscape;

/// The first byte of U+0080 to U+009F in UTF-8; the second is 0x80 to 0x9F.
const C1_LEAD: u8 = 0xC2;
/// The first byte of the characters that change the direction of text in
/// UTF-8: U+202A to U+202E are 0xE2 0x80 0xAA to 0xAE, and U+2066 to U+2069
/// are 0xE2 0x81 0xA6 to 0xA9.
const DIRECTION_LEAD: u8 = 0xE2;
const DELETE: u8 = 0x7F;

/// Escapes JSON that arrives in pieces. Keeps the bytes at the end of a piece
/// that may begin a character to escape, until the next piece shows whether
/// they do.
#[derive(Default)]
pub(super) struct JsonEscaper {
    held: Held,
}

/// The bytes kept from the end of the last piece.
#[derive(Clone, Copy, Default)]
enum Held {
    #[default]
    Nothing,
    C1Lead,
    DirectionLead,
    /// `DIRECTION_LEAD` and the second byte, 0x80 or 0x81.
    DirectionLeadAnd(u8),
}

impl Held {
    /// Appends the held bytes, unchanged.
    fn push_to(self, out: &mut Vec<u8>) {
        match self {
            Self::Nothing => {}
            Self::C1Lead => out.push(C1_LEAD),
            Self::DirectionLead => out.push(DIRECTION_LEAD),
            Self::DirectionLeadAnd(second) => out.extend_from_slice(&[DIRECTION_LEAD, second]),
        }
    }
}

impl JsonEscaper {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Appends `input`, escaped, to `out`.
    fn push_bytes(&mut self, input: &[u8], out: &mut Vec<u8>) {
        out.reserve(input.len());
        for &byte in input {
            self.push_byte(byte, out);
        }
    }

    /// Appends the held bytes, if any, unchanged.
    fn release(&mut self, out: &mut Vec<u8>) {
        mem::take(&mut self.held).push_to(out);
    }

    fn push_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match (mem::take(&mut self.held), byte) {
            // After `C1_LEAD`, 0x80 to 0x9F is U+0080 to U+009F: the code
            // point equals the second byte.
            (Held::C1Lead, 0x80..=0x9F) => push_json_escape(u32::from(byte), out),
            // After `DIRECTION_LEAD`, 0x80 followed by 0xAA to 0xAE is
            // U+202A to U+202E, and 0x81 followed by 0xA6 to 0xA9 is U+2066
            // to U+2069.
            (Held::DirectionLead, 0x80 | 0x81) => self.held = Held::DirectionLeadAnd(byte),
            (Held::DirectionLeadAnd(second @ 0x80), 0xAA..=0xAE)
            | (Held::DirectionLeadAnd(second @ 0x81), 0xA6..=0xA9) => {
                push_json_escape(direction_code_point(second, byte), out);
            }
            (held, _) => {
                // What is held, if anything, does not begin a character to
                // escape, but `byte` may.
                held.push_to(out);
                self.push_first_byte(byte, out);
            }
        }
    }

    /// Looks at a byte with nothing held before it.
    fn push_first_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match byte {
            DELETE => push_json_escape(u32::from(DELETE), out),
            C1_LEAD => self.held = Held::C1Lead,
            DIRECTION_LEAD => self.held = Held::DirectionLead,
            _ => out.push(byte),
        }
    }
}

/// The code point of the character that is `DIRECTION_LEAD`, `second`, and
/// `third` in UTF-8.
fn direction_code_point(second: u8, third: u8) -> u32 {
    (u32::from(DIRECTION_LEAD & 0x0F) << 12)
        | (u32::from(second & 0x3F) << 6)
        | u32::from(third & 0x3F)
}

fn push_json_escape(code_point: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("\\u{code_point:04x}").as_bytes());
}

impl JsonEscape for JsonEscaper {
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        self.push_bytes(input, out);
    }

    fn finish(mut self: Box<Self>, out: &mut Vec<u8>) {
        self.release(out);
    }
}

/// Wraps a writer that JSON is written to. Accepts every write into its own
/// buffer, escaped, and writes the buffer on to the inner writer until it
/// has taken everything or fails. Bytes left by a failure are written first
/// by the next `write` or `flush`; if the inner writer fails then, that call
/// returns the error, and a `write` takes none of its bytes.
pub(super) struct JsonEscapeWriter<W: Write> {
    inner: W,
    escaper: JsonEscaper,
    buffer: Vec<u8>,
    /// How much of `buffer` the inner writer has taken.
    written: usize,
    /// Set while the inner writer's `write` runs, so that it stays set if
    /// that panics and `drop` does not call it again.
    panicked: bool,
}

impl<W: Write> JsonEscapeWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            escaper: JsonEscaper::new(),
            buffer: Vec::new(),
            written: 0,
            panicked: false,
        }
    }

    /// Writes the buffer on to the inner writer until it has taken
    /// everything or fails.
    fn drain(&mut self) -> io::Result<()> {
        while self.written < self.buffer.len() {
            self.panicked = true;
            let result = self.inner.write(&self.buffer[self.written..]);
            self.panicked = false;
            match result {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(written) => self.written += written,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        self.buffer.clear();
        self.written = 0;
        Ok(())
    }
}

impl<W: Write> Write for JsonEscapeWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // `Write::write` must not take any of `buf` when it fails, so earlier
        // bytes that cannot be written fail the call before `buf` is taken.
        self.drain()?;
        self.escaper.push_bytes(buf, &mut self.buffer);
        // `buf` is taken now. If the inner writer fails, the next `write` or
        // `flush` tries the rest again and returns the error if it fails again.
        let _ = self.drain();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.escaper.release(&mut self.buffer);
        self.drain()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for JsonEscapeWriter<W> {
    /// Writes what is left and drops an error with the writer, unless the
    /// inner writer panicked, as `BufWriter` does.
    fn drop(&mut self) {
        if self.panicked {
            return;
        }
        self.escaper.release(&mut self.buffer);
        let _ = self.drain();
    }
}

/// The asynchronous counterpart of [`JsonEscapeWriter`]. It writes its buffer
/// on only at the next write, flush, or shutdown, so the caller flushes after
/// each whole JSON value.
pub(super) struct AsyncJsonEscapeWriter<W: AsyncWrite + Unpin> {
    inner: W,
    escaper: JsonEscaper,
    buffer: Vec<u8>,
    /// How much of `buffer` the inner writer has taken.
    written: usize,
}

impl<W: AsyncWrite + Unpin> AsyncJsonEscapeWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            escaper: JsonEscaper::new(),
            buffer: Vec::new(),
            written: 0,
        }
    }

    /// Writes the buffer on to the inner writer.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.buffer.len() {
            let pending = &self.buffer[self.written..];
            let written = ready!(Pin::new(&mut self.inner).poll_write(cx, pending))?;
            if written == 0 {
                return Poll::Ready(Err(io::Error::from(io::ErrorKind::WriteZero)));
            }
            self.written += written;
        }
        self.buffer.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }
}

/// Accepts each write into the buffer, escaped, once the earlier bytes are
/// written; `poll_flush` and `poll_shutdown` write the buffer on.
impl<W: AsyncWrite + Unpin> AsyncWrite for AsyncJsonEscapeWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        this.escaper.push_bytes(buf, &mut this.buffer);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.escaper.release(&mut this.buffer);
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.escaper.release(&mut this.buffer);
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    fn escape_all(pieces: &[&[u8]]) -> Vec<u8> {
        let mut escaper = JsonEscaper::new();
        let mut out = Vec::new();
        for piece in pieces {
            escaper.push_bytes(piece, &mut out);
        }
        escaper.release(&mut out);
        out
    }

    #[test]
    fn delete_and_c1_controls_become_json_escapes() {
        let json = "[\"\u{7f}\u{80}\u{9b}\u{9f}\"]";

        assert_eq!(
            escape_all(&[json.as_bytes()]),
            br#"["\u007f\u0080\u009b\u009f"]"#
        );
    }

    #[test]
    fn other_characters_stay_including_the_ones_that_share_the_lead_byte() {
        // U+00A0 and U+00BF are 0xC2 0xA0 and 0xC2 0xBF: the same lead byte as
        // the C1 controls, but not controls.
        let json = "\"a\u{a0}\u{bf}ä日本\\u001b\"";

        assert_eq!(escape_all(&[json.as_bytes()]), json.as_bytes());
    }

    #[test]
    fn characters_that_change_the_direction_of_text_become_json_escapes() {
        let json = "[\"\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}\"]";

        assert_eq!(
            escape_all(&[json.as_bytes()]),
            br#"["\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069"]"#
        );
    }

    #[test]
    fn other_characters_near_the_direction_characters_stay() {
        // U+061C, U+200E, and U+200F also have the Unicode property
        // Bidi_Control. U+061C is 0xD8 0x9C. Every other character in the
        // string is three bytes starting with 0xE2, like the characters that
        // change the direction of text. U+200E, U+200F, U+2014, U+2026,
        // U+2029, and U+202F start with 0xE2 0x80, U+2060, U+2065, and
        // U+206A with 0xE2 0x81, and U+20AC with 0xE2 0x82.
        let json = "\"\u{61c}\u{200e}\u{200f}\u{2014}\u{2026}\u{2029}\u{202f}\u{2060}\u{2065}\u{206a}\u{20ac}\"";

        assert_eq!(escape_all(&[json.as_bytes()]), json.as_bytes());
    }

    #[test]
    fn a_direction_character_split_anywhere_is_escaped() {
        for (character, escaped) in [('\u{202e}', br#""\u202e""#), ('\u{2066}', br#""\u2066""#)] {
            let json = format!("\"{character}\"");
            let bytes = json.as_bytes();

            assert_eq!(escape_all(&[&bytes[..2], &bytes[2..]]), escaped);
            assert_eq!(escape_all(&[&bytes[..3], &bytes[3..]]), escaped);
            assert_eq!(
                escape_all(&[&bytes[..2], &bytes[2..3], &bytes[3..]]),
                escaped
            );
        }
    }

    #[test]
    fn bytes_that_break_off_are_passed_on_and_the_next_byte_is_looked_at_again() {
        // Not valid UTF-8, but no byte may be lost, and a character to escape
        // right after the bytes that broke off is still escaped.
        // The input, the bytes passed on unchanged, and the escape after them.
        let cases: [(&[u8], &[u8], &[u8]); 7] = [
            (
                &[DIRECTION_LEAD, 0x80, C1_LEAD, 0x80],
                &[DIRECTION_LEAD, 0x80],
                br"\u0080",
            ),
            (
                &[C1_LEAD, DIRECTION_LEAD, 0x80, 0xAE],
                &[C1_LEAD],
                br"\u202e",
            ),
            (
                &[DIRECTION_LEAD, 0x80, DIRECTION_LEAD, 0x80, 0xAE],
                &[DIRECTION_LEAD, 0x80],
                br"\u202e",
            ),
            (
                &[DIRECTION_LEAD, 0x81, DELETE],
                &[DIRECTION_LEAD, 0x81],
                br"\u007f",
            ),
            (
                &[DIRECTION_LEAD, DIRECTION_LEAD, 0x81, 0xA6],
                &[DIRECTION_LEAD],
                br"\u2066",
            ),
            (
                &[DIRECTION_LEAD, C1_LEAD, 0x80],
                &[DIRECTION_LEAD],
                br"\u0080",
            ),
            (&[DIRECTION_LEAD, DELETE], &[DIRECTION_LEAD], br"\u007f"),
        ];

        for (input, passed_on, escaped) in cases {
            assert_eq!(
                escape_all(&[input]),
                [passed_on, escaped].concat(),
                "{input:02x?}"
            );
        }
    }

    #[test]
    fn direction_bytes_held_at_the_end_are_released_unchanged() {
        assert_eq!(
            escape_all(&[b"\"a", &[DIRECTION_LEAD]]),
            [b'"', b'a', DIRECTION_LEAD]
        );
        assert_eq!(
            escape_all(&[&[DIRECTION_LEAD, 0x80]]),
            [DIRECTION_LEAD, 0x80]
        );
        assert_eq!(
            escape_all(&[&[DIRECTION_LEAD, 0x81]]),
            [DIRECTION_LEAD, 0x81]
        );
    }

    #[test]
    fn a_character_split_between_pieces_is_escaped() {
        let bytes = "\"\u{9b}\"".as_bytes();
        let (first, second) = bytes.split_at(2);

        assert_eq!(escape_all(&[first, second]), br#""\u009b""#);
    }

    #[test]
    fn a_lead_byte_followed_by_another_lead_byte_is_passed_on() {
        // Not valid UTF-8, but the first lead byte must not be lost.
        assert_eq!(
            escape_all(&[&[C1_LEAD, C1_LEAD, 0x80]]),
            [&[C1_LEAD][..], br"\u0080"].concat()
        );
    }

    #[test]
    fn a_non_control_character_split_after_the_lead_byte_stays() {
        let pieces: [&[u8]; 2] = [&[b'"', C1_LEAD], &[0xA0, b'"']];

        assert_eq!(escape_all(&pieces), "\"\u{a0}\"".as_bytes());
    }

    #[test]
    fn delete_after_a_lead_byte_is_escaped_after_the_lead_byte() {
        assert_eq!(
            escape_all(&[&[C1_LEAD, DELETE]]),
            [&[C1_LEAD][..], br"\u007f"].concat()
        );
    }

    #[test]
    fn a_lead_byte_held_at_the_end_is_released_unchanged() {
        assert_eq!(escape_all(&[b"\"a", &[C1_LEAD]]), [b'"', b'a', C1_LEAD]);
    }

    #[test]
    fn the_writer_escapes_what_is_written_and_flushes_through() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());

        writer.write_all("{\"k\":\"\u{9b}\"}".as_bytes()).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), br#"{"k":"\u009b"}"#);
    }

    #[test]
    fn the_writer_handles_a_character_split_between_writes() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());
        let bytes = "\"\u{80}\"".as_bytes();

        writer.write_all(&bytes[..2]).unwrap();
        writer.write_all(&bytes[2..]).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), br#""\u0080""#);
    }

    #[test]
    fn the_writer_handles_a_direction_character_split_into_three_writes() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());

        writer.write_all(&[b'"', DIRECTION_LEAD]).unwrap();
        writer.write_all(&[0x80]).unwrap();
        writer.write_all(&[0xAE, b'"']).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), br#""\u202e""#);
    }

    #[test]
    fn flush_writes_two_held_bytes_unchanged() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());

        writer.write_all(&[b'"', DIRECTION_LEAD, 0x81]).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), [b'"', DIRECTION_LEAD, 0x81]);
    }

    #[test]
    fn flush_writes_a_held_lead_byte_unchanged() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());

        writer.write_all(&[b'"', C1_LEAD]).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), [b'"', C1_LEAD]);
    }

    #[test]
    fn dropping_the_writer_writes_what_is_left() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(output.clone());

        writer.write_all(&[b'"', C1_LEAD]).unwrap();
        drop(writer);

        assert_eq!(output.bytes(), [b'"', C1_LEAD]);
    }

    /// Takes one byte per write, so that the writer has to keep the rest.
    struct OneByteAtATime(CapturedOutput);

    impl Write for OneByteAtATime {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match buf.first() {
                Some(byte) => self.0.write(&[*byte]),
                None => Ok(0),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_writer_keeps_what_the_inner_writer_did_not_take() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(OneByteAtATime(output.clone()));

        writer.write_all("\"\u{7f}\"".as_bytes()).unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), br#""\u007f""#);
    }

    /// Fails while `failing` is set, then takes everything.
    struct FailsWhileSet {
        output: CapturedOutput,
        failing: Arc<Mutex<bool>>,
    }

    impl Write for FailsWhileSet {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if *self.failing.lock().unwrap() {
                return Err(io::Error::other("broken"));
            }
            self.output.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_inner_error_comes_back_from_the_next_call_and_the_bytes_are_kept() {
        let output = CapturedOutput::new();
        let failing = Arc::new(Mutex::new(true));
        let mut writer = JsonEscapeWriter::new(FailsWhileSet {
            output: output.clone(),
            failing: Arc::clone(&failing),
        });

        let accepted = writer.write(b"{}").unwrap();
        let flush_error = writer.flush().unwrap_err();
        *failing.lock().unwrap() = false;
        writer.flush().unwrap();

        assert_eq!(accepted, 2);
        assert_eq!(flush_error.to_string(), "broken");
        assert_eq!(output.bytes(), b"{}");
    }

    #[test]
    fn a_write_is_not_accepted_while_earlier_bytes_cannot_be_written() {
        let output = CapturedOutput::new();
        let failing = Arc::new(Mutex::new(true));
        let mut writer = JsonEscapeWriter::new(FailsWhileSet {
            output: output.clone(),
            failing: Arc::clone(&failing),
        });

        writer.write_all(b"[1").unwrap();
        let first_error = writer.write(b",2]").unwrap_err();
        let second_error = writer.write(b",2]").unwrap_err();
        *failing.lock().unwrap() = false;
        writer.write_all(b",2]").unwrap();
        writer.flush().unwrap();

        assert_eq!(first_error.to_string(), "broken");
        assert_eq!(second_error.to_string(), "broken");
        assert_eq!(output.bytes(), b"[1,2]");
    }

    /// Is interrupted once, then takes everything.
    struct InterruptedOnce {
        output: CapturedOutput,
        interrupted: bool,
    }

    impl Write for InterruptedOnce {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.output.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_writer_retries_an_interrupted_write() {
        let output = CapturedOutput::new();
        let mut writer = JsonEscapeWriter::new(InterruptedOnce {
            output: output.clone(),
            interrupted: false,
        });

        writer.write_all(b"{}").unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), b"{}");
    }

    /// Takes nothing.
    struct TakesNothing;

    impl Write for TakesNothing {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_inner_writer_that_takes_nothing_is_a_write_zero_error() {
        let mut writer = JsonEscapeWriter::new(TakesNothing);

        writer.write_all(b"{}").unwrap();
        let error = writer.flush().unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    }

    /// An asynchronous writer that always fails.
    struct AlwaysFails;

    impl AsyncWrite for AlwaysFails {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::other("broken")))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_async_writer_reports_an_inner_error_on_flush_and_on_the_next_write() {
        let mut writer = AsyncJsonEscapeWriter::new(AlwaysFails);

        writer.write_all(b"{}").await.unwrap();
        let flush_error = writer.flush().await.unwrap_err();
        let write_error = writer.write(b"{}").await.unwrap_err();

        assert_eq!(flush_error.to_string(), "broken");
        assert_eq!(write_error.to_string(), "broken");
    }

    #[tokio::test]
    async fn the_async_writer_escapes_and_flushes_through() {
        let (writer_end, mut reader_end) = tokio::io::duplex(8);
        let mut writer = AsyncJsonEscapeWriter::new(writer_end);
        let json = "{\"k\":\"\u{7f}\u{9b}\"}";
        let bytes = json.as_bytes();

        let write = async {
            writer.write_all(&bytes[..7]).await.unwrap();
            writer.write_all(&bytes[7..]).await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let read = async {
            let mut read = Vec::new();
            reader_end.read_to_end(&mut read).await.unwrap();
            read
        };
        let ((), read) = tokio::join!(write, read);

        assert_eq!(read, br#"{"k":"\u007f\u009b"}"#);
    }

    #[tokio::test]
    async fn the_async_writer_writes_a_held_lead_byte_on_flush() {
        let (writer_end, mut reader_end) = tokio::io::duplex(64);
        let mut writer = AsyncJsonEscapeWriter::new(writer_end);

        writer.write_all(&[b'"', C1_LEAD]).await.unwrap();
        writer.flush().await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        reader_end.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, [b'"', C1_LEAD]);
    }

    /// Counts flushes, and fails them while `fail_flush` is set.
    struct CountsFlushes {
        output: CapturedOutput,
        flushes: Arc<Mutex<u32>>,
        fail_flush: bool,
    }

    impl Write for CountsFlushes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            *self.flushes.lock().unwrap() += 1;
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }

    #[test]
    fn flush_reaches_the_inner_writer_after_the_bytes() {
        let output = CapturedOutput::new();
        let flushes = Arc::new(Mutex::new(0));
        let mut writer = JsonEscapeWriter::new(CountsFlushes {
            output: output.clone(),
            flushes: Arc::clone(&flushes),
            fail_flush: false,
        });

        writer.write_all(b"{}").unwrap();
        writer.flush().unwrap();

        assert_eq!(output.bytes(), b"{}");
        assert_eq!(*flushes.lock().unwrap(), 1);
    }

    #[test]
    fn flush_returns_the_inner_writers_flush_error() {
        let mut writer = JsonEscapeWriter::new(CountsFlushes {
            output: CapturedOutput::new(),
            flushes: Arc::default(),
            fail_flush: true,
        });

        let error = writer.flush().unwrap_err();

        assert_eq!(error.to_string(), "flush failed");
    }

    /// Takes one byte, then fails while `failing` is set.
    struct TakesOneThenFails {
        output: CapturedOutput,
        took_one: bool,
        failing: Arc<Mutex<bool>>,
    }

    impl Write for TakesOneThenFails {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !self.took_one {
                self.took_one = true;
                return self.output.write(&buf[..1]);
            }
            if *self.failing.lock().unwrap() {
                return Err(io::Error::other("broken"));
            }
            self.output.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_that_fails_part_way_resumes_where_it_stopped() {
        let output = CapturedOutput::new();
        let failing = Arc::new(Mutex::new(true));
        let mut writer = JsonEscapeWriter::new(TakesOneThenFails {
            output: output.clone(),
            took_one: false,
            failing: Arc::clone(&failing),
        });

        writer.write_all(b"{}").unwrap();
        let error = writer.flush().unwrap_err();
        let after_error = output.bytes();
        *failing.lock().unwrap() = false;
        writer.flush().unwrap();

        assert_eq!(error.to_string(), "broken");
        assert_eq!(after_error, b"{");
        assert_eq!(output.bytes(), b"{}");
    }

    #[test]
    fn an_inner_error_that_has_cleared_by_the_next_call_is_not_returned() {
        let output = CapturedOutput::new();
        let failing = Arc::new(Mutex::new(true));
        let mut writer = JsonEscapeWriter::new(FailsWhileSet {
            output: output.clone(),
            failing: Arc::clone(&failing),
        });

        writer.write_all(b"{}").unwrap();
        *failing.lock().unwrap() = false;
        writer.flush().unwrap();

        assert_eq!(output.bytes(), b"{}");
    }

    #[test]
    fn dropping_after_an_inner_error_writes_the_rest() {
        let output = CapturedOutput::new();
        let failing = Arc::new(Mutex::new(true));
        let mut writer = JsonEscapeWriter::new(FailsWhileSet {
            output: output.clone(),
            failing: Arc::clone(&failing),
        });

        writer.write_all(b"{}").unwrap();
        *failing.lock().unwrap() = false;
        drop(writer);

        assert_eq!(output.bytes(), b"{}");
    }

    /// Counts its writes and panics in each.
    struct PanicsOnWrite {
        writes: Arc<Mutex<u32>>,
    }

    impl Write for PanicsOnWrite {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            *self.writes.lock().unwrap() += 1;
            panic!("the inner writer panicked");
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn dropping_after_the_inner_writer_panicked_does_not_write_again() {
        let writes = Arc::new(Mutex::new(0));
        let mut writer = JsonEscapeWriter::new(PanicsOnWrite {
            writes: Arc::clone(&writes),
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = writer.write(b"{}");
        }));
        drop(writer);

        assert!(result.is_err());
        assert_eq!(*writes.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn the_async_writer_escapes_a_character_split_between_writes() {
        let (writer_end, mut reader_end) = tokio::io::duplex(64);
        let mut writer = AsyncJsonEscapeWriter::new(writer_end);

        writer.write_all(&[b'"', C1_LEAD]).await.unwrap();
        writer.write_all(&[0x9B, b'"']).await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        reader_end.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, br#""\u009b""#);
    }

    #[tokio::test]
    async fn the_async_writer_escapes_a_direction_character_split_between_writes() {
        let (writer_end, mut reader_end) = tokio::io::duplex(64);
        let mut writer = AsyncJsonEscapeWriter::new(writer_end);

        writer
            .write_all(&[b'"', DIRECTION_LEAD, 0x81])
            .await
            .unwrap();
        writer.write_all(&[0xA6, b'"']).await.unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        reader_end.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, br#""\u2066""#);
    }

    #[tokio::test]
    async fn the_async_writer_writes_two_held_bytes_on_shutdown() {
        let (writer_end, mut reader_end) = tokio::io::duplex(64);
        let mut writer = AsyncJsonEscapeWriter::new(writer_end);

        writer
            .write_all(&[b'"', DIRECTION_LEAD, 0x81])
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut read = Vec::new();
        reader_end.read_to_end(&mut read).await.unwrap();

        assert_eq!(read, [b'"', DIRECTION_LEAD, 0x81]);
    }

    /// An asynchronous writer that takes one byte per call and counts
    /// flushes and shutdowns. It takes nothing while `takes_nothing` is set,
    /// waits while `blocked` is set, and fails flushes while `fail_flush` is
    /// set and shutdowns while `fail_shutdown` is set.
    #[derive(Default)]
    struct AsyncSink {
        bytes: Vec<u8>,
        takes_nothing: bool,
        blocked: bool,
        fail_flush: bool,
        fail_shutdown: bool,
        flushes: u32,
        shutdowns: u32,
    }

    impl AsyncWrite for AsyncSink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let this = self.get_mut();
            if this.blocked {
                return Poll::Pending;
            }
            if this.takes_nothing || buf.is_empty() {
                return Poll::Ready(Ok(0));
            }
            this.bytes.push(buf[0]);
            Poll::Ready(Ok(1))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            this.flushes += 1;
            if this.fail_flush {
                return Poll::Ready(Err(io::Error::other("flush failed")));
            }
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            this.shutdowns += 1;
            if this.fail_shutdown {
                return Poll::Ready(Err(io::Error::other("shutdown failed")));
            }
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_async_writer_keeps_what_the_inner_writer_did_not_take_and_flushes_it() {
        let mut writer = AsyncJsonEscapeWriter::new(AsyncSink::default());

        writer.write_all("\"\u{7f}\"".as_bytes()).await.unwrap();
        writer.flush().await.unwrap();

        assert_eq!(writer.inner.bytes, br#""\u007f""#);
        assert_eq!(writer.inner.flushes, 1);
    }

    #[tokio::test]
    async fn an_async_inner_writer_that_takes_nothing_is_a_write_zero_error() {
        let mut writer = AsyncJsonEscapeWriter::new(AsyncSink {
            takes_nothing: true,
            ..AsyncSink::default()
        });

        writer.write_all(b"{}").await.unwrap();
        let error = writer.flush().await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn the_async_writer_takes_nothing_while_earlier_bytes_wait() {
        let mut writer = AsyncJsonEscapeWriter::new(AsyncSink {
            blocked: true,
            ..AsyncSink::default()
        });
        let mut cx = Context::from_waker(std::task::Waker::noop());

        let first = Pin::new(&mut writer).poll_write(&mut cx, b"[1");
        let second = Pin::new(&mut writer).poll_write(&mut cx, b",2]");
        let buffer_while_waiting = writer.buffer.clone();
        writer.inner.blocked = false;
        let third = Pin::new(&mut writer).poll_write(&mut cx, b",2]");
        let flushed = Pin::new(&mut writer).poll_flush(&mut cx);

        assert!(matches!(first, Poll::Ready(Ok(2))));
        assert!(second.is_pending());
        assert_eq!(buffer_while_waiting, b"[1");
        assert!(matches!(third, Poll::Ready(Ok(3))));
        assert!(matches!(flushed, Poll::Ready(Ok(()))));
        assert_eq!(writer.inner.bytes, b"[1,2]");
    }

    #[tokio::test]
    async fn the_async_writer_returns_the_inner_writers_flush_error() {
        let mut writer = AsyncJsonEscapeWriter::new(AsyncSink {
            fail_flush: true,
            ..AsyncSink::default()
        });

        writer.write_all(b"{}").await.unwrap();
        let error = writer.flush().await.unwrap_err();

        assert_eq!(error.to_string(), "flush failed");
        assert_eq!(writer.inner.bytes, b"{}");
        assert_eq!(writer.inner.flushes, 1);
    }

    #[tokio::test]
    async fn shutdown_writes_a_held_lead_byte_and_returns_the_inner_writers_error() {
        let mut writer = AsyncJsonEscapeWriter::new(AsyncSink {
            fail_shutdown: true,
            ..AsyncSink::default()
        });

        writer.write_all(&[b'"', C1_LEAD]).await.unwrap();
        let error = writer.shutdown().await.unwrap_err();

        assert_eq!(error.to_string(), "shutdown failed");
        assert_eq!(writer.inner.bytes, [b'"', C1_LEAD]);
        assert_eq!(writer.inner.shutdowns, 1);
    }

    #[tokio::test]
    async fn shutdown_returns_an_error_from_writing_the_buffer() {
        let mut writer = AsyncJsonEscapeWriter::new(AlwaysFails);

        writer.write_all(b"{}").await.unwrap();
        let error = writer.shutdown().await.unwrap_err();

        assert_eq!(error.to_string(), "broken");
    }
}
