//! GNU sparse files in a tar: files stored without their all-zero parts,
//! as a map of the parts kept (fragments, each an offset and a length) and
//! the file's real size. GNU tar writes four forms and bsdtar one of them:
//!
//! - the old GNU format (type `S`): the map is in the header and in
//!   extension blocks after it (read by the walk in [`super::tar`]);
//! - PAX 0.0: `GNU.sparse.size`, `GNU.sparse.numblocks`, then
//!   `GNU.sparse.offset` / `GNU.sparse.numbytes` pairs;
//! - PAX 0.1: `GNU.sparse.size`, `GNU.sparse.numblocks`,
//!   `GNU.sparse.name` and `GNU.sparse.map` (offsets and lengths separated
//!   by commas);
//! - PAX 1.0 (also bsdtar's): `GNU.sparse.major` 1, `GNU.sparse.minor` 0,
//!   `GNU.sparse.name` and `GNU.sparse.realsize`, with the map at the start
//!   of the data, one decimal number per line, padded to the next block.
//!
//! The file is rebuilt when it is read: the stored data goes to the
//! fragments in map order, one after another, and the rest is zeros. A map
//! that does not describe the stored data (out of order, overlapping, past
//! the real size, or adding up to more or less than what is stored), one
//! with more than [`MAX_FRAGMENTS`] fragments, or records that do not fit
//! one of the forms above fail the document; its data length comes from
//! the header either way, so the walk goes on. A 0.1 or 1.0 entry without
//! its real name makes the archive unreadable instead, since its header
//! name is a stand-in and failing under it would drop the real document
//! from the index unannounced. So does an empty real name in any form: it
//! is used before a PAX `path` or the header's name, and an empty name is
//! no document.

use std::io::{self, BufRead, Read};

use super::tar::{MAX_NUMBER, read_decimal, read_value, skip};
use super::{DocumentReadError, invalid, names_too_large};

/// Upper bound on the fragments of one sparse file: one per 512-byte block
/// of the largest document, plus a closing empty one. The map then takes
/// about 8 MiB at most.
pub(super) const MAX_FRAGMENTS: usize = (super::MAX_DOCUMENT_BYTES / 512) as usize + 1;

/// How a sparse file's data is laid out, once its headers are read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SparseLayout {
    /// The old GNU format and PAX 0.0 / 0.1 give the map in the headers.
    Map {
        real_size: u64,
        fragments: Vec<(u64, u64)>,
    },
    /// PAX 1.0 puts the map at the start of the data.
    MapInData { real_size: u64 },
}

/// A map being read, which stops growing past [`MAX_FRAGMENTS`]: the rest
/// of a crafted map is still read through, to find where the data starts,
/// but not kept.
#[derive(Default)]
pub(super) struct Fragments {
    list: Vec<(u64, u64)>,
    problem: Option<String>,
}

impl Fragments {
    /// Add one fragment; `None` is a field that is not a number.
    pub(super) fn push(&mut self, offset: Option<u64>, length: Option<u64>) {
        if self.problem.is_some() {
            return;
        }
        match (offset, length) {
            (Some(offset), Some(length)) if self.list.len() < MAX_FRAGMENTS => {
                // Grown by doubling as `push` would, but never past the
                // limit, so the list holds at most `MAX_FRAGMENTS` slots.
                if self.list.len() == self.list.capacity() {
                    let room = MAX_FRAGMENTS - self.list.len();
                    self.list
                        .reserve_exact(self.list.capacity().max(4).min(room));
                }
                self.list.push((offset, length));
            }
            (Some(_), Some(_)) => self.problem = Some(too_many_fragments()),
            _ => self.problem = Some("a sparse map entry is not a number".to_string()),
        }
    }

    fn fail(&mut self, problem: &str) {
        self.problem.get_or_insert_with(|| problem.to_string());
    }

    fn len(&self) -> usize {
        self.list.len()
    }

    pub(super) fn finish(self) -> Result<Vec<(u64, u64)>, String> {
        match self.problem {
            Some(problem) => Err(problem),
            None => Ok(self.list),
        }
    }
}

fn too_many_fragments() -> String {
    format!("the sparse map has more than {MAX_FRAGMENTS} fragments")
}

