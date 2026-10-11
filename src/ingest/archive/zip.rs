//! Reading a `.zip` through its central directory.
//!
//! The format is PKWARE's APPNOTE.TXT (6.3.10; section numbers below are
//! its). The `zip` crate is not used for reading: when the end record at
//! the end of a file does not lead to a central directory it can read, the
//! crate searches further back and reads whatever end record it finds
//! there, so a zip cut short that holds a stored zip (Info-ZIP `zip` stores
//! `.zip` files by default) lists as that inner zip, and a crafted file
//! makes the search take time that grows with the square of its size.
//!
//! What the reader accepts, and what makes the archive unreadable:
//!
//! - The end record (4.3.16) is the one candidate that passes the checks
//!   below, among the signatures in the last 65,557 bytes (the record and
//!   its longest comment) whose record and comment fit in the file. No
//!   candidate, or more than one (a fake record hidden in the real one's
//!   comment), makes the archive unreadable: nothing is read again from
//!   another candidate.
//! - A zip64 end locator right before the end record (4.3.15) means the
//!   counts and positions come from the zip64 end record (4.3.14) it points
//!   to, which must end right at the locator. Info-ZIP `zip` writes one
//!   even when the end record's own fields are not at their maximum.
//! - A split zip (4.3.3) cannot be read from one file: any disk number
//!   other than 0, a count for this disk that differs from the total, or
//!   more than one disk.
//! - The central directory ends right before the end record (the zip64 end
//!   record in a zip64 file), as 4.3.6 lays the file out. A zip with other
//!   data in front of it whose positions were not adjusted, and the end
//!   record of a zip stored inside one cut short, fail this.
//! - Each of the declared entries is a central directory header (4.3.12)
//!   inside the directory; what follows the last one is nothing or exactly
//!   one digital signature record (4.3.13), so a count that is too small
//!   does not silently leave entries out. Fields at 0xFFFFFFFF / 0xFFFF are
//!   read from the zip64 extra field (4.5.3), in its order and only those.
//! - Each entry's local header is where the directory says, and its data
//!   ends before the central directory. Entries whose compressed data
//!   overlap make the archive unreadable (see [`check_no_shared_data`]).
//! - Directories (a name ending in `/`) and symbolic links (Unix file type
//!   in the high 16 bits of the external attributes) are not regular files.
//!
//! A document is read by its compressed size, decompressing as it goes up
//! to the document limit; a size or CRC-32 that does not match the
//! directory fails the document. Only stored and deflated documents are
//! read, and not encrypted ones. Both passes run every check above, in
//! case the archive is replaced in between.

use std::io::{self, BufReader, Read, Seek, SeekFrom};

use super::{
    ArchiveDocument, DocumentReadError, PartialListing, RawEntry, invalid, names, read_capped,
};

const END_RECORD: &[u8; 4] = b"PK\x05\x06";
const END_RECORD_LEN: u64 = 22;
const MAX_COMMENT_LEN: u64 = u16::MAX as u64;
const ZIP64_LOCATOR: &[u8; 4] = b"PK\x06\x07";
const ZIP64_LOCATOR_LEN: u64 = 20;
const ZIP64_END_RECORD: &[u8; 4] = b"PK\x06\x06";
/// The zip64 end record up to and including the directory's offset.
const ZIP64_END_RECORD_LEN: u64 = 56;
const CENTRAL_HEADER: &[u8; 4] = b"PK\x01\x02";
const CENTRAL_HEADER_LEN: usize = 46;
const LOCAL_HEADER: &[u8; 4] = b"PK\x03\x04";
const LOCAL_HEADER_LEN: usize = 30;
const DIGITAL_SIGNATURE: &[u8; 4] = b"PK\x05\x05";
const ZIP64_EXTRA_ID: u16 = 0x0001;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;
const ENCRYPTED_FLAG: u16 = 1;

