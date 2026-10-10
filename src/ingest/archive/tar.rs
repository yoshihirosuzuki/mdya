//! Walking a tar stream block by block.
//!
//! The 512-byte blocks are walked here rather than by the `tar` crate: its
//! own walk reads extension headers into memory whatever their size and
//! splits PAX records at newlines, losing the records after a value that
//! holds one, and its raw walk steps over an entry by its header's size
//! even when a PAX `size` record or a GNU sparse map says otherwise. The
//! crate's header type still picks the fields out of a header block.
//!
//! What the walk accepts, and what makes the archive unreadable:
//!
//! - A header's checksum must match the unsigned or the signed sum of its
//!   bytes (old tars wrote the signed one); otherwise the header is corrupt.
//! - A number field holds octal digits, optionally after spaces and then
//!   followed by any mix of spaces and NULs to the end of the field (tools
//!   differ), or, in a size or sparse field, GNU's base-256 form, whose
//!   first byte is 0x80. A number over 2^63 − 1 is not a number. A size
//!   that is not a number makes the archive unreadable, unless the entry's
//!   type carries no data (`2` to `6`, below), whose size is not read.
//! - Types `0`, NUL and `7` are regular files; with GNU sparse PAX records
//!   they are sparse files, as type `S` is (see [`super::sparse`]). Types
//!   `2` to `6` (links, devices, directories, FIFOs) carry no data whatever
//!   their size says, as POSIX defines them. A hard link (`1`) must carry
//!   none: POSIX's pax lets one carry data and its ustar does not, so data
//!   there would leave the next entry's place unsettled. Other types are
//!   stepped over by their size.
//! - `L` (a long name), `K` (a long link name) and `x` / `X` (PAX records)
//!   apply to the next entry. Two of one kind before an entry, or both an
//!   `L` and a PAX `path`, leave its name or size unsettled. Any left with
//!   no entry before the end-of-archive marker are dropped.
//! - PAX records are split by their length prefixes and handled one at a
//!   time, never held whole: only `path`, `size` and `GNU.sparse.*` are
//!   kept, a later record replacing an earlier one, and other records
//!   (with keys of any length) are read through. A record that does not
//!   parse, an empty `path` or `size` (which in POSIX clears the header's
//!   field), or a global header (`g`) with a `path`, `size` or sparse
//!   record makes the archive unreadable.
//! - The name is a GNU sparse file's real name, else the PAX `path`, else
//!   the long name, else the header's name, with the header's `prefix` in
//!   front when the header is ustar (magic `ustar\0` and version `00`).
//! - The archive must end with two zero blocks. A zero block followed by
//!   anything else, or input that ends without them, makes it unreadable:
//!   a tar cut short at an entry boundary would otherwise look complete,
//!   and a header zeroed by damage would hide the entries after it.
//!   Nothing after the marker is read.

use std::io::{self, BufRead, Read};

use ::tar::{GnuHeader, Header};

use super::sparse::{self, Fragments, SparseLayout, SparseRecords};
use super::{
    ArchiveDocument, DocumentReadError, MAX_LISTED_NAME_BYTES, PartialListing, RawEntry, invalid,
    names, names_too_large, read_capped,
};

const BLOCK: u64 = 512;

/// Longest PAX key whose record the walk looks at; a record with a longer
/// key is read through like any other it does not use.
const MAX_PAX_KEY_BYTES: u64 = 4096;

/// Largest value of a tar number field, a PAX `size` or a sparse number.
pub(super) const MAX_NUMBER: u64 = i64::MAX as u64;

/// List the entries of a tar stream into `listing`, handing back the stream
/// once the end-of-archive marker is read.
pub(super) fn list_tar<S: BufRead>(source: S, listing: &mut PartialListing) -> io::Result<S> {
    let mut walk = Walk::new(source);
    while let Some(entry) = walk.next_entry(listing.name_room())? {
        listing.add(RawEntry {
            order: entry.position as u64,
            position: entry.position,
            name: &entry.name,
            is_regular_file: entry.is_regular_file(),
        })?;
    }
    Ok(walk.into_source())
}

