//! Reads one central directory header (4.3.12 of APPNOTE.TXT).

use std::io;

use flate2::Crc;

use super::source::{Source, u16_at, u32_at, u64_at};
use crate::types::BoxError;

pub(super) const HEADER_SIZE: u64 = 46;
const SIGNATURE: &[u8] = b"PK\x01\x02";
const FLAG_ENCRYPTED: u16 = 1;
const FLAG_UTF8: u16 = 1 << 11;
const EXTRA_ZIP64: u16 = 0x0001;
const EXTRA_UNICODE_PATH: u16 = 0x7075;
/// The version byte and the name's CRC-32, before the Unicode name.
const UNICODE_PATH_LEADING: usize = 5;
const UNIX_KIND_MASK: u32 = 0o170000;
const UNIX_REGULAR: u32 = 0o100000;

/// What a central directory header says about an entry.
pub(super) struct Header {
    name: NameLocation,
    external_attributes: u32,
    pub(super) method: u16,
    pub(super) encrypted: bool,
    pub(super) crc32: u32,
    pub(super) compressed: u64,
    pub(super) uncompressed: u64,
    pub(super) local_offset: u64,
    /// Where the next header starts.
    pub(super) next: u64,
}

/// Where an entry's name is in the file. The first pass keeps the name it
/// returns only after counting its length against the limit on the list.
#[derive(Clone, Copy)]
struct NameLocation {
    at: u64,
    length: u64,
}

impl Header {
    pub(super) fn name_length(&self) -> u64 {
        self.name.length
    }

    pub(super) fn read_name(&self, source: &mut Source) -> io::Result<Vec<u8>> {
        source.read_at(self.name.at, self.name.length)
    }

    /// Whether the entry is a regular file. A Unix kind of 0 means that whoever
    /// wrote the entry recorded no Unix mode, so the kind is not known.
    pub(super) fn is_regular(&self, name: &[u8]) -> bool {
        let kind = (self.external_attributes >> 16) & UNIX_KIND_MASK;
        !name.ends_with(b"/") && (kind == 0 || kind == UNIX_REGULAR)
    }
}

/// Reads the header at `at`, which must end by `directory_end`.
pub(super) fn read_header(
    source: &mut Source,
    at: u64,
    directory_end: u64,
) -> Result<Header, BoxError> {
    if at + HEADER_SIZE > directory_end {
        return Err("a central directory header runs past the directory".into());
    }
    let mut fixed = [0; HEADER_SIZE as usize];
    source.read_exact_at(at, &mut fixed)?;
    if !fixed.starts_with(SIGNATURE) {
        return Err("a central directory header has the wrong signature".into());
    }
    let name_at = at + HEADER_SIZE;
    let name_length = u64::from(u16_at(&fixed, 28));
    let extra_at = name_at + name_length;
    let extra_length = u64::from(u16_at(&fixed, 30));
    let next = extra_at + extra_length + u64::from(u16_at(&fixed, 32));
    if next > directory_end {
        return Err("a central directory header runs past the directory".into());
    }
    let extra = source.read_at(extra_at, extra_length)?;
    let fields = parse_extra(&extra)?;
    let sizes = Sizes::read(&fixed, fields.zip64)?;
    if sizes.disk_start != 0 {
        return Err("an entry starts on another disk".into());
    }
    let flags = u16_at(&fixed, 8);
    let in_directory = NameLocation {
        at: name_at,
        length: name_length,
    };
    let name = choose_name(source, flags, in_directory, extra_at, fields.unicode_path)?;
    Ok(Header {
        name,
        external_attributes: u32_at(&fixed, 38),
        method: u16_at(&fixed, 10),
        encrypted: flags & FLAG_ENCRYPTED != 0,
        crc32: u32_at(&fixed, 16),
        compressed: sizes.compressed,
        uncompressed: sizes.uncompressed,
        local_offset: sizes.local_offset,
        next,
    })
}