/// List the entries of a zip into `listing`.
pub(super) fn list_zip<R: Read + Seek>(
    reader: BufReader<R>,
    listing: &mut PartialListing,
) -> io::Result<()> {
    let mut zip = Zip::open(reader)?;
    let spans = zip.walk_directory(|position, entry| {
        listing.add(RawEntry {
            order: position as u64,
            position,
            name: &entry.name,
            is_regular_file: entry.is_regular_file,
        })
    })?;
    zip.locate_data(spans).map(drop)
}

/// Read `wanted` out of a zip, checking the whole archive again first.
pub(super) fn read_zip<R: Read + Seek>(
    reader: BufReader<R>,
    wanted: &[ArchiveDocument],
    limit: u64,
    mut deliver: impl FnMut(&ArchiveDocument, Result<Vec<u8>, DocumentReadError>) -> bool,
) -> io::Result<()> {
    let mut zip = Zip::open(reader)?;
    let mut found: Vec<Option<CentralEntry>> = Vec::with_capacity(wanted.len());
    let mut pending = wanted.iter().peekable();
    let spans = zip.walk_directory(|position, entry| {
        while pending.next_if(|d| d.position < position).is_some() {
            found.push(None);
        }
        if pending.next_if(|d| d.position == position).is_some() {
            found.push(Some(entry.clone()));
        }
        Ok(())
    })?;
    let data_starts = zip.locate_data(spans)?;
    found.resize_with(wanted.len(), || None);
    for (document, entry) in wanted.iter().zip(found) {
        let result = match entry {
            Some(entry) if entry.is_regular_file && names(&entry.name, &document.path) => {
                zip.read_entry(&entry, data_starts[document.position], limit)
            }
            _ => Err(DocumentReadError::Changed),
        };
        if !deliver(document, result) {
            break;
        }
    }
    Ok(())
}

/// Where the central directory is, from the end records.
#[derive(Debug, PartialEq, Eq)]
struct Directory {
    start: u64,
    size: u64,
    entries: u64,
}

/// One central directory header, as far as mdya uses it.
#[derive(Clone)]
struct CentralEntry {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    crc: u32,
    compressed_size: u64,
    size: u64,
    local_header: u64,
    is_regular_file: bool,
}

/// Each entry's compressed data: where its local header is, then (once the
/// header is read) where the data starts; its compressed size; and the
/// entry's position in the directory. One list of these, 24 bytes an
/// entry, is all [`Zip::locate_data`] holds for the whole directory.
type Spans = Vec<(u64, u64, usize)>;

struct Zip<R> {
    input: Input<R>,
    directory: Directory,
}

impl<R: Read + Seek> Zip<R> {
    fn open(reader: BufReader<R>) -> io::Result<Self> {
        let mut input = Input::new(reader)?;
        let directory = find_directory(&mut input)?;
        Ok(Self { input, directory })
    }

    /// Read every central directory header in order, handing each to
    /// `visit` with its position, and return each entry's span.
    fn walk_directory(
        &mut self,
        mut visit: impl FnMut(usize, &CentralEntry) -> io::Result<()>,
    ) -> io::Result<Spans> {
        let Directory {
            start,
            size,
            entries,
        } = self.directory;
        self.input.seek(start)?;
        let mut left = size;
        // Sized up front so it does not grow to twice what it holds. The
        // entry count can be forged, but no more headers than this fit in
        // the directory, whose size was checked against the file.
        let fit = size / CENTRAL_HEADER_LEN as u64;
        let mut spans = Vec::with_capacity(usize::try_from(entries.min(fit)).unwrap_or(0));
        let mut position = 0usize;
        while (position as u64) < entries {
            let entry = read_central_header(&mut self.input, &mut left)?;
            spans.push((entry.local_header, entry.compressed_size, position));
            visit(position, &entry)?;
            position += 1;
        }
        check_directory_tail(&mut self.input, left)?;
        Ok(spans)
    }