/// Read `wanted` out of a tar stream, walking it again from the start and
/// stopping after the last one.
pub(super) fn read_tar(
    source: impl BufRead,
    wanted: &[ArchiveDocument],
    limit: u64,
    mut deliver: impl FnMut(&ArchiveDocument, Result<Vec<u8>, DocumentReadError>) -> bool,
) -> io::Result<()> {
    let mut pending = wanted.iter().peekable();
    let mut walk = Walk::new(source);
    while pending.peek().is_some() {
        let Some(entry) = walk.next_entry(MAX_LISTED_NAME_BYTES)? else {
            break;
        };
        let Some(document) = pending.next_if(|d| d.position == entry.position) else {
            continue;
        };
        let result = if entry.is_regular_file() && names(&entry.name, &document.path) {
            entry.read_contents(limit)
        } else {
            Err(DocumentReadError::Changed)
        };
        if !deliver(document, result) {
            break;
        }
    }
    Ok(())
}

struct Walk<S> {
    source: S,
    /// Entries handed out so far, so both passes number them alike.
    position: usize,
    /// Bytes of the current entry's data not read yet.
    unread: u64,
    /// Padding after the current entry's data, up to the next block.
    padding: u64,
}

/// One entry, with its data ready to read.
pub(super) struct TarEntry<'w, S> {
    pub(super) position: usize,
    /// The resolved name (see the module docs).
    pub(super) name: Vec<u8>,
    kind: Kind,
    data_len: u64,
    data: Limited<'w, S>,
}

enum Kind {
    File,
    /// A sparse file, or why its map cannot be used.
    Sparse(Result<SparseLayout, String>),
    /// Not a regular file.
    Other,
}

impl<S: BufRead> TarEntry<'_, S> {
    pub(super) fn is_regular_file(&self) -> bool {
        !matches!(self.kind, Kind::Other)
    }

    /// The file the entry holds, rebuilt if it is sparse.
    pub(super) fn read_contents(self, limit: u64) -> Result<Vec<u8>, DocumentReadError> {
        match self.kind {
            Kind::File => read_capped(self.data, limit),
            Kind::Sparse(layout) => sparse::rebuild(
                layout.map_err(DocumentReadError::Read)?,
                self.data,
                self.data_len,
                limit,
            ),
            Kind::Other => Err(DocumentReadError::Changed),
        }
    }
}

impl<S: BufRead> Walk<S> {
    fn new(source: S) -> Self {
        Self {
            source,
            position: 0,
            unread: 0,
            padding: 0,
        }
    }

    fn into_source(self) -> S {
        self.source
    }

