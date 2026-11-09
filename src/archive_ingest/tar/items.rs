//! Walks a tar's headers, item by item.

use std::io::{self, BufRead, Read};

use super::header::{BLOCK_LEN, Header, SparseSlot, slots};
use super::records::Records;
use super::source::{TarBytes, cut_short};
use super::sparse::{self, DataBlocks, SparseMap, SparseRecords};
use super::{ListSize, Stop};
use crate::types::BoxError;

const SPARSE_PREFIX: &[u8] = b"GNU.sparse.";
const SPARSE_HEADER_SLOTS: usize = 504;
const SPARSE_HEADER_IS_EXTENDED: usize = 504;

/// How an item's data is to be read.
pub(super) enum Layout {
    Regular,
    /// A sparse file, or why its map cannot be used.
    Sparse(Result<SparseMap, String>),
    NotRegular,
}

pub(super) struct Item {
    /// Where the item's header block starts in the tar.
    pub(super) at: u64,
    pub(super) name: Vec<u8>,
    pub(super) layout: Layout,
}

impl Item {
    pub(super) fn is_regular(&self) -> bool {
        !matches!(self.layout, Layout::NotRegular)
    }
}

/// Why an item's content was not returned.
pub(super) enum ContentFailure {
    /// Only this document cannot be read.
    Document(BoxError),
    /// The tar ended or could not be read, so nothing after this can be.
    Rest(BoxError),
}

impl ContentFailure {
    pub(super) fn document(cause: impl Into<BoxError>) -> Self {
        Self::Document(cause.into())
    }
}

impl From<io::Error> for ContentFailure {
    fn from(error: io::Error) -> Self {
        Self::Rest(Box::new(error))
    }
}

/// What the headers before an item said about it.
#[derive(Default)]
struct PrefixHeaders {
    long_name: Option<Vec<u8>>,
    long_link_seen: bool,
    extended_seen: bool,
    path: Option<Vec<u8>>,
    size: Option<u64>,
    sparse: SparseRecords,
}

pub(super) struct Items {
    bytes: Box<dyn TarBytes>,
    next_block_at: u64,
    data_left: u64,
    padding_left: u64,
}

impl Items {
    pub(super) fn new(bytes: Box<dyn TarBytes>) -> Self {
        Self {
            bytes,
            next_block_at: 0,
            data_left: 0,
            padding_left: 0,
        }
    }

    /// The next item, or `None` after the end-of-archive indicator.
    ///
    /// Names being held count towards `size` while they are held. Once an
    /// item is returned, only its name is left in `size`.
    pub(super) fn next_item(&mut self, size: &mut ListSize) -> Result<Option<Item>, Stop> {
        self.leave_data()?;
        let mut prefix = PrefixHeaders::default();
        loop {
            let at = self.next_block_at;
            let Some(header) = self.read_header()? else {
                self.bytes.end_of_tar()?;
                return Ok(None);
            };
            match header.typeflag() {
                b'L' => self.read_long_name(&header, &mut prefix, size)?,
                b'K' => self.read_long_link(&header, &mut prefix)?,
                b'x' | b'X' => self.read_extended(&header, &mut prefix, size)?,
                b'g' => self.read_global(&header)?,
                _ => return self.settle_item(at, &header, prefix, size).map(Some),
            }
        }
    }

    /// The content of the item `next_item` just returned, which has this
    /// `layout`, if it fits in `max_content`.
    pub(super) fn read_content(
        &mut self,
        layout: Layout,
        max_content: u64,
    ) -> Result<Vec<u8>, ContentFailure> {
        let data_len = self.data_left;
        match layout {
            Layout::Regular => self.read_regular_content(data_len, max_content),
            Layout::Sparse(map) => sparse::assemble(map, &mut self.data(), data_len, max_content),
            Layout::NotRegular => Err(ContentFailure::document("the item is not a regular file")),
        }
    }

