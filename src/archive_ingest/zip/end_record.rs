//! Finds the central directory from the end records of a zip file. The section
//! numbers are those of PKWARE's APPNOTE.TXT.

use std::io;

use super::source::{Source, u16_at, u32_at, u64_at};
use crate::types::BoxError;

/// 4.3.16: the fixed part of the end of central directory record.
const END_RECORD_SIZE: usize = 22;
const END_SIGNATURE: &[u8] = b"PK\x05\x06";
/// The end record and the longest comment it can carry.
const SEARCH_RANGE: u64 = END_RECORD_SIZE as u64 + 65_535;
/// 4.3.15: the zip64 end of central directory locator.
const LOCATOR_SIZE: u64 = 20;
const LOCATOR_SIGNATURE: &[u8] = b"PK\x06\x07";
/// 4.3.14: the fixed part of the zip64 end of central directory record.
const ZIP64_RECORD_SIZE: u64 = 56;
const ZIP64_SIGNATURE: &[u8] = b"PK\x06\x06";
/// The record's own signature and size field, which its size does not count.
const ZIP64_RECORD_LEADING: u64 = 12;

/// Where the central directory is and how many entries it says it has.
#[derive(Clone, Copy)]
pub(super) struct Directory {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) entries: u64,
}

/// What an end record says, whether the plain one or the zip64 one.
struct EndFields {
    disk: u32,
    directory_disk: u32,
    entries_on_disk: u64,
    entries: u64,
    directory_size: u64,
    directory_start: u64,
}

impl EndFields {
    /// The directory, if the fields describe one that ends at `record`. A zip
    /// split over several disks cannot be read from one file.
    fn directory_ending_at(&self, record: u64) -> Option<Directory> {
        let whole = self.disk == 0 && self.directory_disk == 0;
        let same_count = self.entries_on_disk == self.entries;
        let ends_at_record = self.directory_start.checked_add(self.directory_size)? == record;
        (whole && same_count && ends_at_record).then_some(Directory {
            start: self.directory_start,
            end: record,
            entries: self.entries,
        })
    }
}

/// Finds the one end record that describes a central directory ending right
/// before it. Taking the last candidate instead would let a comment hold a
/// record for an empty zip, and every document would then be deleted.
pub(super) fn find_directory(source: &mut Source) -> Result<Directory, BoxError> {
    let length = source.length()?;
    let tail_start = length.saturating_sub(SEARCH_RANGE);
    let tail = source.read_at(tail_start, length - tail_start)?;
    let mut found = None;
    for index in candidates(&tail) {
        let position = tail_start + index as u64;
        let Some(directory) = check_candidate(source, &tail[index..], position)? else {
            continue;
        };
        if found.replace(directory).is_some() {
            return Err("more than one end of central directory record is valid".into());
        }
    }
    found.ok_or_else(|| "no valid end of central directory record".into())
}

/// The places in `tail` with a signature whose record and comment fit the file.
fn candidates(tail: &[u8]) -> impl Iterator<Item = usize> + '_ {
    (0..tail.len().saturating_sub(END_RECORD_SIZE - 1)).filter(move |&index| {
        let record = &tail[index..];
        let comment = usize::from(u16_at(record, 20));
        record.starts_with(END_SIGNATURE) && record.len() >= END_RECORD_SIZE + comment
    })
}

/// The directory that the end record at `position` describes, if it is valid.
fn check_candidate(
    source: &mut Source,
    record: &[u8],
    position: u64,
) -> io::Result<Option<Directory>> {
    let plain = plain_fields(record);
    let Some(locator_position) = position.checked_sub(LOCATOR_SIZE) else {
        return Ok(plain.directory_ending_at(position));
    };
    let mut locator = [0; LOCATOR_SIZE as usize];
    source.read_exact_at(locator_position, &mut locator)?;
    if !locator.starts_with(LOCATOR_SIGNATURE) {
        return Ok(plain.directory_ending_at(position));
    }
    zip64_directory(source, &locator, locator_position)
}

fn plain_fields(record: &[u8]) -> EndFields {
    EndFields {
        disk: u32::from(u16_at(record, 4)),
        directory_disk: u32::from(u16_at(record, 6)),
        entries_on_disk: u64::from(u16_at(record, 8)),
        entries: u64::from(u16_at(record, 10)),
        directory_size: u64::from(u32_at(record, 12)),
        directory_start: u64::from(u32_at(record, 16)),
    }
}

/// With a locator the zip64 record decides, even if the plain record's fields
/// are not at their maximum: Info-ZIP writes that form for standard input.
fn zip64_directory(
    source: &mut Source,
    locator: &[u8],
    locator_position: u64,
) -> io::Result<Option<Directory>> {
    let record_position = u64_at(locator, 8);
    let one_disk = u32_at(locator, 4) == 0 && u32_at(locator, 16) <= 1;
    let fits = record_position
        .checked_add(ZIP64_RECORD_SIZE)
        .is_some_and(|end| end <= locator_position);
    if !one_disk || !fits {
        return Ok(None);
    }
    let mut record = [0; ZIP64_RECORD_SIZE as usize];
    source.read_exact_at(record_position, &mut record)?;
    let ends_at_locator = record_position
        .checked_add(ZIP64_RECORD_LEADING)
        .and_then(|after_size| after_size.checked_add(u64_at(&record, 4)))
        == Some(locator_position);
    if !record.starts_with(ZIP64_SIGNATURE) || !ends_at_locator {
        return Ok(None);
    }
    Ok(zip64_fields(&record).directory_ending_at(record_position))
}

fn zip64_fields(record: &[u8]) -> EndFields {
    EndFields {
        disk: u32_at(record, 16),
        directory_disk: u32_at(record, 20),
        entries_on_disk: u64_at(record, 24),
        entries: u64_at(record, 32),
        directory_size: u64_at(record, 40),
        directory_start: u64_at(record, 48),
    }
}