    /// Read each entry's local header to find where its data starts, check
    /// that the data ends before the central directory and that no two
    /// entries' data overlap, and return the data starts by position. The
    /// headers are visited in file order, so the input only moves forward:
    /// in directory order, a directory that jumps back and forth would make
    /// the input refill its buffer for every entry.
    fn locate_data(&mut self, mut spans: Spans) -> io::Result<Vec<u64>> {
        let directory_start = self.directory.start;
        spans.sort_unstable_by_key(|&(local_header, ..)| local_header);
        for (start, compressed_size, _) in &mut spans {
            let local_header = *start;
            let mut header = [0u8; LOCAL_HEADER_LEN];
            if local_header
                .checked_add(LOCAL_HEADER_LEN as u64)
                .is_none_or(|end| end > directory_start)
            {
                return Err(local_header_damaged());
            }
            self.input.seek(local_header)?;
            self.input.read_exact(&mut header)?;
            if &header[..4] != LOCAL_HEADER {
                return Err(local_header_damaged());
            }
            let data_start = local_header
                + LOCAL_HEADER_LEN as u64
                + u64::from(le16(&header, 26))
                + u64::from(le16(&header, 28));
            if data_start
                .checked_add(*compressed_size)
                .is_none_or(|end| end > directory_start)
            {
                return Err(invalid(
                    "a zip entry's data runs into the central directory",
                ));
            }
            *start = data_start;
        }
        check_no_shared_data(&mut spans)?;
        spans.sort_unstable_by_key(|&(.., position)| position);
        Ok(spans.into_iter().map(|(start, ..)| start).collect())
    }

    fn read_entry(
        &mut self,
        entry: &CentralEntry,
        data_start: u64,
        limit: u64,
    ) -> Result<Vec<u8>, DocumentReadError> {
        if entry.flags & ENCRYPTED_FLAG != 0 {
            return Err(DocumentReadError::Read(
                "the zip entry is encrypted".to_string(),
            ));
        }
        let read = |e: io::Error| DocumentReadError::Read(e.to_string());
        let data = self
            .input
            .data(data_start, entry.compressed_size)
            .map_err(read)?;
        let bytes = match entry.method {
            STORED => read_capped(data, limit)?,
            DEFLATED => read_capped(flate2::bufread::DeflateDecoder::new(data), limit)?,
            _ => {
                return Err(DocumentReadError::Read(
                    "the zip entry is compressed with a method mdya does not read".to_string(),
                ));
            }
        };
        if bytes.len() as u64 != entry.size {
            return Err(DocumentReadError::Read(
                "the zip entry's size does not match its central directory".to_string(),
            ));
        }
        let mut crc = flate2::Crc::new();
        crc.update(&bytes);
        if crc.sum() != entry.crc {
            return Err(DocumentReadError::Read(
                "the zip entry's CRC does not match its central directory".to_string(),
            ));
        }
        Ok(bytes)
    }
}

/// A buffered reader that knows where it is, so moving forward a little
/// keeps its buffer.
struct Input<R> {
    reader: BufReader<R>,
    len: u64,
    /// `None` after a read that may have left the position unknown.
    position: Option<u64>,
}

impl<R: Read + Seek> Input<R> {
    fn new(mut reader: BufReader<R>) -> io::Result<Self> {
        let len = reader.seek(SeekFrom::End(0))?;
        Ok(Self {
            reader,
            len,
            position: None,
        })
    }

    fn seek(&mut self, to: u64) -> io::Result<()> {
        let relative = self
            .position
            .and_then(|at| i64::try_from(i128::from(to) - i128::from(at)).ok());
        match relative {
            Some(offset) => self.reader.seek_relative(offset)?,
            None => {
                self.reader.seek(SeekFrom::Start(to))?;
            }
        }
        self.position = Some(to);
        Ok(())
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let result = self.reader.read_exact(buf);
        self.position = match (&result, self.position) {
            (Ok(()), Some(at)) => Some(at + buf.len() as u64),
            _ => None,
        };
        result
    }

