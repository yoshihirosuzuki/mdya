//! Reading a zip file at given positions, and the little-endian fields of its
//! records.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Take};

/// A zip file read through a buffer. Moving within the buffer keeps it, so that
/// going through many small records in file order does not read the same
/// blocks again.
pub(super) struct Source {
    reader: BufReader<File>,
    /// Where the reader is, when that is known.
    position: Option<u64>,
}

impl Source {
    pub(super) fn new(file: File) -> Self {
        Self {
            reader: BufReader::new(file),
            position: None,
        }
    }

    pub(super) fn length(&mut self) -> io::Result<u64> {
        self.position = None;
        self.reader.seek(SeekFrom::End(0))
    }

    pub(super) fn read_exact_at(&mut self, at: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.move_to(at)?;
        self.position = None;
        self.reader.read_exact(buffer)?;
        self.position = Some(at + buffer.len() as u64);
        Ok(())
    }

    pub(super) fn read_at(&mut self, at: u64, length: u64) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; length as usize];
        self.read_exact_at(at, &mut bytes)?;
        Ok(bytes)
    }

    /// The `length` bytes at `at`, for reading through a decoder.
    pub(super) fn section(
        &mut self,
        at: u64,
        length: u64,
    ) -> io::Result<Take<&mut BufReader<File>>> {
        self.move_to(at)?;
        self.position = None;
        Ok((&mut self.reader).take(length))
    }

    fn move_to(&mut self, at: u64) -> io::Result<()> {
        match self.position {
            // Both are positions in the file, so the difference fits an i64.
            Some(current) => self.reader.seek_relative(at.wrapping_sub(current) as i64)?,
            None => {
                self.reader.seek(SeekFrom::Start(at))?;
            }
        }
        self.position = Some(at);
        Ok(())
    }
}

pub(super) fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

pub(super) fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|index| bytes[at + index]))
}

pub(super) fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(std::array::from_fn(|index| bytes[at + index]))
}
