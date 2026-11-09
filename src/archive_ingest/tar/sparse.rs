//! Sparse files in a tar: what their records say, and putting a file together
//! from its data blocks.
//!
//! A problem in a sparse map concerns that one document, so it is carried as
//! a reason rather than stopping the walk, which does not need what the map
//! says to find the next item.

use std::io::{BufRead, Read};

use super::items::ContentFailure;
use super::numbers::with_digit;
use super::records::Records;
use super::{ListSize, Stop};

/// 256 MiB divided by 512, plus one: GNU tar's raw hole detection gives at
/// most one data block per 512 bytes and a last empty one. At 16 bytes each,
/// the blocks of one file take about 8 MiB at most.
const MAX_DATA_BLOCKS: usize = 256 * 1024 * 1024 / 512 + 1;

#[derive(Clone, Copy, Debug)]
pub(super) struct DataBlock {
    offset: u64,
    length: u64,
}

/// Data blocks being read. Holds at most [`MAX_DATA_BLOCKS`], so that a crafted
/// map cannot use up memory; the map is still read to its end by the caller.
#[derive(Default)]
pub(super) struct DataBlocks {
    blocks: Vec<DataBlock>,
    trouble: Option<String>,
}

impl DataBlocks {
    /// Adds a block. `None` is an offset or length that is not a number.
    pub(super) fn add(&mut self, offset: Option<u64>, length: Option<u64>) {
        if self.trouble.is_some() {
            return;
        }
        let (Some(offset), Some(length)) = (offset, length) else {
            self.note("a sparse map holds something that is not a number");
            return;
        };
        if self.blocks.len() == MAX_DATA_BLOCKS {
            self.note(&too_many_blocks());
            return;
        }
        // Doubles as `push` would, without ever going past the maximum.
        if self.blocks.len() == self.blocks.capacity() {
            let room = MAX_DATA_BLOCKS - self.blocks.len();
            self.blocks
                .reserve_exact(self.blocks.capacity().max(4).min(room));
        }
        self.blocks.push(DataBlock { offset, length });
    }

    fn note(&mut self, trouble: &str) {
        self.trouble.get_or_insert_with(|| trouble.to_string());
    }

    pub(super) fn finish(self) -> Result<Vec<DataBlock>, String> {
        match self.trouble {
            Some(trouble) => Err(trouble),
            None => Ok(self.blocks),
        }
    }

    fn len(&self) -> usize {
        self.blocks.len()
    }
}

fn too_many_blocks() -> String {
    format!("a sparse map has more than {MAX_DATA_BLOCKS} data blocks")
}

/// How a sparse file's stored data is arranged.
pub(super) enum SparseMap {
    /// The map came from the headers: the old format, and PAX 0.0 and 0.1.
    Listed {
        real_size: u64,
        blocks: Vec<DataBlock>,
    },
    /// PAX 1.0, where the map is at the start of the item's data.
    AtStartOfData { real_size: u64 },
}

/// A number record: not there, there with a number, or there with something else.
#[derive(Clone, Copy, Default)]
enum NumberRecord {
    #[default]
    Absent,
    Number(u64),
    NotANumber,
}

impl NumberRecord {
    fn from_value(value: Option<u64>) -> Self {
        value.map_or(Self::NotANumber, Self::Number)
    }

    fn is_present(self) -> bool {
        !matches!(self, Self::Absent)
    }

    fn require(self, keyword: &str) -> Result<u64, String> {
        match self {
            Self::Number(number) => Ok(number),
            Self::NotANumber => Err(format!("the GNU.sparse.{keyword} record is not a number")),
            Self::Absent => Err(format!("the GNU.sparse.{keyword} record is missing")),
        }
    }
}

/// The `GNU.sparse.` records of one extended header.
#[derive(Default)]
pub(super) struct SparseRecords {
    present: bool,
    name: Option<Vec<u8>>,
    size: NumberRecord,
    numblocks: NumberRecord,
    realsize: NumberRecord,
    major: NumberRecord,
    minor: NumberRecord,
    /// PAX 0.1: a later `GNU.sparse.map` replaces an earlier one.
    map: Option<DataBlocks>,
    /// PAX 0.0: `offset` and `numbytes` records add up.
    pairs: Option<DataBlocks>,
    /// A 0.0 `offset`, waiting for its `numbytes`.
    open_offset: Option<Option<u64>>,
    /// The first thing that makes the records unsettled.
    trouble: Option<String>,
}

impl SparseRecords {
    pub(super) fn present(&self) -> bool {
        self.present
    }