    /// The next entry, or `None` once the end-of-archive marker is read.
    /// `name_room` is how many bytes of names the entry may hold.
    fn next_entry(&mut self, name_room: u64) -> io::Result<Option<TarEntry<'_, S>>> {
        self.skip_entry()?;
        let mut extensions = Extensions::default();
        let block = loop {
            let Some(block) = self.read_header()? else {
                return Ok(None);
            };
            let header = Header::from_byte_slice(&block);
            let typeflag = header.as_old().linkflag[0];
            if !matches!(typeflag, b'L' | b'K' | b'x' | b'X' | b'g') {
                break block;
            }
            self.start_data(header_size(header)?);
            match typeflag {
                b'L' => extensions.read_long_name(&mut self.data(), name_room)?,
                b'K' => extensions.note_long_link()?,
                b'g' => read_global_pax(&mut self.data())?,
                _ => extensions.read_local_pax(&mut self.data(), name_room)?,
            }
            self.skip_entry()?;
        };
        let header = Header::from_byte_slice(&block);
        let Extensions { long_name, pax, .. } = extensions;
        let PaxRecords {
            path,
            size,
            mut sparse,
        } = pax.unwrap_or_default();
        if long_name.is_some() && path.is_some() {
            return Err(invalid(
                "a tar entry has both a GNU long name and a PAX path, so its name is not settled",
            ));
        }
        sparse.check_name()?;
        let name = sparse
            .take_name()
            .or(path)
            .or(long_name)
            .unwrap_or_else(|| header.path_bytes().into_owned());
        let typeflag = header.as_old().linkflag[0];
        // Types 2 to 6 carry no data, so their size field is not read: one
        // that is not a number leaves nothing unsettled.
        let size = match (typeflag, size) {
            (b'2'..=b'6', _) => 0,
            (_, Some(size)) => size,
            (_, None) => header_size(header)?,
        };
        let (data_len, kind) = match typeflag {
            b'2'..=b'6' => (0, Kind::Other),
            b'1' if size == 0 => (0, Kind::Other),
            b'1' => {
                return Err(invalid(
                    "a tar hard link carries data, so where the next entry starts is not settled",
                ));
            }
            b'S' => {
                let gnu = header.as_gnu().ok_or_else(|| {
                    invalid("an old GNU sparse entry in the tar is not in the GNU format")
                })?;
                let map = self.read_old_sparse_map(gnu)?;
                let layout = if sparse.any() {
                    Err("the tar entry has two sparse maps".to_string())
                } else {
                    map
                };
                (size, Kind::Sparse(layout))
            }
            0 | b'0' | b'7' if sparse.any() => (size, Kind::Sparse(sparse.layout())),
            0 | b'0' | b'7' => (size, Kind::File),
            _ => (size, Kind::Other),
        };
        self.start_data(data_len);
        let position = self.position;
        self.position += 1;
        Ok(Some(TarEntry {
            position,
            name,
            kind,
            data_len,
            data: self.data(),
        }))
    }

    /// The next header block with its checksum checked, or `None` at the
    /// end-of-archive marker.
    fn read_header(&mut self) -> io::Result<Option<[u8; 512]>> {
        let block = self.read_block()?.ok_or_else(cut_short)?;
        if is_zero(&block) {
            return match self.read_block()? {
                Some(next) if is_zero(&next) => Ok(None),
                Some(_) => Err(invalid(
                    "a zero block in the tar is followed by more data, so a header may be damaged",
                )),
                None => Err(cut_short()),
            };
        }
        check_checksum(&block)?;
        Ok(Some(block))
    }

    /// The next block, or `None` when the input ends before it.
    fn read_block(&mut self) -> io::Result<Option<[u8; 512]>> {
        let mut block = [0; 512];
        let mut filled = 0;
        while filled < block.len() {
            let read = self.source.read(&mut block[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        match filled {
            0 => Ok(None),
            512 => Ok(Some(block)),
            _ => Err(cut_short()),
        }
    }

    /// Read an old GNU sparse map: the header's slots, then extension
    /// blocks for as long as each says another follows.
    fn read_old_sparse_map(&mut self, gnu: &GnuHeader) -> io::Result<Result<SparseLayout, String>> {
        let mut fragments = Fragments::default();
        let mut ended = add_old_slots(
            &mut fragments,
            gnu.sparse
                .iter()
                .map(|slot| (&slot.offset[..], &slot.numbytes[..])),
        );
        let mut extended = gnu.isextended[0] != 0;
        loop {
            if ended && extended {
                return Err(invalid(
                    "an old GNU sparse map ends but says more of it follows, so where the data starts is not settled",
                ));
            }
            if !extended {
                break;
            }
            let block = self.read_block()?.ok_or_else(cut_short)?;
            // An extension block holds 21 slots of 24 bytes, then the flag
            // saying whether another follows.
            ended = add_old_slots(
                &mut fragments,
                block[..504].chunks(24).map(|slot| slot.split_at(12)),
            );
            extended = block[504] != 0;
        }
        let Some(real_size) = number(&gnu.realsize) else {
            return Ok(Err("the sparse file's size is not a number".to_string()));
        };
        Ok(fragments.finish().map(|fragments| SparseLayout::Map {
            real_size,
            fragments,
        }))
    }

    fn start_data(&mut self, len: u64) {
        self.unread = len;
        self.padding = (BLOCK - len % BLOCK) % BLOCK;
    }

    fn data(&mut self) -> Limited<'_, S> {
        Limited {
            inner: &mut self.source,
            remaining: &mut self.unread,
        }
    }

    /// Step over what is left of the current entry's data and its padding.
    fn skip_entry(&mut self) -> io::Result<()> {
        skip(&mut self.data())?;
        skip(&mut Limited {
            inner: &mut self.source,
            remaining: &mut self.padding,
        })
    }
}

/// Add the slots of one old sparse block up to the first unused one, whose
/// length starts with NUL. Returns whether that slot ended the map.
fn add_old_slots<'a>(
    fragments: &mut Fragments,
    slots: impl Iterator<Item = (&'a [u8], &'a [u8])>,
) -> bool {
    for (offset, length) in slots {
        if length[0] == 0 {
            return true;
        }
        fragments.push(number(offset), number(length));
    }
    false
}

