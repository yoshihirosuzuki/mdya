//! Decompresses a `.tar.gz`, one gzip member after another.

use std::io::{self, BufRead, Read};
use std::mem;

use flate2::bufread::GzDecoder;

/// The decompressed bytes of a series of gzip members. flate2 checks each
/// member's CRC32 and size when the member ends.
pub(super) enum Members<R> {
    Inside(GzDecoder<R>),
    /// The input ended right after a member, or could no longer be read.
    Ended,
}

impl<R: BufRead> Members<R> {
    pub(super) fn new(input: R) -> Self {
        Self::Inside(GzDecoder::new(input))
    }

    /// Reads the current member to its end, so that its CRC32 is checked,
    /// and nothing after it. What follows the last member a tar needs may
    /// not be gzip at all: bsdtar pads its output with NUL bytes.
    pub(super) fn finish_member(&mut self) -> io::Result<()> {
        match self {
            Self::Inside(member) => io::copy(member, &mut io::sink()).map(drop),
            Self::Ended => Ok(()),
        }
    }

    fn start_next_member(&mut self) -> io::Result<()> {
        let Self::Inside(member) = mem::replace(self, Self::Ended) else {
            return Ok(());
        };
        let mut input = member.into_inner();
        if !input.fill_buf()?.is_empty() {
            *self = Self::new(input);
        }
        Ok(())
    }
}

impl<R: BufRead> Read for Members<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Self::Inside(member) = self else {
                return Ok(0);
            };
            let read = member.read(buf)?;
            if read > 0 || buf.is_empty() {
                return Ok(read);
            }
            self.start_next_member()?;
        }
    }
}
