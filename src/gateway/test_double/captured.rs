//! A writer whose bytes a test can read after handing it over: the stdout
//! and stderr of `Io`, or the stdout of `cli`'s streams.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::io::Write;
use std::sync::{Arc, Mutex};

/// Clones share the bytes, so that a test can keep one and hand over
/// another.
#[derive(Clone, Default)]
pub(crate) struct CapturedOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapturedOutput {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }

    /// The bytes as text. Panics when they are not UTF-8.
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.bytes()).unwrap()
    }
}

impl Write for CapturedOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_clone_reads_what_another_wrote() {
        let kept = CapturedOutput::new();
        let mut handed_over: Box<dyn Write + Send> = Box::new(kept.clone());

        handed_over.write_all("結果\n".as_bytes()).unwrap();

        assert_eq!(kept.text(), "結果\n");
    }
}