fn header_size(header: &Header) -> io::Result<u64> {
    number(&header.as_old().size).ok_or_else(|| invalid("a tar header's size is not a number"))
}

fn cut_short() -> io::Error {
    invalid("the tar ends without its end-of-archive marker, so it may be cut short")
}

fn is_zero(block: &[u8; 512]) -> bool {
    block.iter().all(|&b| b == 0)
}

/// Check a header's checksum, the sum of its bytes with the checksum field
/// taken as spaces. Old tars summed the bytes as signed values, which GNU
/// tar and Python's `tarfile` accept too.
fn check_checksum(block: &[u8; 512]) -> io::Result<()> {
    let recorded = octal(&block[148..156])
        .ok_or_else(|| invalid("a tar header's checksum is not a number"))?;
    let (mut unsigned, mut signed) = (0u64, 0i64);
    for (i, &byte) in block.iter().enumerate() {
        let byte = if (148..156).contains(&i) { b' ' } else { byte };
        unsigned += u64::from(byte);
        signed += i64::from(byte as i8);
    }
    if recorded == unsigned || i64::try_from(recorded).is_ok_and(|r| r == signed) {
        return Ok(());
    }
    Err(invalid(
        "a tar header's checksum does not match, so the header is damaged",
    ))
}

/// A size or sparse field: octal, or GNU's base-256 form (first byte 0x80,
/// then the number's bytes, most significant first).
fn number(field: &[u8]) -> Option<u64> {
    match field.split_first() {
        Some((0x80, bytes)) => bytes.iter().try_fold(0u64, |n, &byte| {
            n.checked_mul(256)
                .and_then(|n| n.checked_add(u64::from(byte)))
                .filter(|&n| n <= MAX_NUMBER)
        }),
        _ => octal(field),
    }
}

/// Octal digits, optionally after spaces, then any mix of spaces and NULs
/// to the end of the field.
fn octal(field: &[u8]) -> Option<u64> {
    let start = field.iter().position(|&b| b != b' ')?;
    let field = &field[start..];
    let digits = field
        .iter()
        .take_while(|b| (b'0'..=b'7').contains(*b))
        .count();
    if digits == 0 || !field[digits..].iter().all(|&b| b == b' ' || b == 0) {
        return None;
    }
    field[..digits].iter().try_fold(0u64, |n, &digit| {
        n.checked_mul(8)
            .and_then(|n| n.checked_add(u64::from(digit - b'0')))
            .filter(|&n| n <= MAX_NUMBER)
    })
}

/// The extension headers read so far for the next entry.
#[derive(Default)]
struct Extensions {
    long_name: Option<Vec<u8>>,
    long_link: bool,
    pax: Option<PaxRecords>,
}

impl Extensions {
    /// Bytes of names held for the next entry.
    fn held(&self) -> u64 {
        let long_name = self.long_name.as_ref().map_or(0, |name| name.len() as u64);
        let pax = self.pax.as_ref().map_or(0, PaxRecords::names_len);
        long_name + pax
    }