/// The `GNU.sparse.*` records of one PAX header. A number record holds
/// `Some(None)` when its value is not a number.
#[derive(Default)]
pub(super) struct SparseRecords {
    seen: bool,
    name: Option<Vec<u8>>,
    size: Option<Option<u64>>,
    numblocks: Option<Option<u64>>,
    realsize: Option<Option<u64>>,
    major: Option<Option<u64>>,
    minor: Option<Option<u64>>,
    /// PAX 0.1's map; a later record replaces an earlier one.
    map: Option<Fragments>,
    /// PAX 0.0's offset / numbytes pairs, which accumulate.
    pairs: Option<Fragments>,
    /// A 0.0 offset waiting for its numbytes.
    pending_offset: Option<Option<u64>>,
    /// The first record that fits no form.
    problem: Option<String>,
}

impl SparseRecords {
    /// Whether the header had any `GNU.sparse.*` record.
    pub(super) fn any(&self) -> bool {
        self.seen
    }

    pub(super) fn name_len(&self) -> u64 {
        self.name.as_ref().map_or(0, |name| name.len() as u64)
    }

    /// Handle one `GNU.sparse.<key>` record. `name_room` is how many bytes
    /// the real name may take.
    pub(super) fn record(
        &mut self,
        key: &[u8],
        value: &mut io::Take<impl BufRead>,
        name_room: u64,
    ) -> io::Result<()> {
        self.seen = true;
        match key {
            b"name" => {
                // As with `path`, the record replaced goes first.
                self.name = None;
                if value.limit() > name_room {
                    return Err(names_too_large(super::MAX_LISTED_NAME_BYTES));
                }
                self.name = Some(read_value(value)?);
            }
            b"size" => self.size = Some(read_decimal(value)?),
            b"numblocks" => self.numblocks = Some(read_decimal(value)?),
            b"realsize" => self.realsize = Some(read_decimal(value)?),
            b"major" => self.major = Some(read_decimal(value)?),
            b"minor" => self.minor = Some(read_decimal(value)?),
            b"offset" => {
                let offset = read_decimal(value)?;
                if self.pending_offset.replace(offset).is_some() {
                    self.note("a GNU.sparse.offset record has no GNU.sparse.numbytes after it");
                }
            }
            b"numbytes" => {
                let length = read_decimal(value)?;
                match self.pending_offset.take() {
                    Some(offset) => self.pairs.get_or_insert_default().push(offset, length),
                    None => {
                        self.note("a GNU.sparse.numbytes record has no GNU.sparse.offset before it")
                    }
                }
            }
            b"map" => self.map = Some(read_map_record(value)?),
            _ => self.note("an unknown GNU.sparse record"),
        }
        Ok(())
    }

    fn note(&mut self, problem: &str) {
        self.problem.get_or_insert_with(|| problem.to_string());
    }

    /// Fail when the records are those of PAX 0.1 or 1.0 but the real name
    /// is missing or empty, since the header's name is only a stand-in, and
    /// when the real name is empty whatever the records: it comes before a
    /// PAX `path` or the header's name and would leave the entry nameless.
    pub(super) fn check_name(&self) -> io::Result<()> {
        let names_itself = self.map.is_some()
            || self.major.is_some()
            || self.minor.is_some()
            || self.realsize.is_some();
        match &self.name {
            Some(name) if !name.is_empty() => Ok(()),
            _ if names_itself => Err(invalid(
                "a GNU sparse file in the tar has no real name, only a stand-in",
            )),
            Some(_) => Err(invalid(
                "a GNU.sparse.name record in the tar is empty, so the entry's name is not settled",
            )),
            None => Ok(()),
        }
    }

    /// The real name, when the records give one.
    pub(super) fn take_name(&mut self) -> Option<Vec<u8>> {
        self.name.take()
    }