    /// Step over `len` bytes after a read that left the position known.
    fn skip(&mut self, len: u64) -> io::Result<()> {
        let at = self
            .position
            .and_then(|at| at.checked_add(len))
            .ok_or_else(|| invalid("a zip central directory header is damaged"))?;
        self.seek(at)
    }

    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> io::Result<()> {
        self.seek(at)?;
        self.read_exact(buf)
    }

    /// `len` bytes from `at`, for reading an entry's data.
    fn data(&mut self, at: u64, len: u64) -> io::Result<io::Take<&mut BufReader<R>>> {
        self.seek(at)?;
        self.position = None;
        Ok((&mut self.reader).take(len))
    }
}

/// Find the one end record that passes the checks among the candidates at
/// the end of the file.
fn find_directory<R: Read + Seek>(input: &mut Input<R>) -> io::Result<Directory> {
    let window_len = input.len.min(END_RECORD_LEN + MAX_COMMENT_LEN);
    let window_start = input.len - window_len;
    let mut window = vec![0; window_len as usize];
    input.read_at(window_start, &mut window)?;
    let mut found = None;
    for at in (0..window.len().saturating_sub(END_RECORD_LEN as usize - 1)).rev() {
        if &window[at..at + 4] != END_RECORD {
            continue;
        }
        let record = &window[at..at + END_RECORD_LEN as usize];
        // The record and its comment must fit in the file.
        if at as u64 + END_RECORD_LEN + u64::from(le16(record, 20)) > window_len {
            continue;
        }
        if let Some(directory) = check_candidate(input, record, window_start + at as u64)? {
            if found.is_some() {
                return Err(invalid(
                    "the zip has more than one end-of-central-directory record, so which one to use is not settled",
                ));
            }
            found = Some(directory);
        }
    }
    found.ok_or_else(|| {
        invalid("the zip has no usable end-of-central-directory record, so it may be cut short")
    })
}

/// The directory a candidate end record describes, if it passes the
/// checks in the module docs.
fn check_candidate<R: Read + Seek>(
    input: &mut Input<R>,
    record: &[u8],
    record_at: u64,
) -> io::Result<Option<Directory>> {
    let mut locator = [0u8; ZIP64_LOCATOR_LEN as usize];
    let has_locator = record_at >= ZIP64_LOCATOR_LEN && {
        input.read_at(record_at - ZIP64_LOCATOR_LEN, &mut locator)?;
        &locator[..4] == ZIP64_LOCATOR
    };
    if !has_locator {
        let directory = Directory {
            start: u64::from(le32(record, 16)),
            size: u64::from(le32(record, 12)),
            entries: u64::from(le16(record, 10)),
        };
        let one_disk =
            le16(record, 4) == 0 && le16(record, 6) == 0 && le16(record, 8) == le16(record, 10);
        return Ok((one_disk && ends_at(&directory, record_at)).then_some(directory));
    }
    let locator_at = record_at - ZIP64_LOCATOR_LEN;
    let zip64_at = le64(&locator, 8);
    if le32(&locator, 4) != 0
        || le32(&locator, 16) > 1
        || zip64_at
            .checked_add(ZIP64_END_RECORD_LEN)
            .is_none_or(|end| end > locator_at)
    {
        return Ok(None);
    }
    let mut zip64 = [0u8; ZIP64_END_RECORD_LEN as usize];
    input.read_at(zip64_at, &mut zip64)?;
    // The record's size counts what follows its first 12 bytes.
    let ends_at_locator = le64(&zip64, 4).checked_add(12) == Some(locator_at - zip64_at);
    let one_disk =
        le32(&zip64, 16) == 0 && le32(&zip64, 20) == 0 && le64(&zip64, 24) == le64(&zip64, 32);
    let directory = Directory {
        start: le64(&zip64, 48),
        size: le64(&zip64, 40),
        entries: le64(&zip64, 32),
    };
    Ok((&zip64[..4] == ZIP64_END_RECORD
        && ends_at_locator
        && one_disk
        && ends_at(&directory, zip64_at))
    .then_some(directory))
}