    fn read_long_name(&mut self, data: &mut impl BufRead, name_room: u64) -> io::Result<()> {
        if self.long_name.is_some() {
            return Err(two_of_a_kind());
        }
        let room = name_room.saturating_sub(self.held());
        self.long_name = Some(read_until_nul(data, room)?);
        Ok(())
    }

    fn note_long_link(&mut self) -> io::Result<()> {
        if std::mem::replace(&mut self.long_link, true) {
            return Err(two_of_a_kind());
        }
        Ok(())
    }

    fn read_local_pax(&mut self, data: &mut impl BufRead, name_room: u64) -> io::Result<()> {
        if self.pax.is_some() {
            return Err(two_of_a_kind());
        }
        let room = name_room.saturating_sub(self.held());
        self.pax = Some(PaxRecords::read(data, room)?);
        Ok(())
    }
}

fn two_of_a_kind() -> io::Error {
    invalid("two tar extension headers of the same kind describe one entry")
}

/// A GNU long name: the bytes up to the first NUL. The buffer grows as the
/// name is read rather than being sized from the header, whose size could
/// be forged far past the name.
fn read_until_nul(data: &mut impl BufRead, room: u64) -> io::Result<Vec<u8>> {
    let mut name = Vec::new();
    loop {
        let buf = data.fill_buf()?;
        if buf.is_empty() {
            return Ok(name);
        }
        let (part, ended) = match buf.iter().position(|&b| b == 0) {
            Some(nul) => (&buf[..nul], true),
            None => (buf, false),
        };
        if (name.len() + part.len()) as u64 > room {
            return Err(names_too_large(MAX_LISTED_NAME_BYTES));
        }
        name.extend_from_slice(part);
        let read = part.len();
        data.consume(read);
        if ended {
            return Ok(name);
        }
    }
}

/// The records of a local PAX header that the walk uses.
#[derive(Default)]
struct PaxRecords {
    path: Option<Vec<u8>>,
    size: Option<u64>,
    sparse: SparseRecords,
}

impl PaxRecords {
    fn names_len(&self) -> u64 {
        self.path.as_ref().map_or(0, |path| path.len() as u64) + self.sparse.name_len()
    }