    /// The layout the records describe, or why they describe none.
    pub(super) fn layout(self) -> Result<SparseLayout, String> {
        if let Some(problem) = self.problem {
            return Err(problem);
        }
        let number = |field: Option<Option<u64>>, record: &str| match field {
            Some(Some(value)) => Ok(value),
            Some(None) => Err(format!("the GNU.sparse.{record} record is not a number")),
            None => Err(format!("the GNU.sparse.{record} record is missing")),
        };
        let mixed = || Err("the GNU.sparse records mix sparse formats".to_string());
        if self.major.is_some() || self.minor.is_some() || self.realsize.is_some() {
            if self.map.is_some()
                || self.pairs.is_some()
                || self.pending_offset.is_some()
                || self.size.is_some()
                || self.numblocks.is_some()
            {
                return mixed();
            }
            if number(self.major, "major")? != 1 || number(self.minor, "minor")? != 0 {
                return Err("the GNU sparse format version is not 1.0".to_string());
            }
            let real_size = number(self.realsize, "realsize")?;
            return Ok(SparseLayout::MapInData { real_size });
        }
        let real_size = number(self.size, "size")?;
        let count = number(self.numblocks, "numblocks")?;
        let fragments = match (self.map, self.pairs) {
            (Some(_), Some(_)) => return mixed(),
            (Some(map), None) => map,
            (None, pairs) => {
                if self.pending_offset.is_some() {
                    return Err(
                        "a GNU.sparse.offset record has no GNU.sparse.numbytes after it"
                            .to_string(),
                    );
                }
                pairs.unwrap_or_default()
            }
        };
        if fragments.problem.is_none() && fragments.len() as u64 != count {
            return Err(
                "the sparse map does not hold the number of fragments it declares".to_string(),
            );
        }
        Ok(SparseLayout::Map {
            real_size,
            fragments: fragments.finish()?,
        })
    }
}

/// Read a PAX 0.1 `GNU.sparse.map` value, numbers separated by commas,
/// pairing them as it goes.
fn read_map_record(value: &mut impl BufRead) -> io::Result<Fragments> {
    let mut fragments = Fragments::default();
    let mut number: Option<u64> = None;
    let mut offset: Option<Option<u64>> = None;
    let mut add = |fragments: &mut Fragments, number: Option<u64>| match offset.take() {
        Some(start) => fragments.push(start, number),
        None => offset = Some(number),
    };
    let mut empty = true;
    loop {
        let buf = value.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let read = buf.len();
        for &byte in buf {
            empty = false;
            match byte {
                b'0'..=b'9' => {
                    let digit = u64::from(byte - b'0');
                    number = Some(
                        number
                            .unwrap_or(0)
                            .checked_mul(10)
                            .and_then(|n| n.checked_add(digit))
                            .filter(|&n| n <= MAX_NUMBER)
                            .unwrap_or(u64::MAX),
                    );
                }
                b',' => add(&mut fragments, number.take().filter(|&n| n <= MAX_NUMBER)),
                _ => fragments.fail("the GNU.sparse.map record is not a list of numbers"),
            }
        }
        value.consume(read);
    }
    if !empty {
        add(&mut fragments, number.take().filter(|&n| n <= MAX_NUMBER));
    }
    if offset.is_some() {
        fragments.fail("the GNU.sparse.map record has an offset without a length");
    }
    Ok(fragments)
}

/// Rebuild a sparse file from its layout and its stored data (`data_len`
/// bytes, the PAX 1.0 map included), failing when the map does not
/// describe the data or the file would pass `limit` bytes.
pub(super) fn rebuild(
    layout: SparseLayout,
    mut data: impl BufRead,
    data_len: u64,
    limit: u64,
) -> Result<Vec<u8>, DocumentReadError> {
    let (real_size, fragments, stored) = match layout {
        SparseLayout::Map {
            real_size,
            fragments,
        } => {
            check_size(real_size, limit)?;
            (real_size, fragments, data_len)
        }
        SparseLayout::MapInData { real_size } => {
            check_size(real_size, limit)?;
            let (fragments, map_len) =
                read_map_in_data(&mut data, data_len).map_err(DocumentReadError::Read)?;
            (real_size, fragments, data_len - map_len)
        }
    };
    check_fragments(real_size, &fragments, stored).map_err(DocumentReadError::Read)?;
    let mut file = vec![0; usize::try_from(real_size).expect("capped at the document limit")];
    for (offset, length) in fragments {
        // Checked above: every fragment ends within the file.
        let start = offset as usize;
        data.read_exact(&mut file[start..start + length as usize])
            .map_err(|e| DocumentReadError::Read(e.to_string()))?;
    }
    Ok(file)
}

