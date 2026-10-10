//! Decompressing a `.tar.gz` / `.tgz` one gzip member at a time.
//!
//! A gzip file can be several members, each compressed on its own, one
//! after another; `gzip -d` decompresses them all, so the tar is read
//! across member boundaries. Each member's CRC is checked when it ends.
//! Once the tar's end-of-archive marker has been read, the member holding
//! its last byte is read to its end, so its CRC is checked too
//! ([`GzipMembers::finish_member`]), and nothing after that member is read:
//! bsdtar pads its gzip output with NUL bytes, which do not make a member.

use std::io::{self, BufRead, Read};

use flate2::bufread::GzDecoder;

pub(super) struct GzipMembers<R> {
    state: State<R>,
}

enum State<R> {
    /// Decompressing a member; flate2 checks its CRC and size at its end.
    Member(GzDecoder<R>),
    /// A member ended with a matching CRC, and whether another follows is
    /// not known yet. The input stays here, so a read that fails while
    /// looking is tried again by the next read instead of passing for the
    /// end.
    Between(R),
    /// The input ended right after a member.
    Ended,
}

impl<R: BufRead> GzipMembers<R> {
    pub(super) fn new(input: R) -> Self {
        Self {
            state: State::Member(GzDecoder::new(input)),
        }
    }

    /// Read the current member to its end, so its CRC is checked, and
    /// nothing after it.
    pub(super) fn finish_member(&mut self) -> io::Result<()> {
        match &mut self.state {
            State::Member(member) => io::copy(member, &mut io::sink()).map(drop),
            State::Between(_) | State::Ended => Ok(()),
        }
    }
}

impl<R: BufRead> Read for GzipMembers<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match &mut self.state {
                State::Member(member) => {
                    let read = member.read(buf)?;
                    if read > 0 || buf.is_empty() {
                        return Ok(read);
                    }
                    let State::Member(member) = std::mem::replace(&mut self.state, State::Ended)
                    else {
                        unreachable!("matched above");
                    };
                    self.state = State::Between(member.into_inner());
                }
                State::Between(input) => {
                    if input.fill_buf()?.is_empty() {
                        self.state = State::Ended;
                        continue;
                    }
                    let State::Between(input) = std::mem::replace(&mut self.state, State::Ended)
                    else {
                        unreachable!("matched above");
                    };
                    self.state = State::Member(GzDecoder::new(input));
                }
                State::Ended => return Ok(0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).expect("gzip");
        encoder.finish().expect("finish gzip")
    }

    fn read_all(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        GzipMembers::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn members_are_read_one_after_another() {
        let bytes = [gzip(b"first "), gzip(b"second")].concat();

        assert_eq!(read_all(&bytes).expect("read"), b"first second");
    }

    #[test]
    fn a_member_with_a_wrong_crc_fails() {
        let mut bytes = gzip(b"text");
        // The CRC is the 4 bytes before the size at the end.
        let crc = bytes.len() - 8;
        bytes[crc] ^= 0xff;

        assert!(read_all(&bytes).is_err());
    }

    #[test]
    fn a_member_cut_short_fails() {
        let bytes = gzip(&[b'a'; 4096]);

        assert!(read_all(&bytes[..bytes.len() - 4]).is_err());
    }

    #[test]
    fn bytes_after_a_member_that_are_not_a_member_fail_when_read() {
        let bytes = [gzip(b"text"), vec![0; 16]].concat();

        assert!(read_all(&bytes).is_err());
    }

    /// Input that fails once when asked for more at `fail_at`, and hands
    /// out nothing past `fail_at` before that.
    struct FailsOnce<'a> {
        bytes: &'a [u8],
        at: usize,
        fail_at: usize,
        failed: bool,
    }

    impl Read for FailsOnce<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let read = available.len().min(buf.len());
            buf[..read].copy_from_slice(&available[..read]);
            self.consume(read);
            Ok(read)
        }
    }

    impl BufRead for FailsOnce<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.at == self.fail_at && !self.failed {
                self.failed = true;
                return Err(io::Error::other("the input failed once"));
            }
            let end = if self.at < self.fail_at {
                self.fail_at
            } else {
                self.bytes.len()
            };
            Ok(&self.bytes[self.at..end])
        }

        fn consume(&mut self, amount: usize) {
            self.at += amount;
        }
    }

    #[test]
    fn a_read_that_fails_between_members_is_not_taken_for_the_end() {
        let first = gzip(b"first ");
        let bytes = [first.clone(), gzip(b"second")].concat();
        let mut members = GzipMembers::new(FailsOnce {
            bytes: &bytes,
            at: 0,
            fail_at: first.len(),
            failed: false,
        });
        let mut out = Vec::new();

        assert!(members.read_to_end(&mut out).is_err());
        members.read_to_end(&mut out).expect("read on");

        assert_eq!(out, b"first second");
    }

    #[test]
    fn finishing_checks_the_current_member_and_reads_nothing_after_it() {
        let padded = [gzip(b"head tail"), vec![0; 64]].concat();
        let mut input = &padded[..];
        let mut members = GzipMembers::new(&mut input);
        let mut head = [0u8; 4];
        members.read_exact(&mut head).expect("head");

        members.finish_member().expect("finish");

        assert_eq!(&head, b"head");
        assert_eq!(input, &[0u8; 64][..]);

        let mut corrupt = gzip(b"head tail");
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 0xff;
        let mut members = GzipMembers::new(&corrupt[..]);
        members.read_exact(&mut head).expect("head");
        assert!(members.finish_member().is_err());
    }
}