    /// Read one local PAX header, holding at most `room` bytes of names.
    fn read(data: &mut impl BufRead, room: u64) -> io::Result<Self> {
        let mut records = Self::default();
        for_each_pax_record(data, |key, value| {
            match key {
                Some(b"path") => {
                    if value.limit() == 0 {
                        return Err(invalid(
                            "a PAX header has an empty path, which would clear the entry's name",
                        ));
                    }
                    // The record replaces any earlier one, which goes first
                    // so the two are never held together.
                    records.path = None;
                    if value.limit() > room.saturating_sub(records.sparse.name_len()) {
                        return Err(names_too_large(MAX_LISTED_NAME_BYTES));
                    }
                    records.path = Some(read_value(value)?);
                }
                Some(b"size") => {
                    if value.limit() == 0 {
                        return Err(invalid(
                            "a PAX header has an empty size, which would clear the entry's size",
                        ));
                    }
                    let size = read_decimal(value)?
                        .ok_or_else(|| invalid("a PAX size record is not a number"))?;
                    records.size = Some(size);
                }
                Some(key) if key.starts_with(b"GNU.sparse.") => {
                    let path = records.path.as_ref().map_or(0, |p| p.len() as u64);
                    records.sparse.record(
                        &key[b"GNU.sparse.".len()..],
                        value,
                        room.saturating_sub(path),
                    )?;
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(records)
    }
}

/// A PAX value read whole, into a buffer of its exact length (which the
/// caller has checked against the room for names), so the buffer never
/// grows past it.
pub(super) fn read_value<R: Read>(value: &mut io::Take<R>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(value.limit()).unwrap_or(0));
    value.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Read a global PAX header through. Its `path`, `size` or sparse records
/// would apply to every entry after it, which needs carrying values across
/// entries and would gather every entry without its own `path` under one
/// name, so they make the archive unreadable; its other records (a
/// `comment`, say) are not used.
fn read_global_pax(data: &mut impl BufRead) -> io::Result<()> {
    for_each_pax_record(data, |key, _| match key {
        Some(b"path" | b"size") => Err(invalid(
            "a global PAX header in the tar sets a path or size for every entry",
        )),
        Some(key) if key.starts_with(b"GNU.sparse.") => Err(invalid(
            "a global PAX header in the tar has sparse file records",
        )),
        _ => Ok(()),
    })
}

/// Split PAX records, `<length> <key>=<value>\n` with the length counting
/// the whole record, by their lengths, and hand each key to `visit` with a
/// reader of its value (which `visit` need not read). A key longer than
/// [`MAX_PAX_KEY_BYTES`] is handed as `None`. NUL bytes after the last
/// record are let through, since no record can hide in them.
fn for_each_pax_record<B: BufRead>(
    data: &mut B,
    mut visit: impl FnMut(Option<&[u8]>, &mut io::Take<&mut B>) -> io::Result<()>,
) -> io::Result<()> {
    let malformed = || invalid("malformed PAX record");
    let mut key = Vec::new();
    loop {
        let Some(&first) = data.fill_buf()?.first() else {
            return Ok(());
        };
        if first == 0 {
            return skip_nul_padding(data);
        }
        let (length, digits) = read_record_length(data)?;
        // What follows the length and its space: the key, `=`, the value
        // and the newline. The shortest has a one-byte key and no value;
        // a shorter length could never move the walk forward.
        let body = length
            .checked_sub(digits + 1)
            .filter(|&body| body >= 3)
            .ok_or_else(malformed)?;
        key.clear();
        let mut key_len = 0u64;
        loop {
            // The `=` and the newline must still fit in the record.
            if key_len + 2 > body {
                return Err(malformed());
            }
            match read_byte(data)?.ok_or_else(malformed)? {
                b'=' => break,
                0 => return Err(malformed()),
                byte => {
                    key_len += 1;
                    if key_len <= MAX_PAX_KEY_BYTES {
                        key.push(byte);
                    }
                }
            }
        }
        if key_len == 0 {
            return Err(malformed());
        }
        let mut value = data.by_ref().take(body - key_len - 2);
        let usable = (key_len <= MAX_PAX_KEY_BYTES).then_some(&key[..]);
        visit(usable, &mut value)?;
        skip(&mut value)?;
        if value.limit() != 0 || read_byte(data)? != Some(b'\n') {
            return Err(malformed());
        }
    }
}

fn read_record_length(data: &mut impl BufRead) -> io::Result<(u64, u64)> {
    let malformed = || invalid("malformed PAX record");
    let (mut length, mut digits) = (0u64, 0u64);
    loop {
        match read_byte(data)?.ok_or_else(malformed)? {
            byte @ b'0'..=b'9' => {
                length = length
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                    .filter(|&n| n <= MAX_NUMBER)
                    .ok_or_else(malformed)?;
                digits += 1;
            }
            b' ' if digits > 0 => return Ok((length, digits)),
            _ => return Err(malformed()),
        }
    }
}

fn skip_nul_padding(data: &mut impl BufRead) -> io::Result<()> {
    loop {
        let buf = data.fill_buf()?;
        if buf.is_empty() {
            return Ok(());
        }
        if buf.iter().any(|&b| b != 0) {
            return Err(invalid("malformed PAX record"));
        }
        let read = buf.len();
        data.consume(read);
    }
}

fn read_byte(data: &mut impl BufRead) -> io::Result<Option<u8>> {
    let Some(&byte) = data.fill_buf()?.first() else {
        return Ok(None);
    };
    data.consume(1);
    Ok(Some(byte))
}

/// A whole value as a decimal number: ASCII digits only (Rust's parser
/// would also take a leading `+`), at most 2^63 − 1. `None` when it is not
/// one.
pub(super) fn read_decimal(value: &mut impl BufRead) -> io::Result<Option<u64>> {
    let mut number: Option<u64> = None;
    loop {
        let buf = value.fill_buf()?;
        if buf.is_empty() {
            return Ok(number);
        }
        for &byte in buf {
            if !byte.is_ascii_digit() {
                return Ok(None);
            }
            number = number
                .unwrap_or(0)
                .checked_mul(10)
                .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                .filter(|&n| n <= MAX_NUMBER);
            if number.is_none() {
                return Ok(None);
            }
        }
        let read = buf.len();
        value.consume(read);
    }
}

/// Read through everything `reader` has left.
pub(super) fn skip(reader: &mut impl BufRead) -> io::Result<()> {
    loop {
        let read = reader.fill_buf()?.len();
        if read == 0 {
            return Ok(());
        }
        reader.consume(read);
    }
}

/// The next `remaining` bytes of a tar stream: an entry's data, or its
/// padding. Running out of input first is an error, since the tar
/// promised those bytes.
struct Limited<'a, S> {
    inner: &'a mut S,
    remaining: &'a mut u64,
}

impl<S: BufRead> Read for Limited<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let read = available.len().min(buf.len());
        buf[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

impl<S: BufRead> BufRead for Limited<'_, S> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if *self.remaining == 0 {
            return Ok(&[]);
        }
        let remaining = usize::try_from(*self.remaining).unwrap_or(usize::MAX);
        let buf = self.inner.fill_buf()?;
        if buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the tar ends inside an entry, so it is cut short",
            ));
        }
        Ok(&buf[..buf.len().min(remaining)])
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
        *self.remaining -= amount as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_fields_are_read_however_tools_end_them() {
        // GNU tar, Python and git end a size with NUL, bsdtar with a space.
        assert_eq!(octal(b"00000001007\0"), Some(0o1007));
        assert_eq!(octal(b"00000001007 "), Some(0o1007));
        // Checksums: six digits, NUL, space; or seven digits and NUL.
        assert_eq!(octal(b"010622\0 "), Some(0o10622));
        assert_eq!(octal(b"0010622\0"), Some(0o10622));
        // Old tars put spaces in front.
        assert_eq!(octal(b"   12 \0\0\0\0"), Some(0o12));
    }

    #[test]
    fn fields_that_hold_no_octal_number_are_not_numbers() {
        for field in [
            &b"\0\0\0\0\0\0\0\0"[..],
            b"        ",
            b"0000008\0",
            b"00 12\0\0\0",
            b"zzzzzzz\0",
            b"-0000001",
        ] {
            assert_eq!(octal(field), None, "{field:?}");
        }
    }

    #[test]
    fn numbers_past_2_63_minus_1_are_not_numbers() {
        // 2^63 - 1 is 21 octal digits, 777777777777777777777.
        assert_eq!(octal(b"777777777777777777777"), Some(MAX_NUMBER));
        assert_eq!(octal(b"1000000000000000000000"), None);
        let mut base256 = [0u8; 12];
        base256[0] = 0x80;
        base256[4..].copy_from_slice(&MAX_NUMBER.to_be_bytes());
        assert_eq!(number(&base256), Some(MAX_NUMBER));
        base256[4] = 0x80;
        assert_eq!(number(&base256), None);
    }

    #[test]
    fn base256_sizes_are_read() {
        let mut field = [0u8; 12];
        field[0] = 0x80;
        field[4..].copy_from_slice(&(8u64 << 30).to_be_bytes());
        assert_eq!(number(&field), Some(8 << 30));
    }

    fn block_with_checksum(sum: impl Fn(&[u8; 512]) -> i64) -> [u8; 512] {
        let mut header = ::tar::Header::new_ustar();
        header.as_old_mut().name[..4].copy_from_slice(b"a.md");
        // A byte over 0x7f makes the signed and unsigned sums differ.
        header.as_old_mut().name[4] = 0xe9;
        header.set_size(0);
        let mut block = *header.as_bytes();
        block[148..156].copy_from_slice(b"        ");
        let value = sum(&block);
        block[148..156].copy_from_slice(format!("{value:06o}\0 ").as_bytes());
        block
    }

    #[test]
    fn checksums_summed_signed_or_unsigned_are_accepted() {
        let unsigned = block_with_checksum(|b| b.iter().map(|&x| i64::from(x)).sum());
        let signed = block_with_checksum(|b| b.iter().map(|&x| i64::from(x as i8)).sum());

        assert!(check_checksum(&unsigned).is_ok());
        assert!(check_checksum(&signed).is_ok());
    }

    #[test]
    fn a_checksum_that_matches_neither_sum_is_rejected() {
        let wrong = block_with_checksum(|b| b.iter().map(|&x| i64::from(x)).sum::<i64>() + 1);

        assert!(check_checksum(&wrong).is_err());
    }

    /// Each record's key (`None` when too long) and value.
    type Records = Vec<(Option<Vec<u8>>, Vec<u8>)>;

    fn pax_records(bytes: &[u8]) -> io::Result<Records> {
        let mut records = Vec::new();
        let mut data = bytes;
        for_each_pax_record(&mut data, |key, value| {
            let mut bytes = Vec::new();
            value.read_to_end(&mut bytes)?;
            records.push((key.map(<[u8]>::to_vec), bytes));
            Ok(())
        })?;
        Ok(records)
    }

    #[test]
    fn pax_records_are_split_by_their_lengths() {
        let records = pax_records(b"15 key=a\nb\nc\n=\n10 path=x\n\0\0\0").expect("records");

        assert_eq!(
            records,
            [
                (Some(b"key".to_vec()), b"a\nb\nc\n=".to_vec()),
                (Some(b"path".to_vec()), b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn values_may_hold_nul_and_long_keys_are_passed_over() {
        let long_key = "k".repeat(MAX_PAX_KEY_BYTES as usize + 1);
        // A four-digit length, its space, the key, `=`, `v` and the newline.
        let record = format!("{} {long_key}=v\n", long_key.len() + 8);
        let mut bytes = b"12 path=a\0b\n".to_vec();
        bytes.extend(record.as_bytes());

        let records = pax_records(&bytes).expect("records");

        assert_eq!(records[0], (Some(b"path".to_vec()), b"a\0b".to_vec()));
        assert_eq!(records[1], (None, b"v".to_vec()));
    }

    #[test]
    fn malformed_pax_records_are_rejected() {
        for bytes in [
            // The length runs past the header's end.
            &b"99 path=a.md\n"[..],
            // No `=`, no newline at the end, an empty key, a NUL in the key.
            b"13 path a.md\n",
            b"13 path=a.md!",
            b"8 =a.md\n",
            b"10 pa\0h=a\n",
            // Not a length: a sign, no digits, no space.
            b"+14 path=a.md\n",
            b" 13 path=a.md\n",
            b"13path=a.md\n",
            // Shorter than the shortest record, which would never advance.
            b"4 k=\n",
            b"0 \n",
            // NUL bytes ahead of a record.
            b"\x00\x00\x0010 path=x\n",
            // A length whose last digit would carry past 2^64 - 1.
            b"18446744073709551616 path=x\n",
        ] {
            assert!(pax_records(bytes).is_err(), "{bytes:?}");
        }
        // The shortest record: one-byte key, empty value.
        assert!(pax_records(b"5 k=\n").is_ok());
    }

    #[test]
    fn decimals_are_digits_only_up_to_2_63_minus_1() {
        let decimal = |s: &str| read_decimal(&mut s.as_bytes()).expect("read");
        assert_eq!(decimal("8589934592"), Some(8 << 30));
        assert_eq!(decimal("0009"), Some(9));
        assert_eq!(decimal(&MAX_NUMBER.to_string()), Some(MAX_NUMBER));
        // 2^64 and just past it: the last digit's addition would carry past
        // 2^64 - 1 and wrap around to a small number if it went unchecked.
        for value in [
            "",
            "+4",
            "-4",
            "4 ",
            "1e3",
            "9223372036854775808",
            "18446744073709551616",
            "18446744073709551619",
        ] {
            assert_eq!(decimal(value), None, "{value:?}");
        }
    }
}
