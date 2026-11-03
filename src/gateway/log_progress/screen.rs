//! Keeps the one-line progress display on the last line of stderr while logs
//! and messages are written above it.

use std::io::{self, Write};

/// stderr, with the one-line progress display drawn on it, if any. The
/// display is drawn again in place with a carriage return and spaces,
/// without the escape sequences that a terminal may not take.
pub(super) struct Screen {
    stderr: Box<dyn Write + Send>,
    /// The text of the one-line progress display on screen.
    progress: Option<String>,
}

impl Screen {
    pub(super) fn new(stderr: Box<dyn Write + Send>) -> Self {
        Self {
            stderr,
            progress: None,
        }
    }

    /// Writes the bytes of logs, messages, or lines of progress, which end
    /// with a line break, with the one-line progress display cleared before
    /// and drawn again after them.
    pub(super) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(progress) = self.progress.take() else {
            self.stderr.write_all(bytes)?;
            return self.stderr.flush();
        };
        let mut out = cleared(&progress).into_bytes();
        out.extend_from_slice(bytes);
        out.push(b'\r');
        out.extend_from_slice(progress.as_bytes());
        self.progress = Some(progress);
        self.stderr.write_all(&out)?;
        self.stderr.flush()
    }

    /// Draws the one-line progress display with `text`, over the one on
    /// screen if there is one. Writes nothing if the one on screen has the
    /// same text.
    pub(super) fn draw_progress(&mut self, text: String) -> io::Result<()> {
        if self.progress.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        let left_over = self
            .progress
            .as_deref()
            .map_or(0, |shown| width(shown).saturating_sub(width(&text)));
        let out = format!("\r{text}{}", " ".repeat(left_over));
        self.progress = Some(text);
        self.stderr.write_all(out.as_bytes())?;
        self.stderr.flush()
    }

    /// Clears the one-line progress display and stops drawing it.
    pub(super) fn clear_progress(&mut self) -> io::Result<()> {
        let Some(progress) = self.progress.take() else {
            return Ok(());
        };
        self.stderr.write_all(cleared(&progress).as_bytes())?;
        self.stderr.flush()
    }
}

/// What clears `shown` and leaves the cursor at the start of its line.
fn cleared(shown: &str) -> String {
    format!("\r{}\r", " ".repeat(width(shown)))
}

/// The columns `text` takes, counting each character as one. The one-line
/// progress display holds only characters that take one column.
pub(super) fn width(text: &str) -> usize {
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    fn screen() -> (Screen, CapturedOutput) {
        let output = CapturedOutput::new();
        (Screen::new(Box::new(output.clone())), output)
    }

    #[test]
    fn without_a_display_the_bytes_are_written_as_they_are() {
        let (mut screen, output) = screen();

        screen.write(b"warning\n").unwrap();

        assert_eq!(output.text(), "warning\n");
    }

    #[test]
    fn the_display_is_drawn_from_the_start_of_the_line() {
        let (mut screen, output) = screen();

        screen.draw_progress("3/10".to_owned()).unwrap();

        assert_eq!(output.text(), "\r3/10");
    }

    #[test]
    fn a_shorter_display_covers_the_rest_of_the_longer_one_with_spaces() {
        let (mut screen, output) = screen();

        screen.draw_progress("9/10".to_owned()).unwrap();
        screen.draw_progress("10/10".to_owned()).unwrap();
        screen.draw_progress("done".to_owned()).unwrap();

        assert_eq!(output.text(), "\r9/10\r10/10\rdone ");
    }

    #[test]
    fn the_same_display_is_not_drawn_again() {
        let (mut screen, output) = screen();

        screen.draw_progress("3/10".to_owned()).unwrap();
        screen.draw_progress("3/10".to_owned()).unwrap();

        assert_eq!(output.text(), "\r3/10");
    }

    #[test]
    fn a_log_clears_the_display_and_the_display_is_drawn_again_below_it() {
        let (mut screen, output) = screen();
        screen.draw_progress("3/10".to_owned()).unwrap();

        screen.write(b"warning\n").unwrap();

        assert_eq!(output.text(), "\r3/10\r    \rwarning\n\r3/10");
    }

    #[test]
    fn a_cleared_display_is_not_drawn_again() {
        let (mut screen, output) = screen();
        screen.draw_progress("3/10".to_owned()).unwrap();

        screen.clear_progress().unwrap();
        screen.write(b"done\n").unwrap();

        assert_eq!(output.text(), "\r3/10\r    \rdone\n");
    }

    #[test]
    fn clearing_without_a_display_writes_nothing() {
        let (mut screen, output) = screen();

        screen.clear_progress().unwrap();

        assert_eq!(output.text(), "");
    }

    #[test]
    fn the_width_of_the_display_is_counted_in_characters() {
        let (mut screen, output) = screen();
        screen.draw_progress("…".to_owned()).unwrap();

        screen.clear_progress().unwrap();

        assert_eq!(output.text(), "\r…\r \r");
    }
}
