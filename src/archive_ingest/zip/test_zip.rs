//! Builds zip files byte by byte, for the forms that no archiver writes.

use std::io::Write;

use flate2::Compression;
use flate2::write::DeflateEncoder;

pub(super) const METHOD_STORED: u16 = 0;
pub(super) const METHOD_DEFLATED: u16 = 8;
pub(super) const FLAG_ENCRYPTED: u16 = 1;
pub(super) const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
pub(super) const FLAG_UTF8: u16 = 1 << 11;

pub(super) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// A field of a central directory header that can be moved to the Zip64
/// Extended Information Extra Field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Zip64Field {
    Uncompressed,
    Compressed,
    LocalOffset,
    DiskStart,
}

#[derive(Clone)]
pub(super) struct TestEntry {
    pub(super) name: Vec<u8>,
    /// The bytes the file holds for the entry, compressed if it is.
    pub(super) data: Vec<u8>,
    pub(super) method: u16,
    pub(super) flags: u16,
    pub(super) crc32: u32,
    pub(super) uncompressed_size: u64,
    pub(super) external_attributes: u32,
    /// The extra field of the central directory header.
    pub(super) extra: Vec<u8>,
    /// The extra field of the local file header.
    pub(super) local_extra: Vec<u8>,
    /// The fields to write as the maximum, with the value in the extra field.
    pub(super) in_zip64: Vec<Zip64Field>,
    pub(super) disk_start: u32,
}

impl TestEntry {
    pub(super) fn stored(name: &str, content: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            data: content.to_vec(),
            method: METHOD_STORED,
            flags: 0,
            crc32: crc32(content),
            uncompressed_size: content.len() as u64,
            external_attributes: 0,
            extra: Vec::new(),
            local_extra: Vec::new(),
            in_zip64: Vec::new(),
            disk_start: 0,
        }
    }

    pub(super) fn deflated(name: &str, content: &[u8]) -> Self {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content).unwrap();
        Self {
            data: encoder.finish().unwrap(),
            method: METHOD_DEFLATED,
            ..Self::stored(name, content)
        }
    }

    /// An entry with the Unix file type `kind` in its external attributes.
    pub(super) fn with_unix_kind(mut self, kind: u32) -> Self {
        self.external_attributes = kind << 16;
        self
    }

    fn write_local(&self, bytes: &mut Vec<u8>) {
        let descriptor = self.flags & FLAG_DATA_DESCRIPTOR != 0;
        bytes.extend_from_slice(b"PK\x03\x04");
        push_u16(bytes, 20);
        push_u16(bytes, self.flags);
        push_u16(bytes, self.method);
        push_u32(bytes, 0);
        push_u32(bytes, if descriptor { 0 } else { self.crc32 });
        push_u32(
            bytes,
            if descriptor {
                0
            } else {
                self.data.len() as u32
            },
        );
        push_u32(
            bytes,
            if descriptor {
                0
            } else {
                self.uncompressed_size as u32
            },
        );
        push_u16(bytes, self.name.len() as u16);
        push_u16(bytes, self.local_extra.len() as u16);
        bytes.extend_from_slice(&self.name);
        bytes.extend_from_slice(&self.local_extra);
        bytes.extend_from_slice(&self.data);
        if descriptor {
            bytes.extend_from_slice(b"PK\x07\x08");
            push_u32(bytes, self.crc32);
            push_u32(bytes, self.data.len() as u32);
            push_u32(bytes, self.uncompressed_size as u32);
        }
    }

    fn write_central(&self, bytes: &mut Vec<u8>, local_offset: u64) {
        let moved = |field| self.in_zip64.contains(&field);
        let extra = self.central_extra(local_offset);
        bytes.extend_from_slice(b"PK\x01\x02");
        push_u16(bytes, 20);
        push_u16(bytes, 20);
        push_u16(bytes, self.flags);
        push_u16(bytes, self.method);
        push_u32(bytes, 0);
        push_u32(bytes, self.crc32);
        push_u32(
            bytes,
            field_or_max(moved(Zip64Field::Compressed), self.data.len() as u64),
        );
        push_u32(
            bytes,
            field_or_max(moved(Zip64Field::Uncompressed), self.uncompressed_size),
        );
        push_u16(bytes, self.name.len() as u16);
        push_u16(bytes, extra.len() as u16);
        push_u16(bytes, 0);
        push_u16(
            bytes,
            if moved(Zip64Field::DiskStart) {
                0xFFFF
            } else {
                self.disk_start as u16
            },
        );
        push_u16(bytes, 0);
        push_u32(bytes, self.external_attributes);
        push_u32(
            bytes,
            field_or_max(moved(Zip64Field::LocalOffset), local_offset),
        );
        bytes.extend_from_slice(&self.name);
        bytes.extend_from_slice(&extra);
    }

    fn central_extra(&self, local_offset: u64) -> Vec<u8> {
        let moved = |field| self.in_zip64.contains(&field);
        let mut values = Vec::new();
        if moved(Zip64Field::Uncompressed) {
            values.extend_from_slice(&self.uncompressed_size.to_le_bytes());
        }
        if moved(Zip64Field::Compressed) {
            values.extend_from_slice(&(self.data.len() as u64).to_le_bytes());
        }
        if moved(Zip64Field::LocalOffset) {
            values.extend_from_slice(&local_offset.to_le_bytes());
        }
        if moved(Zip64Field::DiskStart) {
            values.extend_from_slice(&self.disk_start.to_le_bytes());
        }
        let mut extra = Vec::new();
        if !self.in_zip64.is_empty() {
            extra.extend_from_slice(&extra_field(0x0001, &values));
        }
        extra.extend_from_slice(&self.extra);
        extra
    }
}