/// The extra field records that this reader uses.
#[derive(Default)]
struct ExtraFields<'a> {
    zip64: Option<&'a [u8]>,
    unicode_path: Option<UnicodePath<'a>>,
}

/// An Info-ZIP Unicode Path Extra Field.
#[derive(Clone, Copy)]
struct UnicodePath<'a> {
    /// Where `data` starts within the extra field.
    offset: usize,
    data: &'a [u8],
}

fn parse_extra(extra: &[u8]) -> Result<ExtraFields<'_>, BoxError> {
    let mut fields = ExtraFields::default();
    let mut rest = extra;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err("an extra field is cut short".into());
        }
        let end = 4 + usize::from(u16_at(rest, 2));
        let Some(data) = rest.get(4..end) else {
            return Err("an extra field runs past its end".into());
        };
        match u16_at(rest, 0) {
            EXTRA_ZIP64 => fields.zip64 = fields.zip64.or(Some(data)),
            EXTRA_UNICODE_PATH => {
                let offset = extra.len() - rest.len() + 4;
                fields.unicode_path = fields.unicode_path.or(Some(UnicodePath { offset, data }));
            }
            _ => {}
        }
        rest = &rest[end..];
    }
    Ok(fields)
}

struct Sizes {
    uncompressed: u64,
    compressed: u64,
    local_offset: u64,
    disk_start: u32,
}

impl Sizes {
    /// 4.5.3: a field at its maximum is in the zip64 extra field, in this order
    /// and only if it is at its maximum.
    fn read(fixed: &[u8], zip64: Option<&[u8]>) -> Result<Self, BoxError> {
        let mut zip64 = Zip64Values { rest: zip64 };
        Ok(Self {
            uncompressed: zip64.widened(u32_at(fixed, 24))?,
            compressed: zip64.widened(u32_at(fixed, 20))?,
            local_offset: zip64.widened(u32_at(fixed, 42))?,
            disk_start: zip64.widened_disk(u16_at(fixed, 34))?,
        })
    }
}

/// The values of a zip64 extra field that have not been used yet.
struct Zip64Values<'a> {
    rest: Option<&'a [u8]>,
}

impl Zip64Values<'_> {
    fn widened(&mut self, field: u32) -> Result<u64, BoxError> {
        if field != u32::MAX {
            return Ok(u64::from(field));
        }
        Ok(u64_at(self.take(8)?, 0))
    }

    fn widened_disk(&mut self, field: u16) -> Result<u32, BoxError> {
        if field != u16::MAX {
            return Ok(u32::from(field));
        }
        Ok(u32_at(self.take(4)?, 0))
    }

    fn take(&mut self, length: usize) -> Result<&[u8], BoxError> {
        let Some((value, rest)) = self.rest.and_then(|rest| rest.split_at_checked(length)) else {
            return Err("a zip64 extra field is missing or too short".into());
        };
        self.rest = Some(rest);
        Ok(value)
    }
}

/// 4.6.9: the Unicode Path counts only for a name not flagged as UTF-8, with
/// version 1, and when it was made from the name that is in the directory.
fn choose_name(
    source: &mut Source,
    flags: u16,
    in_directory: NameLocation,
    extra_at: u64,
    unicode_path: Option<UnicodePath<'_>>,
) -> io::Result<NameLocation> {
    if flags & FLAG_UTF8 != 0 {
        return Ok(in_directory);
    }
    let Some(UnicodePath { offset, data }) =
        unicode_path.filter(|path| path.data.len() >= UNICODE_PATH_LEADING && path.data[0] == 1)
    else {
        return Ok(in_directory);
    };
    let mut crc = Crc::new();
    crc.update(&source.read_at(in_directory.at, in_directory.length)?);
    if crc.sum() != u32_at(data, 1) {
        return Ok(in_directory);
    }
    Ok(NameLocation {
        at: extra_at + (offset + UNICODE_PATH_LEADING) as u64,
        length: (data.len() - UNICODE_PATH_LEADING) as u64,
    })
}