fn check_size(real_size: u64, limit: u64) -> Result<(), DocumentReadError> {
    if real_size > limit {
        return Err(DocumentReadError::TooLarge { limit });
    }
    Ok(())
}

/// Check that the fragments describe the stored data: each ends within the
/// file, they come in order without overlapping, and they add up to what
/// is stored. Offsets and lengths are at most 2^63 − 1, so their sums do
/// not overflow.
fn check_fragments(real_size: u64, fragments: &[(u64, u64)], stored: u64) -> Result<(), String> {
    let mut previous_end = 0;
    let mut total = 0;
    for &(offset, length) in fragments {
        let end = offset + length;
        if end > real_size {
            return Err("a sparse fragment ends past the file's size".to_string());
        }
        if offset < previous_end {
            return Err("the sparse fragments are out of order or overlap".to_string());
        }
        previous_end = end;
        total += length;
    }
    if total != stored {
        return Err("the sparse fragments do not add up to the data stored".to_string());
    }
    Ok(())
}

/// Read a PAX 1.0 map from the start of the data: the number of fragments,
/// then each offset and length, one decimal number per line, the whole
/// padded to the next block. Returns the fragments and the map's length
/// with its padding.
fn read_map_in_data(
    data: &mut impl BufRead,
    data_len: u64,
) -> Result<(Vec<(u64, u64)>, u64), String> {
    let mut read = 0;
    let count = read_map_line(data, &mut read)?;
    if count > MAX_FRAGMENTS as u64 {
        return Err(too_many_fragments());
    }
    let mut fragments = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = read_map_line(data, &mut read)?;
        let length = read_map_line(data, &mut read)?;
        fragments.push((offset, length));
    }
    let map_len = read.div_ceil(512) * 512;
    if map_len > data_len {
        return Err("the sparse map runs past the entry's data".to_string());
    }
    skip(&mut data.take(map_len - read)).map_err(|e| e.to_string())?;
    Ok((fragments, map_len))
}