/// Whether the directory ends right where the record after it starts.
fn ends_at(directory: &Directory, record_at: u64) -> bool {
    directory.start.checked_add(directory.size) == Some(record_at)
}

/// Read one central directory header, `left` being what remains of the
/// directory.
fn read_central_header<R: Read + Seek>(
    input: &mut Input<R>,
    left: &mut u64,
) -> io::Result<CentralEntry> {
    let damaged = || invalid("a zip central directory header is damaged");
    let mut header = [0u8; CENTRAL_HEADER_LEN];
    take(left, CENTRAL_HEADER_LEN as u64).ok_or_else(damaged)?;
    input.read_exact(&mut header)?;
    if &header[..4] != CENTRAL_HEADER {
        return Err(damaged());
    }
    let mut name = vec![0; usize::from(le16(&header, 28))];
    let mut extra = vec![0; usize::from(le16(&header, 30))];
    let comment_len = u64::from(le16(&header, 32));
    take(left, (name.len() + extra.len()) as u64 + comment_len).ok_or_else(damaged)?;
    input.read_exact(&mut name)?;
    input.read_exact(&mut extra)?;
    input.skip(comment_len)?;

    let mut size = u64::from(le32(&header, 24));
    let mut compressed_size = u64::from(le32(&header, 20));
    let mut local_header = u64::from(le32(&header, 42));
    let mut disk = u64::from(le16(&header, 34));
    if [size, compressed_size, local_header].contains(&u64::from(u32::MAX))
        || disk == u64::from(u16::MAX)
    {
        let mut zip64 = zip64_extra(&extra)?;
        let mut next = |field: &mut u64, max: u64, len: usize| -> io::Result<()> {
            if *field == max {
                *field = zip64.take_le(len).ok_or_else(zip64_damaged)?;
            }
            Ok(())
        };
        next(&mut size, u64::from(u32::MAX), 8)?;
        next(&mut compressed_size, u64::from(u32::MAX), 8)?;
        next(&mut local_header, u64::from(u32::MAX), 8)?;
        next(&mut disk, u64::from(u16::MAX), 4)?;
    }
    if disk != 0 {
        return Err(invalid(
            "a zip entry starts on another disk, so the zip is split",
        ));
    }
    let mode = le32(&header, 38) >> 16;
    let is_symlink = mode & 0o170000 == 0o120000;
    Ok(CentralEntry {
        is_regular_file: !name.ends_with(b"/") && !is_symlink,
        name,
        flags: le16(&header, 8),
        method: le16(&header, 10),
        crc: le32(&header, 16),
        compressed_size,
        size,
        local_header,
    })
}

/// Take `amount` off `left`, or `None` when that is more than is left.
fn take(left: &mut u64, amount: u64) -> Option<()> {
    *left = left.checked_sub(amount)?;
    Some(())
}

/// The zip64 extended information in an extra field, which must parse as
/// a list of `(id, size, data)` blocks.
fn zip64_extra(mut extra: &[u8]) -> io::Result<Fields<'_>> {
    let mut found = None;
    while !extra.is_empty() {
        if extra.len() < 4 {
            return Err(zip64_damaged());
        }
        let id = le16(extra, 0);
        let len = usize::from(le16(extra, 2));
        let data = extra.get(4..4 + len).ok_or_else(zip64_damaged)?;
        if id == ZIP64_EXTRA_ID && found.is_none() {
            found = Some(Fields(data));
        }
        extra = &extra[4 + len..];
    }
    found.ok_or_else(zip64_damaged)
}