    fn read_regular_content(
        &mut self,
        data_len: u64,
        max_content: u64,
    ) -> Result<Vec<u8>, ContentFailure> {
        if data_len > max_content {
            return Err(ContentFailure::document(format!(
                "the document is larger than {max_content} bytes"
            )));
        }
        // Grown as it is read, so that a forged size allocates nothing.
        let mut content = Vec::new();
        self.data().read_to_end(&mut content)?;
        Ok(content)
    }

    fn read_header(&mut self) -> Result<Option<Header>, Stop> {
        let block = self.read_block()?.ok_or_else(no_indicator)?;
        if block.iter().all(|byte| *byte == 0) {
            return match self.read_block()? {
                Some(next) if next.iter().all(|byte| *byte == 0) => Ok(None),
                Some(_) => Err(Stop::unreadable(
                    "a zero block in the tar is followed by more data, so a header may be damaged",
                )),
                None => Err(no_indicator()),
            };
        }
        Header::checked(block)
            .map(Some)
            .ok_or_else(|| Stop::unreadable("a header's checksum does not match"))
    }

    /// The next block, or `None` if the file ends before its first byte.
    fn read_block(&mut self) -> Result<Option<[u8; 512]>, Stop> {
        let mut block = [0; 512];
        let mut filled = 0;
        while filled < block.len() {
            let read = self.bytes.read(&mut block[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        self.next_block_at += BLOCK_LEN;
        match filled {
            0 => Ok(None),
            512 => Ok(Some(block)),
            _ => Err(no_indicator()),
        }
    }

    fn read_long_name(
        &mut self,
        header: &Header,
        prefix: &mut PrefixHeaders,
        size: &mut ListSize,
    ) -> Result<(), Stop> {
        if prefix.long_name.is_some() {
            return Err(two_of_a_kind());
        }
        self.begin_data(header_size(header)?);
        prefix.long_name = Some(read_until_nul(&mut self.data(), size)?);
        self.leave_data()
    }

    fn read_long_link(&mut self, header: &Header, prefix: &mut PrefixHeaders) -> Result<(), Stop> {
        if std::mem::replace(&mut prefix.long_link_seen, true) {
            return Err(two_of_a_kind());
        }
        self.begin_data(header_size(header)?);
        self.leave_data()
    }

    fn read_extended(
        &mut self,
        header: &Header,
        prefix: &mut PrefixHeaders,
        size: &mut ListSize,
    ) -> Result<(), Stop> {
        if std::mem::replace(&mut prefix.extended_seen, true) {
            return Err(two_of_a_kind());
        }
        self.begin_data(header_size(header)?);
        let mut data = self.data();
        let mut records = Records::new(&mut data);
        while records.advance()? {
            take_extended_record(&mut records, prefix, size)?;
        }
        self.leave_data()
    }

    fn read_global(&mut self, header: &Header) -> Result<(), Stop> {
        self.begin_data(header_size(header)?);
        let mut data = self.data();
        let mut records = Records::new(&mut data);
        while records.advance()? {
            let Some(keyword) = records.keyword() else {
                continue;
            };
            if matches!(keyword, b"path" | b"size") || keyword.starts_with(SPARSE_PREFIX) {
                return Err(Stop::unreadable(
                    "a global extended header sets a path, a size or a sparse map for every item",
                ));
            }
        }
        self.leave_data()
    }

    fn settle_item(
        &mut self,
        at: u64,
        header: &Header,
        prefix: PrefixHeaders,
        size: &mut ListSize,
    ) -> Result<Item, Stop> {
        let PrefixHeaders {
            long_name,
            path,
            size: extended_size,
            mut sparse,
            ..
        } = prefix;
        if long_name.is_some() && path.is_some() {
            return Err(Stop::unreadable(
                "an item has both a long name and an extended path",
            ));
        }
        sparse.check_name()?;
        let name = settle_name(header, sparse.take_name(), path, long_name, size)?;
        let (data_len, layout) = self.settle_layout(header, extended_size, sparse)?;
        self.begin_data(data_len);
        Ok(Item { at, name, layout })
    }

    fn settle_layout(
        &mut self,
        header: &Header,
        extended_size: Option<u64>,
        sparse: SparseRecords,
    ) -> Result<(u64, Layout), Stop> {
        let data_len = || extended_size.map_or_else(|| header_size(header), Ok);
        Ok(match header.typeflag() {
            b'2'..=b'6' => (0, Layout::NotRegular),
            b'1' => {
                if data_len()? != 0 {
                    return Err(Stop::unreadable("a hard link carries data"));
                }
                (0, Layout::NotRegular)
            }
            b'S' => {
                let len = data_len()?;
                let map = self.read_old_sparse_map(header)?;
                let map = if sparse.present() {
                    Err("the item has two sparse maps".to_string())
                } else {
                    map
                };
                (len, Layout::Sparse(map))
            }
            0 | b'0' | b'7' if sparse.present() => (data_len()?, Layout::Sparse(sparse.into_map())),
            0 | b'0' | b'7' => (data_len()?, Layout::Regular),
            _ => (data_len()?, Layout::NotRegular),
        })
    }

    /// Reads the map of a typeflag `S` item: the header's four `struct
    /// sparse`, and more blocks for as long as each says that another
    /// follows. The data starts after them, so they are read to their end
    /// even if the map is already known to be of no use.
    fn read_old_sparse_map(&mut self, header: &Header) -> Result<Result<SparseMap, String>, Stop> {
        if !header.is_gnu_format() {
            return Err(Stop::unreadable(
                "an item of typeflag S is not in the GNU format",
            ));
        }
        let mut blocks = DataBlocks::default();
        let mut map_ended = add_old_slots(&mut blocks, header.old_sparse_slots());
        let mut extended = header.old_is_extended();
        loop {
            if map_ended && extended {
                return Err(Stop::unreadable(
                    "a sparse map has ended but says that more of it follows",
                ));
            }
            if !extended {
                break;
            }
            let block = self.read_block()?.ok_or_else(no_indicator)?;
            map_ended = add_old_slots(&mut blocks, slots(&block[..SPARSE_HEADER_SLOTS]));
            extended = block[SPARSE_HEADER_IS_EXTENDED] != 0;
        }
        let Some(real_size) = header.old_real_size() else {
            return Ok(Err("a sparse file's size is not a number".to_string()));
        };
        Ok(blocks
            .finish()
            .map(|blocks| SparseMap::Listed { real_size, blocks }))
    }

    fn begin_data(&mut self, len: u64) {
        self.data_left = len;
        self.padding_left = (BLOCK_LEN - len % BLOCK_LEN) % BLOCK_LEN;
        self.next_block_at += len + self.padding_left;
    }

    fn data(&mut self) -> Data<'_> {
        Data {
            bytes: self.bytes.as_mut(),
            left: &mut self.data_left,
        }
    }

    /// Steps over what is left of the current item's data and its padding.
    fn leave_data(&mut self) -> Result<(), Stop> {
        let to_skip = self.data_left + self.padding_left;
        self.data_left = 0;
        self.padding_left = 0;
        self.bytes.skip(to_skip)?;
        Ok(())
    }
}

fn no_indicator() -> Stop {
    Stop::unreadable("the tar ends without its end-of-archive indicator")
}

fn two_of_a_kind() -> Stop {
    Stop::unreadable("two headers of the same kind describe one item")
}

fn header_size(header: &Header) -> Result<u64, Stop> {
    header
        .size()
        .ok_or_else(|| Stop::unreadable("a header's size is not a number"))
}

/// Reads up to the first NUL, or to the end of the data. The name grows as it
/// is read, and counts towards `size` as it does, because the size the header
/// states could be forged.
fn read_until_nul(data: &mut impl BufRead, size: &mut ListSize) -> Result<Vec<u8>, Stop> {
    let mut name = Vec::new();
    loop {
        let available = data.fill_buf()?;
        if available.is_empty() {
            return Ok(name);
        }
        let nul = available.iter().position(|byte| *byte == 0);
        let part = &available[..nul.unwrap_or(available.len())];
        size.add(part.len() as u64)?;
        name.extend_from_slice(part);
        let read = part.len();
        data.consume(read);
        if nul.is_some() {
            return Ok(name);
        }
    }
}

fn take_extended_record(
    records: &mut Records<'_, impl BufRead>,
    prefix: &mut PrefixHeaders,
    size: &mut ListSize,
) -> Result<(), Stop> {
    let Some(keyword) = records.keyword().map(<[u8]>::to_vec) else {
        return Ok(());
    };
    match keyword.as_slice() {
        b"path" => take_path(records, prefix, size),
        b"size" => take_size(records, prefix),
        keyword => match keyword.strip_prefix(SPARSE_PREFIX) {
            Some(sparse_keyword) => prefix.sparse.take_record(sparse_keyword, records, size),
            None => Ok(()),
        },
    }
}

/// An empty `path` or `size` would, under POSIX, take back the header's own
/// field, leaving the item with no name or no length.
fn take_path(
    records: &mut Records<'_, impl BufRead>,
    prefix: &mut PrefixHeaders,
    size: &mut ListSize,
) -> Result<(), Stop> {
    if records.value_len() == 0 {
        return Err(Stop::unreadable("an extended header has an empty path"));
    }
    if let Some(earlier) = prefix.path.take() {
        size.sub(earlier.len() as u64);
    }
    size.add(records.value_len())?;
    prefix.path = Some(records.read_value()?);
    Ok(())
}

fn take_size(
    records: &mut Records<'_, impl BufRead>,
    prefix: &mut PrefixHeaders,
) -> Result<(), Stop> {
    if records.value_len() == 0 {
        return Err(Stop::unreadable("an extended header has an empty size"));
    }
    let size = records
        .read_decimal()?
        .ok_or_else(|| Stop::unreadable("an extended header's size is not a number"))?;
    prefix.size = Some(size);
    Ok(())
}

/// Picks the name to use, in this order, and leaves only it in `size`.
fn settle_name(
    header: &Header,
    sparse_name: Option<Vec<u8>>,
    path: Option<Vec<u8>>,
    long_name: Option<Vec<u8>>,
    size: &mut ListSize,
) -> Result<Vec<u8>, Stop> {
    let mut chosen: Option<Vec<u8>> = None;
    for candidate in [sparse_name, path, long_name].into_iter().flatten() {
        if chosen.is_some() {
            size.sub(candidate.len() as u64);
        } else {
            chosen = Some(candidate);
        }
    }
    if let Some(name) = chosen {
        return Ok(name);
    }
    let name = header.name();
    size.add(name.len() as u64)?;
    Ok(name)
}

/// Adds the slots of one block up to the first unused one, whose `numbytes`
/// starts with NUL. Returns whether such a slot ended the map.
fn add_old_slots<'a>(blocks: &mut DataBlocks, slots: impl Iterator<Item = SparseSlot<'a>>) -> bool {
    for (offset, length) in slots {
        if length[0] == 0 {
            return true;
        }
        blocks.add(
            super::numbers::octal_or_base_256(offset),
            super::numbers::octal_or_base_256(length),
        );
    }
    false
}

/// The rest of the current item's data. Running out of file first is an
/// error, since the headers promised those bytes.
struct Data<'a> {
    bytes: &'a mut dyn TarBytes,
    left: &'a mut u64,
}

impl Read for Data<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let read = available.len().min(buf.len());
        buf[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

impl BufRead for Data<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if *self.left == 0 {
            return Ok(&[]);
        }
        let limit = usize::try_from(*self.left).unwrap_or(usize::MAX);
        let available = self.bytes.fill_buf()?;
        if available.is_empty() {
            return Err(cut_short());
        }
        Ok(&available[..available.len().min(limit)])
    }

    fn consume(&mut self, amount: usize) {
        self.bytes.consume(amount);
        *self.left -= amount as u64;
    }
}