    pub(super) fn name_len(&self) -> u64 {
        self.name.as_ref().map_or(0, |name| name.len() as u64)
    }

    /// Takes in one record, whose keyword started with `GNU.sparse.`.
    pub(super) fn take_record(
        &mut self,
        keyword: &[u8],
        records: &mut Records<'_, impl BufRead>,
        size: &mut ListSize,
    ) -> Result<(), Stop> {
        self.present = true;
        match keyword {
            b"name" => {
                if let Some(name) = self.name.take() {
                    size.sub(name.len() as u64);
                }
                size.add(records.value_len())?;
                self.name = Some(records.read_value()?);
            }
            b"size" => self.size = NumberRecord::from_value(records.read_decimal()?),
            b"numblocks" => self.numblocks = NumberRecord::from_value(records.read_decimal()?),
            b"realsize" => self.realsize = NumberRecord::from_value(records.read_decimal()?),
            b"major" => self.major = NumberRecord::from_value(records.read_decimal()?),
            b"minor" => self.minor = NumberRecord::from_value(records.read_decimal()?),
            b"offset" => {
                let offset = records.read_decimal()?;
                if self.open_offset.replace(offset).is_some() {
                    self.note("a GNU.sparse.offset record has no GNU.sparse.numbytes after it");
                }
            }
            b"numbytes" => {
                let length = records.read_decimal()?;
                match self.open_offset.take() {
                    Some(offset) => self.pairs.get_or_insert_default().add(offset, length),
                    None => {
                        self.note(
                            "a GNU.sparse.numbytes record has no GNU.sparse.offset before it",
                        );
                    }
                }
            }
            b"map" => self.map = Some(read_map_record(records)?),
            _ => self.note("the GNU.sparse. records include one that is not known"),
        }
        Ok(())
    }

    fn note(&mut self, trouble: &str) {
        self.trouble.get_or_insert_with(|| trouble.to_string());
    }

    /// Fails when the real name is missing or empty where the header's name
    /// is only a stand-in, or empty anywhere: it comes before every other
    /// name, so an empty one would leave the item with no name.
    pub(super) fn check_name(&self) -> Result<(), Stop> {
        let header_name_is_a_stand_in = self.map.is_some()
            || self.major.is_present()
            || self.minor.is_present()
            || self.realsize.is_present();
        match &self.name {
            Some(name) if !name.is_empty() => Ok(()),
            _ if header_name_is_a_stand_in => Err(Stop::unreadable(
                "a sparse file in the tar has no real name, only a stand-in",
            )),
            Some(_) => Err(Stop::unreadable(
                "a GNU.sparse.name record in the tar is empty",
            )),
            None => Ok(()),
        }
    }

    pub(super) fn take_name(&mut self) -> Option<Vec<u8>> {
        self.name.take()
    }

    /// The map the records describe, or why they describe none.
    pub(super) fn into_map(self) -> Result<SparseMap, String> {
        if let Some(trouble) = self.trouble {
            return Err(trouble);
        }
        if self.major.is_present() || self.minor.is_present() || self.realsize.is_present() {
            return self.into_version_1_map();
        }
        let real_size = self.size.require("size")?;
        let declared = self.numblocks.require("numblocks")?;
        let blocks = match (self.map, self.pairs) {
            (Some(_), Some(_)) => return Err(mixed_forms()),
            (Some(map), None) => map,
            (None, pairs) => {
                if self.open_offset.is_some() {
                    return Err(
                        "a GNU.sparse.offset record has no GNU.sparse.numbytes after it".into(),
                    );
                }
                pairs.unwrap_or_default()
            }
        };
        if blocks.trouble.is_none() && blocks.len() as u64 != declared {
            return Err("a sparse map does not hold the data blocks it declares".into());
        }
        Ok(SparseMap::Listed {
            real_size,
            blocks: blocks.finish()?,
        })
    }

    fn into_version_1_map(self) -> Result<SparseMap, String> {
        let has_0_x_records = self.map.is_some()
            || self.pairs.is_some()
            || self.open_offset.is_some()
            || self.size.is_present()
            || self.numblocks.is_present();
        if has_0_x_records {
            return Err(mixed_forms());
        }
        if self.major.require("major")? != 1 || self.minor.require("minor")? != 0 {
            return Err("the sparse format's version is not 1.0".into());
        }
        Ok(SparseMap::AtStartOfData {
            real_size: self.realsize.require("realsize")?,
        })
    }
}

fn mixed_forms() -> String {
    "the GNU.sparse. records mix sparse formats".into()
}