/// One record of an extra field.
pub(super) fn extra_field(id: u16, data: &[u8]) -> Vec<u8> {
    let mut field = Vec::new();
    push_u16(&mut field, id);
    push_u16(&mut field, data.len() as u16);
    field.extend_from_slice(data);
    field
}

#[derive(Default)]
pub(super) struct TestZip {
    pub(super) entries: Vec<TestEntry>,
    pub(super) comment: Vec<u8>,
    pub(super) digital_signature: Option<Vec<u8>>,
    pub(super) zip64: bool,
    /// Writes the plain end record's fields as their maximum values.
    pub(super) end_record_at_maximum: bool,
}

/// A built zip file and where its parts are.
pub(super) struct Built {
    pub(super) bytes: Vec<u8>,
    pub(super) local_headers: Vec<usize>,
    pub(super) central_headers: Vec<usize>,
    pub(super) directory_start: usize,
    pub(super) end_record: usize,
    pub(super) zip64_record: usize,
    pub(super) zip64_locator: usize,
}

impl Built {
    pub(super) fn put_u8(&mut self, at: usize, value: u8) {
        self.bytes[at] = value;
    }

    pub(super) fn put_u16(&mut self, at: usize, value: u16) {
        self.bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub(super) fn put_u32(&mut self, at: usize, value: u32) {
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    pub(super) fn put_u64(&mut self, at: usize, value: u64) {
        self.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
}

impl TestZip {
    pub(super) fn of(entries: Vec<TestEntry>) -> Self {
        Self {
            entries,
            ..Self::default()
        }
    }

    pub(super) fn build(&self) -> Built {
        let mut bytes = Vec::new();
        let mut local_headers = Vec::new();
        for entry in &self.entries {
            local_headers.push(bytes.len());
            entry.write_local(&mut bytes);
        }
        let directory_start = bytes.len();
        let mut central_headers = Vec::new();
        for (entry, local) in self.entries.iter().zip(&local_headers) {
            central_headers.push(bytes.len());
            entry.write_central(&mut bytes, *local as u64);
        }
        if let Some(signature) = &self.digital_signature {
            bytes.extend_from_slice(b"PK\x05\x05");
            push_u16(&mut bytes, signature.len() as u16);
            bytes.extend_from_slice(signature);
        }
        let directory_size = bytes.len() - directory_start;
        let (zip64_record, zip64_locator) = self.write_zip64(&mut bytes, directory_start);
        let end_record = bytes.len();
        self.write_end_record(&mut bytes, directory_start, directory_size);
        Built {
            bytes,
            local_headers,
            central_headers,
            directory_start,
            end_record,
            zip64_record,
            zip64_locator,
        }
    }

    fn write_zip64(&self, bytes: &mut Vec<u8>, directory_start: usize) -> (usize, usize) {
        if !self.zip64 {
            return (0, 0);
        }
        let directory_size = bytes.len() - directory_start;
        let record = bytes.len();
        bytes.extend_from_slice(b"PK\x06\x06");
        push_u64(bytes, 44);
        push_u16(bytes, 45);
        push_u16(bytes, 45);
        push_u32(bytes, 0);
        push_u32(bytes, 0);
        push_u64(bytes, self.entries.len() as u64);
        push_u64(bytes, self.entries.len() as u64);
        push_u64(bytes, directory_size as u64);
        push_u64(bytes, directory_start as u64);
        let locator = bytes.len();
        bytes.extend_from_slice(b"PK\x06\x07");
        push_u32(bytes, 0);
        push_u64(bytes, record as u64);
        push_u32(bytes, 1);
        (record, locator)
    }

    fn write_end_record(&self, bytes: &mut Vec<u8>, directory_start: usize, directory_size: usize) {
        let maximum = self.end_record_at_maximum;
        let count = if maximum {
            0xFFFF
        } else {
            self.entries.len() as u16
        };
        bytes.extend_from_slice(b"PK\x05\x06");
        push_u16(bytes, 0);
        push_u16(bytes, 0);
        push_u16(bytes, count);
        push_u16(bytes, count);
        push_u32(bytes, field_or_max(maximum, directory_size as u64));
        push_u32(bytes, field_or_max(maximum, directory_start as u64));
        push_u16(bytes, self.comment.len() as u16);
        bytes.extend_from_slice(&self.comment);
    }
}

/// An end of central directory record, for placing where a test wants it.
pub(super) fn end_record(entries: u16, directory_size: u32, directory_offset: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"PK\x05\x06");
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, 0);
    push_u16(&mut bytes, entries);
    push_u16(&mut bytes, entries);
    push_u32(&mut bytes, directory_size);
    push_u32(&mut bytes, directory_offset);
    push_u16(&mut bytes, 0);
    bytes
}

fn field_or_max(at_maximum: bool, value: u64) -> u32 {
    if at_maximum { u32::MAX } else { value as u32 }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