/// The values of a zip64 extra field, taken in order.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn take_le(&mut self, len: usize) -> Option<u64> {
        let (value, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(
            value
                .iter()
                .rev()
                .fold(0u64, |n, &byte| (n << 8) | u64::from(byte)),
        )
    }
}

/// Check what follows the last central directory header: nothing, or one
/// digital signature record filling the rest exactly.
fn check_directory_tail<R: Read + Seek>(input: &mut Input<R>, left: u64) -> io::Result<()> {
    if left == 0 {
        return Ok(());
    }
    let mut head = [0u8; 6];
    let fits = left >= 6 && {
        input.read_exact(&mut head)?;
        &head[..4] == DIGITAL_SIGNATURE && u64::from(le16(&head, 4)) + 6 == left
    };
    if !fits {
        return Err(invalid(
            "the zip's central directory holds more than its declared entries",
        ));
    }
    Ok(())
}

fn local_header_damaged() -> io::Error {
    invalid("a zip entry's local header is damaged")
}

fn zip64_damaged() -> io::Error {
    invalid("a zip entry's zip64 information is missing or damaged")
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// Fail when two zip entries' compressed data overlap, given each entry's
/// `(data start, compressed size)`. The format allows it, but a crafted zip
/// can point any number of entries at the same few bytes, each
/// decompressing up to [`super::MAX_DOCUMENT_BYTES`]. Sorting first takes
/// n log n steps rather than comparing every pair.
fn check_no_shared_data(spans: &mut Spans) -> io::Result<()> {
    spans.sort_unstable_by_key(|&(start, ..)| start);
    let mut previous_end: Option<u64> = None;
    for &(start, size, _) in spans.iter().filter(|&&(_, size, _)| size > 0) {
        if previous_end.is_some_and(|end| end > start) {
            return Err(invalid("zip entries share compressed data"));
        }
        previous_end = Some(start.saturating_add(size));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_data_is_told_apart_from_adjacent_and_empty_data() {
        let check = |data: &[(u64, u64)]| {
            let mut spans: Spans = data
                .iter()
                .enumerate()
                .map(|(position, &(start, size))| (start, size, position))
                .collect();
            check_no_shared_data(&mut spans)
        };
        assert!(check(&[(30, 10), (0, 10), (10, 20)]).is_ok());
        assert!(check(&[(5, 0), (5, 0), (0, 10)]).is_ok());
        assert!(check(&[(0, 10), (9, 5)]).is_err());
        assert!(check(&[(0, u64::MAX), (9, 5)]).is_err());
        assert!(check(&[(0, 100), (40, 0), (10, 5)]).is_err());
    }

    #[test]
    fn zip64_fields_are_read_in_order_only_for_the_fields_at_their_maximum() {
        let mut data = Vec::new();
        data.extend(7u64.to_le_bytes());
        data.extend(9u64.to_le_bytes());
        let mut extra = Vec::new();
        extra.extend(0x5455u16.to_le_bytes());
        extra.extend(1u16.to_le_bytes());
        extra.push(0);
        extra.extend(ZIP64_EXTRA_ID.to_le_bytes());
        extra.extend((data.len() as u16).to_le_bytes());
        extra.extend(&data);

        let mut fields = zip64_extra(&extra).expect("zip64");

        assert_eq!(fields.take_le(8), Some(7));
        assert_eq!(fields.take_le(8), Some(9));
        assert_eq!(fields.take_le(8), None);
    }

    #[test]
    fn extra_fields_that_do_not_parse_or_lack_zip64_are_rejected() {
        let mut runs_over = Vec::new();
        runs_over.extend(ZIP64_EXTRA_ID.to_le_bytes());
        runs_over.extend(16u16.to_le_bytes());
        runs_over.extend([0u8; 8]);
        for extra in [&b""[..], b"\x55\x54", &runs_over, b"\x55\x54\x00\x00"] {
            assert!(zip64_extra(extra).is_err(), "{extra:?}");
        }
    }
}