/// A PAX 0.1 map: offsets and lengths, separated by commas.
fn read_map_record(records: &mut Records<'_, impl BufRead>) -> Result<DataBlocks, Stop> {
    let mut blocks = DataBlocks::default();
    let mut waiting_offset: Option<Option<u64>> = None;
    records.read_number_list(|number| match waiting_offset.take() {
        Some(offset) => blocks.add(offset, number),
        None => waiting_offset = Some(number),
    })?;
    if waiting_offset.is_some() {
        blocks.note("the GNU.sparse.map record has an offset with no length");
    }
    Ok(blocks)
}

/// Puts a sparse file together from `data_len` bytes of stored data.
pub(super) fn assemble(
    map: Result<SparseMap, String>,
    data: &mut impl BufRead,
    data_len: u64,
    max_content: u64,
) -> Result<Vec<u8>, ContentFailure> {
    let (real_size, blocks, stored_len) = match map.map_err(ContentFailure::document)? {
        SparseMap::Listed { real_size, blocks } => {
            check_real_size(real_size, max_content)?;
            (real_size, blocks, data_len)
        }
        SparseMap::AtStartOfData { real_size } => {
            check_real_size(real_size, max_content)?;
            let (blocks, map_len) = read_map_at_start(data, data_len)?;
            (real_size, blocks, data_len - map_len)
        }
    };
    check_blocks(&blocks, real_size, stored_len).map_err(ContentFailure::document)?;
    let len = usize::try_from(real_size).map_err(|_| too_large(max_content))?;
    let mut file = vec![0; len];
    for block in blocks {
        // Each block ends inside the file, which `check_blocks` has made sure of.
        let start = block.offset as usize;
        data.read_exact(&mut file[start..start + block.length as usize])?;
    }
    Ok(file)
}

fn check_real_size(real_size: u64, max_content: u64) -> Result<(), ContentFailure> {
    if real_size > max_content {
        return Err(too_large(max_content));
    }
    Ok(())
}

fn too_large(max_content: u64) -> ContentFailure {
    ContentFailure::document(format!("the document is larger than {max_content} bytes"))
}

/// Offsets and lengths are at most 2^63 - 1, so sums of two do not overflow,
/// and once each block ends inside the file, neither does the total.
fn check_blocks(blocks: &[DataBlock], real_size: u64, stored_len: u64) -> Result<(), String> {
    let mut previous_end = 0;
    let mut total = 0;
    for block in blocks {
        let end = block.offset + block.length;
        if end > real_size {
            return Err("a data block ends past the sparse file's size".into());
        }
        if block.offset < previous_end {
            return Err("the sparse file's data blocks are out of order or overlap".into());
        }
        previous_end = end;
        total += block.length;
    }
    if total != stored_len {
        return Err("the sparse file's data blocks do not add up to the data stored".into());
    }
    Ok(())
}

/// Reads a PAX 1.0 map: the number of blocks, then each block's offset and
/// length, one decimal number to a line. Returns the blocks and the length
/// of the map with its padding to the next block.
fn read_map_at_start(
    data: &mut impl BufRead,
    data_len: u64,
) -> Result<(Vec<DataBlock>, u64), ContentFailure> {
    let mut read = 0;
    let count = read_map_line(data, &mut read)?;
    if count > MAX_DATA_BLOCKS as u64 {
        return Err(ContentFailure::document(too_many_blocks()));
    }
    let mut blocks = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = read_map_line(data, &mut read)?;
        let length = read_map_line(data, &mut read)?;
        blocks.push(DataBlock { offset, length });
    }
    let map_len = read.div_ceil(512) * 512;
    if map_len > data_len {
        return Err(ContentFailure::document(
            "the sparse map runs past the item's data",
        ));
    }
    let padding = map_len - read;
    std::io::copy(&mut data.take(padding), &mut std::io::sink())?;
    Ok((blocks, map_len))
}

fn read_map_line(data: &mut impl BufRead, read: &mut u64) -> Result<u64, ContentFailure> {
    let not_a_number =
        || ContentFailure::document("the sparse map holds something that is not a number");
    let mut number: Option<u64> = None;
    loop {
        let Some(byte) = data.fill_buf()?.first().copied() else {
            return Err(ContentFailure::document("the sparse map ends early"));
        };
        data.consume(1);
        *read += 1;
        match byte {
            b'0'..=b'9' => {
                number = Some(
                    with_digit(number.unwrap_or(0), 10, u64::from(byte - b'0'))
                        .ok_or_else(not_a_number)?,
                );
            }
            b'\n' => return number.ok_or_else(not_a_number),
            _ => return Err(not_a_number()),
        }
    }
}
