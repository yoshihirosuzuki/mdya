//! Walks the central directory, then checks the local file headers and the
//! ranges of compressed data that it points to.

use super::end_record::{Directory, find_directory};
use super::header::{HEADER_SIZE, Header, read_header};
use super::source::{Source, u16_at};
use crate::types::BoxError;

const LOCAL_HEADER_SIZE: u64 = 30;
const LOCAL_SIGNATURE: &[u8] = b"PK\x03\x04";
/// 4.3.13: the signature and the size field of a digital signature.
const SIGNATURE_LEADING: u64 = 6;
const DIGITAL_SIGNATURE: &[u8] = b"PK\x05\x05";

/// Where an entry's header and compressed data are.
#[derive(Clone, Copy)]
pub(super) struct Located {
    pub(super) header: u64,
    pub(super) data_start: u64,
}

/// What the walk keeps of each entry, kept for every entry because the check
/// for overlapping data needs them all.
struct Extent {
    header: u64,
    local_offset: u64,
    compressed: u64,
}

/// A walk through the central directory, one header at a time.
pub(super) struct Walk {
    directory: Directory,
    position: u64,
    read: u64,
    extents: Vec<Extent>,
}

impl Walk {
    pub(super) fn start(source: &mut Source) -> Result<Self, BoxError> {
        let directory = find_directory(source)?;
        // The entry count can be false; the directory's size cannot exceed the
        // file, and each header takes at least `HEADER_SIZE` bytes of it.
        let most_entries = (directory.end - directory.start) / HEADER_SIZE;
        Ok(Self {
            directory,
            position: directory.start,
            read: 0,
            extents: Vec::with_capacity(directory.entries.min(most_entries) as usize),
        })
    }

    /// The next header, or `None` once the entry count has been read.
    pub(super) fn next_header(&mut self, source: &mut Source) -> Result<Option<Header>, BoxError> {
        if self.read == self.directory.entries {
            return Ok(None);
        }
        let header = read_header(source, self.position, self.directory.end)?;
        self.extents.push(Extent {
            header: self.position,
            local_offset: header.local_offset,
            compressed: header.compressed,
        });
        self.position = header.next;
        self.read += 1;
        Ok(Some(header))
    }

    /// The index of the header that `next_header` returned last.
    pub(super) fn last_index(&self) -> u64 {
        self.read - 1
    }

    pub(super) fn directory_end(&self) -> u64 {
        self.directory.end
    }

    /// Checks what can only be checked once every header has been read.
    pub(super) fn finish(self, source: &mut Source) -> Result<Vec<Located>, BoxError> {
        check_remainder(source, self.position, self.directory.end)?;
        let data_starts = locate_data(source, &self.extents, self.directory.start)?;
        Ok(self
            .extents
            .iter()
            .zip(data_starts)
            .map(|(extent, data_start)| Located {
                header: extent.header,
                data_start,
            })
            .collect())
    }
}

/// After the last header there is nothing, or one digital signature. Anything
/// else means that the entry count is too low and entries would be missed.
fn check_remainder(source: &mut Source, position: u64, end: u64) -> Result<(), BoxError> {
    if position == end {
        return Ok(());
    }
    let mut leading = [0; SIGNATURE_LEADING as usize];
    if position + SIGNATURE_LEADING > end {
        return Err("the central directory has bytes after its headers".into());
    }
    source.read_exact_at(position, &mut leading)?;
    let signature_end = position + SIGNATURE_LEADING + u64::from(u16_at(&leading, 4));
    if !leading.starts_with(DIGITAL_SIGNATURE) || signature_end != end {
        return Err("the central directory has bytes after its headers".into());
    }
    Ok(())
}

/// Where each entry's data starts, in the order of `extents`. The local headers
/// are read in file order, so that a directory listing them out of order does
/// not make the buffer read again for each one.
fn locate_data(
    source: &mut Source,
    extents: &[Extent],
    directory_start: u64,
) -> Result<Vec<u64>, BoxError> {
    let mut file_order: Vec<usize> = (0..extents.len()).collect();
    file_order.sort_unstable_by_key(|&index| extents[index].local_offset);
    let mut data_starts = vec![0; extents.len()];
    for index in file_order {
        data_starts[index] = data_start(source, &extents[index], directory_start)?;
    }
    check_no_overlap(extents, &data_starts)?;
    Ok(data_starts)
}

/// Only the local header's signature and lengths are used: with a data
/// descriptor its CRC-32 and sizes are 0, and the directory decides the rest.
fn data_start(source: &mut Source, extent: &Extent, directory_start: u64) -> Result<u64, BoxError> {
    let Some(after_header) = extent
        .local_offset
        .checked_add(LOCAL_HEADER_SIZE)
        .filter(|end| *end <= directory_start)
    else {
        return Err("a local file header is not before the central directory".into());
    };
    let mut header = [0; LOCAL_HEADER_SIZE as usize];
    source.read_exact_at(extent.local_offset, &mut header)?;
    if !header.starts_with(LOCAL_SIGNATURE) {
        return Err("a local file header has the wrong signature".into());
    }
    let start = after_header + u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
    let ends_in_time = start
        .checked_add(extent.compressed)
        .is_some_and(|end| end <= directory_start);
    if !ends_in_time {
        return Err("an entry's data runs into the central directory".into());
    }
    Ok(start)
}

/// Overlapping entries would let a small file hold any number of entries that
/// inflate to a lot of data.
fn check_no_overlap(extents: &[Extent], data_starts: &[u64]) -> Result<(), BoxError> {
    let mut ranges: Vec<(u64, u64)> = extents
        .iter()
        .zip(data_starts)
        .filter(|(extent, _)| extent.compressed > 0)
        .map(|(extent, start)| (*start, start + extent.compressed))
        .collect();
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err("the data of two entries overlaps".into());
    }
    Ok(())
}
