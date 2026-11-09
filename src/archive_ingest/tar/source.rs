//! Where the bytes of a tar come from: the file itself, or its gunzipped form.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};

use super::gzip::Members;

/// The bytes of a tar, from its first byte on.
pub(super) trait TarBytes: BufRead + Send {
    /// Moves `count` bytes ahead. Fails if the tar ends before that.
    fn skip(&mut self, count: u64) -> io::Result<()>;

    /// Called once the end-of-archive indicator has been read.
    fn end_of_tar(&mut self) -> io::Result<()>;
}

pub(super) fn cut_short() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the tar ends before the data its headers describe",
    )
}

/// Goes back to the start of `file`, and reads the tar in it.
pub(super) fn open(mut file: File, gzip: bool) -> io::Result<Box<dyn TarBytes>> {
    file.seek(SeekFrom::Start(0))?;
    if gzip {
        return Ok(Box::new(BufReader::new(Members::new(BufReader::new(file)))));
    }
    let len = file.metadata()?.len();
    Ok(Box::new(PlainTar {
        reader: BufReader::new(file),
        len,
    }))
}

struct PlainTar {
    reader: BufReader<File>,
    len: u64,
}

impl Read for PlainTar {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl BufRead for PlainTar {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.reader.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.reader.consume(amount);
    }
}

impl TarBytes for PlainTar {
    /// Seeks rather than reads, so that a listing does not read the whole of
    /// an uncompressed tar.
    fn skip(&mut self, count: u64) -> io::Result<()> {
        let buffered = self.reader.buffer().len();
        if count <= buffered as u64 {
            self.reader.consume(count as usize);
            return Ok(());
        }
        let target = self.reader.stream_position()?.saturating_add(count);
        if target > self.len {
            return Err(cut_short());
        }
        self.reader.seek(SeekFrom::Start(target)).map(drop)
    }

    fn end_of_tar(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TarBytes for BufReader<Members<BufReader<File>>> {
    fn skip(&mut self, count: u64) -> io::Result<()> {
        let skipped = io::copy(&mut self.take(count), &mut io::sink())?;
        if skipped < count {
            return Err(cut_short());
        }
        Ok(())
    }

    fn end_of_tar(&mut self) -> io::Result<()> {
        self.get_mut().finish_member()
    }
}