/// One decimal number ending in a newline.
fn read_map_line(data: &mut impl BufRead, read: &mut u64) -> Result<u64, String> {
    let bad = || "the sparse map is not a list of numbers".to_string();
    let mut number: Option<u64> = None;
    loop {
        let byte = {
            let buf = data.fill_buf().map_err(|e| e.to_string())?;
            *buf.first()
                .ok_or_else(|| "the sparse map ends early".to_string())?
        };
        data.consume(1);
        *read += 1;
        match byte {
            b'0'..=b'9' => {
                let digit = u64::from(byte - b'0');
                number = Some(
                    number
                        .unwrap_or(0)
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(digit))
                        .filter(|&n| n <= MAX_NUMBER)
                        .ok_or_else(bad)?,
                );
            }
            b'\n' => return number.ok_or_else(bad),
            _ => return Err(bad()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(entries: &[(&str, &str)]) -> SparseRecords {
        let mut records = SparseRecords::default();
        for (key, value) in entries {
            let mut value = value.as_bytes().take(value.len() as u64);
            records
                .record(key.as_bytes(), &mut value, u64::MAX)
                .expect("record");
        }
        records
    }

    fn map(real_size: u64, fragments: &[(u64, u64)]) -> Result<SparseLayout, String> {
        Ok(SparseLayout::Map {
            real_size,
            fragments: fragments.to_vec(),
        })
    }

    #[test]
    fn the_three_pax_forms_give_their_layouts() {
        let v00 = records(&[
            ("size", "65543"),
            ("numblocks", "2"),
            ("offset", "0"),
            ("numbytes", "512"),
            ("offset", "65536"),
            ("numbytes", "7"),
        ]);
        let v01 = records(&[
            ("size", "65543"),
            ("numblocks", "2"),
            ("name", "few.md"),
            ("map", "0,512,65536,7"),
        ]);
        let v10 = records(&[
            ("major", "1"),
            ("minor", "0"),
            ("name", "few.md"),
            ("realsize", "65543"),
        ]);

        assert_eq!(v00.layout(), map(65543, &[(0, 512), (65536, 7)]));
        assert_eq!(v01.layout(), map(65543, &[(0, 512), (65536, 7)]));
        assert_eq!(
            v10.layout(),
            Ok(SparseLayout::MapInData { real_size: 65543 })
        );
    }

    #[test]
    fn later_records_replace_earlier_ones_except_0_0_pairs() {
        let replaced = records(&[
            ("size", "1"),
            ("size", "65536"),
            ("numblocks", "1"),
            ("map", "0,1"),
            ("map", "65536,0"),
            ("name", "x.md"),
        ]);

        assert_eq!(replaced.layout(), map(65536, &[(65536, 0)]));
    }

    #[test]
    fn records_that_fit_no_form_fail_the_document() {
        for entries in [
            // 0.0 pairs out of step.
            &[("size", "1"), ("numblocks", "1"), ("numbytes", "1")][..],
            &[("size", "1"), ("numblocks", "1"), ("offset", "0")],
            &[
                ("size", "1"),
                ("numblocks", "1"),
                ("offset", "0"),
                ("offset", "0"),
                ("numbytes", "1"),
            ],
            // The count does not match the map.
            &[("size", "2"), ("numblocks", "2"), ("map", "0,1")],
            // A version other than 1.0, or 1.0 mixed with 0.x records.
            &[
                ("major", "2"),
                ("minor", "0"),
                ("name", "a"),
                ("realsize", "1"),
            ],
            &[
                ("major", "1"),
                ("minor", "0"),
                ("name", "a"),
                ("realsize", "1"),
                ("numblocks", "0"),
            ],
            // Values that are not numbers.
            &[("size", "+1"), ("numblocks", "0")],
            &[("size", "1"), ("numblocks", "1"), ("map", "0,x")],
            &[("size", "1"), ("numblocks", "1"), ("map", "0,,1")],
            &[("size", "1"), ("numblocks", "1"), ("map", "0,1,2")],
            // Missing records, an unknown record, a name alone.
            &[("numblocks", "0"), ("map", "")],
            &[
                ("size", "0"),
                ("numblocks", "0"),
                ("map", ""),
                ("bogus", "1"),
            ],
            &[("name", "a.md")],
        ] {
            let mut entries = entries.to_vec();
            if entries.iter().any(|(k, _)| *k == "map")
                && !entries.iter().any(|(k, _)| *k == "name")
            {
                entries.push(("name", "a.md"));
            }
            assert!(records(&entries).layout().is_err(), "{entries:?}");
        }
    }

    #[test]
    fn an_empty_0_0_or_0_1_map_with_no_fragments_declared_is_a_file_of_zeros() {
        let v01 = records(&[
            ("size", "8"),
            ("numblocks", "0"),
            ("name", "a"),
            ("map", ""),
        ]);

        assert_eq!(v01.layout(), map(8, &[]));
    }

    #[test]
    fn a_0_1_or_1_0_entry_needs_its_real_name() {
        for entries in [
            &[("size", "1"), ("numblocks", "0"), ("map", "")][..],
            &[("size", "1"), ("numblocks", "0"), ("map", ""), ("name", "")],
            &[("major", "1"), ("minor", "0"), ("realsize", "1")],
            &[("realsize", "1")],
        ] {
            assert!(records(entries).check_name().is_err(), "{entries:?}");
        }
        // 0.0 names nothing: the entry keeps the name it would have anyway.
        let v00 = records(&[("size", "1"), ("numblocks", "0")]);
        assert!(v00.check_name().is_ok());
    }

    #[test]
    fn a_map_stops_growing_past_the_limit() {
        let mut fragments = Fragments::default();
        for i in 0..MAX_FRAGMENTS as u64 {
            fragments.push(Some(i * 1024), Some(512));
        }
        // Full, and holding no more room than the limit's 8 MiB.
        assert_eq!(fragments.list.capacity(), MAX_FRAGMENTS);

        fragments.push(Some(u64::MAX >> 2), Some(512));

        assert_eq!(fragments.list.capacity(), MAX_FRAGMENTS);
        assert_eq!(fragments.finish(), Err(too_many_fragments()));
    }

    #[test]
    fn a_long_0_1_map_past_the_limit_fails() {
        let mut value = String::new();
        for i in 0..=MAX_FRAGMENTS {
            value.push_str(&format!("{},1,", i * 2));
        }
        value.pop();
        let records = records(&[
            ("size", "100"),
            ("numblocks", &(MAX_FRAGMENTS + 1).to_string()),
            ("name", "a.md"),
            ("map", &value),
        ]);

        assert_eq!(records.layout(), Err(too_many_fragments()));
    }

    fn rebuilt(layout: SparseLayout, data: &[u8], limit: u64) -> Result<Vec<u8>, String> {
        rebuild(layout, data, data.len() as u64, limit).map_err(|e| e.to_string())
    }

    #[test]
    fn fragments_get_the_stored_data_in_turn_and_holes_are_zeros() {
        let layout = SparseLayout::Map {
            real_size: 12,
            fragments: vec![(2, 3), (8, 2), (12, 0)],
        };

        assert_eq!(
            rebuilt(layout, b"abcde", 1024).expect("rebuild"),
            b"\0\0abc\0\0\0de\0\0"
        );
    }

    #[test]
    fn a_map_in_the_data_is_read_across_blocks_and_padding_is_skipped() {
        // A map whose lines cross the first block boundary.
        let mut map = b"2\n".to_vec();
        map.extend(format!("{:0>508}\n", 4).as_bytes());
        map.extend(b"2\n600\n1\n");
        let map_len = map.len().div_ceil(512) * 512;
        map.resize(map_len, b'#');
        let mut data = map;
        data.extend(b"xyz");
        let layout = SparseLayout::MapInData { real_size: 601 };

        let file = rebuilt(layout, &data, 1024).expect("rebuild");

        let mut expected = vec![0; 601];
        expected[4..6].copy_from_slice(b"xy");
        expected[600] = b'z';
        assert_eq!(file, expected);
    }

    #[test]
    fn maps_that_do_not_describe_the_data_fail() {
        for (fragments, data) in [
            // Out of order, overlapping, past the size.
            (vec![(8, 2), (2, 3)], &b"abcde"[..]),
            (vec![(2, 3), (4, 2)], b"abcde"),
            (vec![(2, 3), (11, 2)], b"abcde"),
            // More or less data than the fragments take.
            (vec![(2, 3)], b"abcde"),
            (vec![(2, 3), (8, 4)], b"abcde"),
        ] {
            let layout = SparseLayout::Map {
                real_size: 12,
                fragments: fragments.clone(),
            };
            assert!(rebuilt(layout, data, 1024).is_err(), "{fragments:?}");
        }
    }

    #[test]
    fn packed_fragments_whose_lengths_are_not_whole_blocks_are_read_as_packed() {
        let layout = SparseLayout::Map {
            real_size: 2000,
            fragments: vec![(0, 3), (1000, 2)],
        };

        let file = rebuilt(layout, b"abcde", 4096).expect("rebuild");

        assert_eq!(&file[..3], b"abc");
        assert_eq!(&file[1000..1002], b"de");
    }

    #[test]
    fn a_real_size_past_the_limit_fails_before_anything_is_read() {
        let layout = SparseLayout::MapInData { real_size: 2048 };

        let err = rebuilt(layout, b"not a map", 1024).expect_err("too large");

        assert!(err.contains("limit"), "{err}");
    }

    #[test]
    fn maps_in_the_data_that_do_not_parse_fail() {
        for data in [
            &b""[..],
            b"1\n0\n",
            b"x\n",
            b"\n",
            b"1\n0\n+1\n",
            b"99999999999999999999\n",
        ] {
            let layout = SparseLayout::MapInData { real_size: 512 };
            assert!(rebuilt(layout, data, 1024).is_err(), "{data:?}");
        }
        let too_many = format!("{}\n", MAX_FRAGMENTS + 1);
        let layout = SparseLayout::MapInData { real_size: 512 };
        assert_eq!(
            rebuilt(layout, too_many.as_bytes(), 1024),
            Err(too_many_fragments())
        );
    }
}
