//! Tests of the tar reader, through the functions the driver calls.
//!
//! "Real" tests read `tests/fixtures/archives/`, written by the archivers
//! themselves. The others build forms no archiver writes, byte by byte.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::{tar_contents, tar_listing};
use crate::archive_ingest::transfer::{Entry, ListError, ListSize, Position, ReadResult};

const NO_LIMIT: u64 = u64::MAX;
const MAX_CONTENT: u64 = 256 * 1024 * 1024;
/// 256 MiB divided by 512, plus one.
const MAX_DATA_BLOCKS: usize = 524_289;

// ---- building tars ----

fn with_checksum(mut block: Vec<u8>) -> Vec<u8> {
    block[148..156].copy_from_slice(b"        ");
    let sum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    block
}

/// A header block in the GNU format, whose size field says `size`.
fn header(name: &[u8], typeflag: u8, size: u64) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    block[..name.len()].copy_from_slice(name);
    block[100..108].copy_from_slice(b"0000644\0");
    block[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    block[156] = typeflag;
    block[257..265].copy_from_slice(b"ustar  \0");
    with_checksum(block)
}

fn with_size_field(mut block: Vec<u8>, field: &[u8; 12]) -> Vec<u8> {
    block[124..136].copy_from_slice(field);
    with_checksum(block)
}

fn with_magic(mut block: Vec<u8>, magic_and_version: &[u8; 8]) -> Vec<u8> {
    block[257..265].copy_from_slice(magic_and_version);
    with_checksum(block)
}

fn padded(data: &[u8]) -> Vec<u8> {
    let mut bytes = data.to_vec();
    bytes.resize(data.len().div_ceil(512) * 512, 0);
    bytes
}

fn member(name: &[u8], typeflag: u8, data: &[u8]) -> Vec<u8> {
    [header(name, typeflag, data.len() as u64), padded(data)].concat()
}

fn document(name: &[u8], data: &[u8]) -> Vec<u8> {
    member(name, b'0', data)
}

fn long_name(name: &[u8]) -> Vec<u8> {
    member(b"././@LongLink", b'L', &[name, b"\0"].concat())
}

fn extended(records: &[u8]) -> Vec<u8> {
    member(b"PaxHeader", b'x', records)
}

fn global(records: &[u8]) -> Vec<u8> {
    member(b"pax_global_header", b'g', records)
}

/// `parts` and the end-of-archive indicator.
fn tar_of(parts: &[Vec<u8>]) -> Vec<u8> {
    [parts.concat(), vec![0; 1024]].concat()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// One extended header record; the length counts the whole record.
fn record(keyword: &str, value: &[u8]) -> Vec<u8> {
    let rest = keyword.len() + value.len() + 3;
    let mut length = rest;
    while length != rest + length.to_string().len() {
        length = rest + length.to_string().len();
    }
    [
        format!("{length} {keyword}=").as_bytes(),
        value,
        b"\n".as_slice(),
    ]
    .concat()
}

fn records(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    pairs
        .iter()
        .flat_map(|(keyword, value)| record(keyword, value))
        .collect()
}

// ---- reading ----

fn file_holding(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    // The cursor stays at the end, so every test also checks that the reader
    // goes back to the start.
    file.write_all(bytes).unwrap();
    file
}

fn list_file(file: File, gzip: bool, limit: u64) -> Result<Vec<Entry>, ListError> {
    let mut listing = tar_listing(file, gzip);
    let mut size = ListSize::new(limit);
    let mut entries = Vec::new();
    while let Some(entry) = listing.next(&mut size)? {
        entries.push(entry);
    }
    Ok(entries)
}

fn list(bytes: &[u8]) -> Result<Vec<Entry>, ListError> {
    list_file(file_holding(bytes), false, NO_LIMIT)
}

fn list_limited(bytes: &[u8], limit: u64) -> Result<Vec<Entry>, ListError> {
    list_file(file_holding(bytes), false, limit)
}

fn list_gz(bytes: &[u8]) -> Result<Vec<Entry>, ListError> {
    list_file(file_holding(bytes), true, NO_LIMIT)
}

/// What the second pass gave for one position.
#[derive(Debug, PartialEq)]
enum Got {
    Item {
        name: Vec<u8>,
        regular: bool,
        content: Result<Vec<u8>, String>,
    },
    RestUnreadable(String),
}

fn got(result: ReadResult) -> Got {
    match result {
        ReadResult::Entry {
            name,
            regular,
            content,
        } => Got::Item {
            name,
            regular,
            content: content.map_err(|cause| cause.to_string()),
        },
        ReadResult::RestUnreadable(cause) => Got::RestUnreadable(cause.to_string()),
    }
}

fn read_positions(file: File, gzip: bool, max: u64, positions: &[Position]) -> Vec<Got> {
    let mut contents = tar_contents(file, gzip, max).unwrap();
    positions
        .iter()
        .map(|position| got(contents.read(*position)))
        .collect()
}

fn positions_of(entries: &[Entry]) -> Vec<Position> {
    entries.iter().map(|entry| entry.position).collect()
}

/// Lists `bytes` and reads back every item, in order.
fn read_all_limited(bytes: &[u8], gzip: bool, max: u64) -> Vec<Got> {
    let entries = list_file(file_holding(bytes), gzip, NO_LIMIT).unwrap();
    read_positions(file_holding(bytes), gzip, max, &positions_of(&entries))
}

fn read_all(bytes: &[u8]) -> Vec<Got> {
    read_all_limited(bytes, false, MAX_CONTENT)
}

fn ok(name: &str, content: &[u8]) -> Got {
    Got::Item {
        name: name.as_bytes().to_vec(),
        regular: true,
        content: Ok(content.to_vec()),
    }
}

fn names(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| String::from_utf8_lossy(&entry.name).into_owned())
        .collect()
}

#[track_caller]
fn assert_unreadable(bytes: &[u8], reason: &str) {
    assert_unreadable_result(list(bytes), reason);
}

#[track_caller]
fn assert_unreadable_result(result: Result<Vec<Entry>, ListError>, reason: &str) {
    match result {
        Err(ListError::SourceUnreadable(cause)) => {
            assert!(cause.to_string().contains(reason), "{cause}");
        }
        other => panic!("expected an unreadable source, got {other:?}"),
    }
}

#[track_caller]
fn assert_too_large(result: Result<Vec<Entry>, ListError>) {
    assert!(
        matches!(result, Err(ListError::ListSizeExceeded)),
        "{result:?}"
    );
}

#[track_caller]
fn assert_document_fails(item: &Got, reason: &str) {
    match item {
        Got::Item {
            content: Err(cause),
            ..
        } => assert!(cause.contains(reason), "{cause}"),
        other => panic!("expected a document that cannot be read, got {other:?}"),
    }
}

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archives")
        .join(name)
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap()
}

fn list_fixture(name: &str) -> Result<Vec<Entry>, ListError> {
    list_file(File::open(fixture_path(name)).unwrap(), true, NO_LIMIT)
}

/// The regular items named `*.md`, with their content, in archive order.
fn fixture_documents(name: &str) -> Vec<Got> {
    let entries = list_fixture(name).unwrap();
    let wanted: Vec<Position> = entries
        .iter()
        .filter(|entry| entry.regular && entry.name.ends_with(b".md"))
        .map(|entry| entry.position)
        .collect();
    let file = File::open(fixture_path(name)).unwrap();
    read_positions(file, true, MAX_CONTENT, &wanted)
}

#[track_caller]
fn assert_not_regular(entries: &[Entry], name: &str) {
    let entry = entries
        .iter()
        .find(|entry| entry.name == name.as_bytes())
        .unwrap_or_else(|| panic!("{name} is not in {:?}", names(entries)));
    assert!(!entry.regular, "{name} is listed as a regular file");
}

// ---- the two passes ----

#[test]
fn items_come_in_archive_order_with_growing_positions() {
    let bytes = tar_of(&[
        document(b"b.md", b"second letter"),
        document(b"a.md", &[b'x'; 600]),
        member(b"dir/", b'5', b""),
    ]);

    let entries = list(&bytes).unwrap();

    assert_eq!(names(&entries), ["b.md", "a.md", "dir/"]);
    assert!(entries[0].position < entries[1].position);
    assert!(entries[1].position < entries[2].position);
}

#[test]
fn listing_returns_none_again_after_the_end() {
    let mut listing = tar_listing(file_holding(&tar_of(&[document(b"a.md", b"x")])), false);
    let mut size = ListSize::new(NO_LIMIT);

    assert!(listing.next(&mut size).unwrap().is_some());
    assert!(listing.next(&mut size).unwrap().is_none());
    assert!(listing.next(&mut size).unwrap().is_none());
}

#[test]
fn the_second_pass_reads_only_the_positions_it_is_given() {
    let bytes = tar_of(&[
        document(b"a.md", b"first"),
        document(b"b.md", b"second"),
        document(b"c.md", b"third"),
    ]);
    let entries = list(&bytes).unwrap();

    let read = read_positions(
        file_holding(&bytes),
        false,
        MAX_CONTENT,
        &[entries[1].position],
    );

    assert_eq!(read, [ok("b.md", b"second")]);
}

#[test]
fn a_position_past_a_partly_read_item_is_reached() {
    let bytes = tar_of(&[
        document(b"a.md", &[b'x'; 2000]),
        document(b"b.md", b"second"),
    ]);

    assert_eq!(
        read_all(&bytes),
        [ok("a.md", &[b'x'; 2000]), ok("b.md", b"second")]
    );
}

#[test]
fn an_item_that_is_not_a_regular_file_is_returned_as_such() {
    let bytes = tar_of(&[member(b"link.md", b'2', b"")]);

    let read = read_all(&bytes);

    match &read[0] {
        Got::Item {
            name,
            regular,
            content,
        } => {
            assert_eq!(name, b"link.md");
            assert!(!regular);
            assert!(content.is_err());
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_second_pass_returns_what_it_finds_at_the_position_when_the_file_changed() {
    let listed = tar_of(&[document(b"a.md", b"old")]);
    let entries = list(&listed).unwrap();
    let rewritten = tar_of(&[document(b"z.md", b"new")]);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert_eq!(read, [ok("z.md", b"new")]);
}

#[test]
fn a_position_with_no_item_means_the_rest_cannot_be_read() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    // b.md's header is at another block once the first item is longer.
    let rewritten = tar_of(&[document(b"a.md", &[b'x'; 2000]), document(b"b.md", b"y")]);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &[entries[1].position],
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_position_after_the_end_of_the_tar_means_the_rest_cannot_be_read() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    let rewritten = tar_of(&[document(b"a.md", b"x")]);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &[entries[1].position],
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_document_over_the_content_limit_cannot_be_read_and_the_next_one_can() {
    let bytes = tar_of(&[document(b"big.md", &[b'x'; 11]), document(b"b.md", b"y")]);

    let read = read_all_limited(&bytes, false, 10);

    assert_document_fails(&read[0], "larger than");
    assert_eq!(read[1], ok("b.md", b"y"));
}

#[test]
fn a_document_exactly_at_the_content_limit_is_read() {
    let bytes = tar_of(&[document(b"a.md", &[b'x'; 10])]);

    assert_eq!(
        read_all_limited(&bytes, false, 10),
        [ok("a.md", &[b'x'; 10])]
    );
}

#[test]
fn data_that_runs_past_the_end_of_the_file_on_the_second_pass_stops_the_rest() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    // The same length, but a.md now says its data goes past the file's end.
    let mut rewritten = header(b"a.md", b'0', 1 << 20);
    rewritten.resize(listed.len(), 0);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_document_too_large_to_read_that_also_passes_the_end_stops_the_next_position() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    let mut rewritten = header(b"a.md", b'0', 1 << 32);
    rewritten.resize(listed.len(), 0);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert_document_fails(&read[0], "larger than");
    assert!(matches!(&read[1], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn data_cut_short_before_a_later_position_stops_the_rest() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    let rewritten = header(b"a.md", b'0', 1 << 32);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &[entries[1].position],
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_name_past_the_list_limit_on_the_second_pass_stops_the_rest() {
    let listed = tar_of(&[extended(&record("path", b"a.md")), document(b"x.md", b"x")]);
    let entries = list(&listed).unwrap();
    // The record says it holds more than 256 MiB of name; none of it is there.
    let start = b"268435500 path=aaaa".to_vec();
    let rewritten = tar_of(&[
        [header(b"PaxHeader", b'x', 268_435_500), padded(&start)].concat(),
        document(b"x.md", b"x"),
    ]);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(
        matches!(&read[0], Got::RestUnreadable(cause) if cause.contains("list")),
        "{read:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_go_back_to_its_start_is_unreadable() {
    use std::os::fd::OwnedFd;

    let pipe_end = || {
        let (reader, _writer) = std::io::pipe().unwrap();
        File::from(OwnedFd::from(reader))
    };

    assert!(matches!(
        list_file(pipe_end(), false, NO_LIMIT),
        Err(ListError::SourceUnreadable(_))
    ));
    assert!(tar_contents(pipe_end(), false, MAX_CONTENT).is_err());
}

#[test]
fn a_broken_header_at_the_position_read_stops_the_rest() {
    let listed = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    let entries = list(&listed).unwrap();
    let mut rewritten = listed.clone();
    rewritten[0] ^= 0xff;

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(matches!(&read[0], Got::RestUnreadable(cause) if cause.contains("checksum")));
}

#[test]
fn a_broken_header_before_the_position_read_stops_the_rest() {
    let listed = tar_of(&[
        member(b"dir/", b'5', b""),
        document(b"a.md", b"x"),
        document(b"b.md", b"y"),
    ]);
    let entries = list(&listed).unwrap();
    let mut rewritten = listed.clone();
    rewritten[0] ^= 0xff;

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &[entries[1].position],
    );

    assert!(matches!(&read[0], Got::RestUnreadable(cause) if cause.contains("checksum")));
}

#[test]
fn a_broken_extended_header_on_the_second_pass_stops_the_rest() {
    let listed = tar_of(&[extended(&record("path", b"a.md")), document(b"x.md", b"x")]);
    let entries = list(&listed).unwrap();
    let rewritten = tar_of(&[extended(b"9 no equals\n"), document(b"x.md", b"x")]);

    let read = read_positions(
        file_holding(&rewritten),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(matches!(&read[0], Got::RestUnreadable(cause) if cause.contains("malformed")));
}

#[test]
fn names_are_returned_byte_for_byte() {
    let bytes = tar_of(&[
        document(b"docs\\a.md", b"x"),
        document(b"./docs//a.md", b"y"),
        document(b"latin1-\xe9.md", b"z"),
    ]);

    let entries = list(&bytes).unwrap();

    assert_eq!(entries[0].name, b"docs\\a.md");
    assert_eq!(entries[1].name, b"./docs//a.md");
    assert_eq!(entries[2].name, b"latin1-\xe9.md");
}

#[test]
fn items_with_the_same_name_are_all_returned_in_order() {
    let bytes = tar_of(&[document(b"a.md", b"first"), document(b"a.md", b"second")]);

    assert_eq!(
        read_all(&bytes),
        [ok("a.md", b"first"), ok("a.md", b"second")]
    );
}

// ---- the ends of a tar ----

#[test]
fn a_tar_with_no_bytes_is_unreadable() {
    assert_unreadable(b"", "end-of-archive");
}

#[test]
fn a_tar_that_ends_at_an_item_boundary_is_unreadable() {
    assert_unreadable(&document(b"a.md", b"x"), "end-of-archive");
}

#[test]
fn a_tar_that_ends_after_one_zero_block_is_unreadable() {
    let bytes = [document(b"a.md", b"x"), vec![0; 512]].concat();

    assert_unreadable(&bytes, "end-of-archive");
}

#[test]
fn a_tar_that_ends_inside_a_block_is_unreadable() {
    let bytes = tar_of(&[document(b"a.md", b"x")]);

    // Part of the way through the block after a.md's data.
    assert_unreadable(&bytes[..1100], "end-of-archive");
}

#[test]
fn a_zero_block_followed_by_an_item_is_unreadable() {
    let bytes = [
        document(b"a.md", b"x"),
        vec![0; 512],
        document(b"b.md", b"y"),
        vec![0; 1024],
    ]
    .concat();

    assert_unreadable(&bytes, "zero block");
}

#[test]
fn a_tar_that_ends_inside_an_items_data_is_unreadable() {
    let bytes = [header(b"a.md", b'0', 4096), vec![b'x'; 512]].concat();

    assert_unreadable(&bytes, "ends");
}

#[test]
fn items_after_the_end_of_archive_indicator_are_not_read() {
    let first = tar_of(&[document(b"a.md", b"x")]);
    for after in [
        tar_of(&[document(b"b.md", b"y")]),
        b"not a tar header".repeat(64),
    ] {
        let bytes = [first.clone(), after].concat();

        assert_eq!(names(&list(&bytes).unwrap()), ["a.md"]);
    }
}

// ---- headers and number fields ----

#[test]
fn a_checksum_summed_as_signed_bytes_is_accepted() {
    // A byte over 0x7f makes the signed and unsigned sums differ.
    let mut block = header(b"a\xe9.md", b'0', 0);
    block[148..156].copy_from_slice(b"        ");
    let signed: i64 = block.iter().map(|byte| i64::from(*byte as i8)).sum();
    block[148..156].copy_from_slice(format!("{signed:06o}\0 ").as_bytes());

    assert_eq!(list(&tar_of(&[block])).unwrap()[0].name, b"a\xe9.md");
}

#[test]
fn a_checksum_matching_neither_sum_is_unreadable() {
    let mut block = header(b"a.md", b'0', 0);
    block[148] = b'7';

    assert_unreadable(&tar_of(&[block]), "checksum");
}

#[test]
fn a_checksum_field_that_is_not_octal_is_unreadable() {
    let mut block = header(b"a.md", b'0', 0);
    block[148..156].copy_from_slice(b"zzzzzzz\0");

    assert_unreadable(&tar_of(&[block]), "checksum");
}

#[test]
fn seven_digit_checksums_are_accepted() {
    let mut block = header(b"a.md", b'0', 0);
    block[148..156].copy_from_slice(b"        ");
    let sum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
    block[148..156].copy_from_slice(format!("{sum:07o}\0").as_bytes());

    assert_eq!(names(&list(&tar_of(&[block])).unwrap()), ["a.md"]);
}

#[test]
fn size_fields_are_read_however_tools_write_them() {
    for field in [
        b"00000000005\0",
        b"00000000005 ",
        b"          5\0",
        b"   5 \0 \0\0 \0 ",
    ] {
        let block = with_size_field(header(b"a.md", b'0', 0), field);
        let bytes = tar_of(&[block, padded(b"12345")]);

        assert_eq!(read_all(&bytes), [ok("a.md", b"12345")], "{field:?}");
    }
}

#[test]
fn size_fields_that_are_not_numbers_are_unreadable() {
    for field in [
        // No digits at all.
        b"\0\0\0\0\0\0\0\0\0\0\0\0",
        b"            ",
        // Characters that are not octal digits.
        b"0000000000z\0",
        b"00000000008\0",
        b"-0000000004\0",
        b"00 12\0\0\0\0\0\0\0",
        // 2^63 in base-256, one past the largest number.
        b"\x80\0\0\0\x80\0\0\0\0\0\0\0",
    ] {
        let block = with_size_field(header(b"a.md", b'0', 0), field);

        assert_unreadable(&tar_of(&[block]), "size");
    }
}

#[test]
fn a_base_256_size_up_to_2_63_minus_1_is_a_number() {
    let mut field = [0u8; 12];
    field[0] = 0x80;
    field[4..].copy_from_slice(&5u64.to_be_bytes());
    let block = with_size_field(header(b"a.md", b'0', 0), &field);

    assert_eq!(
        read_all(&tar_of(&[block, padded(b"12345")])),
        [ok("a.md", b"12345")]
    );
}

#[test]
fn a_base_256_size_of_2_63_minus_1_is_accepted_as_a_number() {
    let mut field = [0u8; 12];
    field[0] = 0x80;
    field[4..].copy_from_slice(&((1u64 << 63) - 1).to_be_bytes());
    let block = with_size_field(header(b"a.md", b'0', 0), &field);

    // The size is a number, so the tar is unreadable only for running out.
    assert_unreadable(&tar_of(&[block]), "ends");
}

// ---- typeflags ----

#[test]
fn typeflags_0_nul_and_7_are_regular_files() {
    for typeflag in [b'0', 0, b'7'] {
        let bytes = tar_of(&[member(b"a.md", typeflag, b"text")]);

        assert_eq!(read_all(&bytes), [ok("a.md", b"text")], "{typeflag}");
    }
}

#[test]
fn typeflags_2_to_6_hold_no_data_whatever_their_size_says() {
    for typeflag in b'2'..=b'6' {
        for odd in [
            header(b"odd.md", typeflag, 5),
            with_size_field(header(b"odd.md", typeflag, 0), b"zzzzzzzzzzz\0"),
        ] {
            let bytes = tar_of(&[odd, document(b"a.md", b"text")]);

            let entries = list(&bytes).unwrap();

            assert_eq!(names(&entries), ["odd.md", "a.md"]);
            assert!(!entries[0].regular);
        }
    }
}

#[test]
fn an_extended_size_on_typeflags_2_to_6_is_ignored() {
    let bytes = tar_of(&[
        extended(&record("size", b"9999")),
        header(b"dir/", b'5', 0),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["dir/", "a.md"]);
}

#[test]
fn a_hard_link_with_no_data_is_read_past_and_is_not_regular() {
    let bytes = tar_of(&[header(b"hard.md", b'1', 0), document(b"a.md", b"text")]);

    let entries = list(&bytes).unwrap();

    assert_eq!(names(&entries), ["hard.md", "a.md"]);
    assert!(!entries[0].regular);
}

#[test]
fn a_hard_link_with_data_is_unreadable() {
    let in_header = tar_of(&[member(b"hard.md", b'1', b"data")]);
    let in_extended = tar_of(&[
        extended(&record("size", b"4")),
        header(b"hard.md", b'1', 0),
        padded(b"data"),
    ]);

    assert_unreadable(&in_header, "hard link");
    assert_unreadable(&in_extended, "hard link");
}

#[test]
fn a_hard_link_whose_size_is_not_a_number_is_unreadable() {
    let block = with_size_field(header(b"hard.md", b'1', 0), b"zzzzzzzzzzz\0");

    assert_unreadable(&tar_of(&[block]), "size");
}

#[test]
fn other_typeflags_are_stepped_over_by_their_size_and_are_not_regular() {
    let bytes = tar_of(&[
        member(b"dir.md", b'D', b"Ya.md\0\0"),
        document(b"a.md", b"text"),
    ]);

    let entries = list(&bytes).unwrap();

    assert_eq!(names(&entries), ["dir.md", "a.md"]);
    assert!(!entries[0].regular);
}

#[test]
fn a_regular_item_whose_size_is_not_a_number_is_unreadable() {
    let block = with_size_field(header(b"a.md", b'0', 0), b"zzzzzzzzzzz\0");

    assert_unreadable(&tar_of(&[block]), "size");
}

#[test]
fn a_prefix_header_whose_size_is_not_a_number_is_unreadable() {
    for typeflag in [b'L', b'K', b'x', b'X', b'g'] {
        let block = with_size_field(header(b"x", typeflag, 0), b"zzzzzzzzzzz\0");

        assert_unreadable(&tar_of(&[block]), "size");
    }
}

// ---- names ----

#[test]
fn a_ustar_header_joins_its_prefix_to_its_name() {
    let mut block = with_magic(header(b"a.md", b'0', 0), b"ustar\x0000");
    block[345..349].copy_from_slice(b"docs");

    let entries = list(&tar_of(&[with_checksum(block)])).unwrap();

    assert_eq!(entries[0].name, b"docs/a.md");
}

#[test]
fn a_ustar_header_with_an_empty_prefix_has_just_its_name() {
    let block = with_magic(header(b"a.md", b'0', 0), b"ustar\x0000");

    assert_eq!(list(&tar_of(&[block])).unwrap()[0].name, b"a.md");
}

#[test]
fn the_prefix_is_not_used_unless_the_magic_and_version_are_ustars() {
    for magic in [b"ustar\0  ", b"ustar  \0", b"\0\0\0\0\0\0\0\0"] {
        let mut block = with_magic(header(b"a.md", b'0', 0), magic);
        block[345..349].copy_from_slice(b"docs");

        let entries = list(&tar_of(&[with_checksum(block)])).unwrap();

        assert_eq!(entries[0].name, b"a.md", "{magic:?}");
    }
}

#[test]
fn a_long_name_names_the_next_item() {
    let long = format!("{}/doc.md", "d".repeat(120));
    let bytes = tar_of(&[long_name(long.as_bytes()), document(b"short.md", b"text")]);

    assert_eq!(read_all(&bytes), [ok(&long, b"text")]);
}

#[test]
fn a_long_name_stops_at_its_first_nul() {
    let bytes = tar_of(&[
        member(b"././@LongLink", b'L', b"name.md\0garbage"),
        document(b"short.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["name.md"]);
}

#[test]
fn a_long_name_with_no_nul_runs_to_the_end_of_its_data() {
    let bytes = tar_of(&[
        member(b"././@LongLink", b'L', b"name.md"),
        document(b"short.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["name.md"]);
}

#[test]
fn an_empty_long_name_gives_an_empty_name() {
    let bytes = tar_of(&[long_name(b""), document(b"a.md", b"text")]);

    assert_eq!(list(&bytes).unwrap()[0].name, b"");
}

#[test]
fn a_long_link_name_is_read_past_and_does_not_rename_the_item() {
    let bytes = tar_of(&[
        member(b"././@LongLink", b'K', b"target\0"),
        header(b"link.md", b'2', 0),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["link.md", "a.md"]);
}

#[test]
fn an_extended_path_names_the_next_item_only() {
    let bytes = tar_of(&[
        extended(&record("path", b"pax/long.md")),
        document(b"short.md", b"from pax"),
        document(b"plain.md", b"plain"),
    ]);

    assert_eq!(
        read_all(&bytes),
        [ok("pax/long.md", b"from pax"), ok("plain.md", b"plain")]
    );
}

#[test]
fn solaris_typeflag_x_is_an_extended_header_too() {
    let bytes = tar_of(&[
        member(b"PaxHeader", b'X', &record("path", b"pax.md")),
        document(b"short.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["pax.md"]);
}

#[test]
fn prefix_headers_are_recognised_by_typeflag_whatever_the_magic() {
    let block = with_magic(header(b"PaxHeader", b'x', 11), b"\0\0\0\0\0\0\0\0");
    let bytes = tar_of(&[
        [block, padded(&record("path", b"a"))].concat(),
        document(b"short.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["a"]);
}

#[test]
fn the_sparse_name_comes_before_the_path_and_the_header_name() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("path", b"from-path.md"),
            ("GNU.sparse.name", b"real.md"),
            ("GNU.sparse.size", b"4"),
            ("GNU.sparse.numblocks", b"1"),
            ("GNU.sparse.offset", b"0"),
            ("GNU.sparse.numbytes", b"4"),
        ])),
        document(b"header.md", b"text"),
    ]);

    assert_eq!(read_all(&bytes), [ok("real.md", b"text")]);
}

#[test]
fn the_sparse_name_comes_before_a_long_name() {
    let bytes = tar_of(&[
        long_name(b"from-long-name.md"),
        extended(&records(&[
            ("GNU.sparse.name", b"real.md"),
            ("GNU.sparse.size", b"4"),
            ("GNU.sparse.numblocks", b"1"),
            ("GNU.sparse.offset", b"0"),
            ("GNU.sparse.numbytes", b"4"),
        ])),
        document(b"header.md", b"text"),
    ]);

    assert_eq!(read_all(&bytes), [ok("real.md", b"text")]);
}

#[test]
fn two_prefix_headers_of_the_same_kind_are_unreadable() {
    for (first, second) in [
        (long_name(b"x.md"), long_name(b"y.md")),
        (member(b"k", b'K', b"x\0"), member(b"k", b'K', b"y\0")),
        (
            extended(&record("size", b"0")),
            extended(&record("size", b"0")),
        ),
        (
            extended(&record("size", b"0")),
            member(b"X", b'X', &record("size", b"0")),
        ),
    ] {
        let bytes = tar_of(&[first, second, document(b"a.md", b"x")]);

        assert_unreadable(&bytes, "same kind");
    }
}

#[test]
fn a_long_name_with_an_extended_path_is_unreadable() {
    let bytes = tar_of(&[
        long_name(b"x.md"),
        extended(&record("path", b"y.md")),
        document(b"a.md", b"x"),
    ]);

    assert_unreadable(&bytes, "both");
}

#[test]
fn prefix_headers_with_no_item_before_the_end_are_dropped() {
    for prefix in [
        long_name(b"x.md"),
        member(b"k", b'K', b"x\0"),
        extended(&record("path", b"x.md")),
    ] {
        let bytes = tar_of(&[document(b"a.md", b"text"), prefix]);

        assert_eq!(names(&list(&bytes).unwrap()), ["a.md"]);
    }
}

// ---- extended header records ----

#[test]
fn records_are_split_by_their_lengths_not_at_newlines() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("SCHILY.xattr.bin", b"a\nb\n"),
            ("path", b"docs/a.md"),
        ])),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["docs/a.md"]);
}

#[test]
fn nul_bytes_after_the_last_record_are_accepted() {
    let bytes = tar_of(&[
        extended(&[record("path", b"docs/a.md"), vec![0; 12]].concat()),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["docs/a.md"]);
}

#[test]
fn a_nul_inside_a_path_value_is_part_of_the_name() {
    let bytes = tar_of(&[
        extended(&record("path", b"b\0c.md")),
        document(b"b.md", b"x"),
    ]);

    assert_eq!(list(&bytes).unwrap()[0].name, b"b\0c.md");
}

#[test]
fn a_nul_inside_a_sparse_name_value_is_part_of_the_name() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("GNU.sparse.size", b"0"),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.name", b"b\0c.md"),
            ("GNU.sparse.map", b""),
        ])),
        document(b"stand-in.md", b""),
    ]);

    assert_eq!(list(&bytes).unwrap()[0].name, b"b\0c.md");
}

#[test]
fn the_last_of_repeated_keywords_is_used() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("path", b"first.md"),
            ("size", b"9999"),
            ("path", b"docs/a.md"),
            ("size", b"4"),
        ])),
        header(b"a.md", b'0', 0),
        padded(b"text"),
    ]);

    assert_eq!(read_all(&bytes), [ok("docs/a.md", b"text")]);
}

#[test]
fn an_extended_size_decides_where_the_data_ends() {
    let bytes = tar_of(&[
        extended(&record("size", b"600")),
        header(b"big.md", b'0', 0),
        padded(&[b'a'; 600]),
        document(b"after.md", b"after"),
    ]);

    assert_eq!(
        read_all(&bytes),
        [ok("big.md", &[b'a'; 600]), ok("after.md", b"after")]
    );
}

#[test]
fn malformed_records_are_unreadable() {
    for records in [
        // The length runs past the end of the extended header.
        &b"99 path=a.md\n"[..],
        // No `=`.
        b"13 path a.md\n",
        // The last byte is not a newline.
        b"13 path=a.md!",
        // An empty keyword.
        b"8 =a.md\n",
        // A NUL in the keyword.
        b"10 pa\0h=a\n",
        // Not a decimal length.
        b"+14 path=a.md\n",
        b" 13 path=a.md\n",
        b"13path=a.md\n",
        b"x3 path=a.md\n",
        // Shorter than the shortest record.
        b"4 k=\n",
        b"0 \n",
        // A length past 2^63 - 1.
        b"18446744073709551616 path=x\n",
        // NUL bytes ahead of a record.
        b"\0\0\0\x0010 path=x\n",
    ] {
        let bytes = tar_of(&[extended(records), document(b"a.md", b"x")]);

        assert_unreadable(&bytes, "malformed");
    }
}

#[test]
fn a_malformed_record_after_one_that_is_not_used_is_unreadable() {
    let bytes = tar_of(&[
        extended(&[record("comment", b"fine"), b"9 x y z\n".to_vec()].concat()),
        document(b"a.md", b"x"),
    ]);

    assert_unreadable(&bytes, "malformed");
}

#[test]
fn the_shortest_record_is_accepted() {
    let bytes = tar_of(&[extended(b"5 k=\n"), document(b"a.md", b"x")]);

    assert_eq!(names(&list(&bytes).unwrap()), ["a.md"]);
}

#[test]
fn records_with_keywords_past_4_kib_are_read_past() {
    let long_keyword = "k".repeat(4097);
    let bytes = tar_of(&[
        extended(&records(&[
            (long_keyword.as_str(), b"v"),
            ("path", b"docs/a.md"),
        ])),
        document(b"a.md", b"x"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["docs/a.md"]);
}

#[test]
fn a_record_with_a_keyword_of_exactly_4_kib_is_still_looked_at() {
    let keyword = format!("GNU.sparse.{}", "k".repeat(4096 - 11));
    let bytes = tar_of(&[extended(&record(&keyword, b"v")), document(b"a.md", b"x")]);

    let read = read_all(&bytes);

    assert_document_fails(&read[0], "GNU.sparse");
}

#[test]
fn an_extended_size_that_is_not_a_decimal_is_unreadable() {
    for value in [
        &b"abc"[..],
        b"+4",
        b"-4",
        b"4 ",
        b"9223372036854775808",
        b"18446744073709551616",
    ] {
        let bytes = tar_of(&[extended(&record("size", value)), document(b"a.md", b"x")]);

        assert_unreadable(&bytes, "size");
    }
}

#[test]
fn an_extended_size_of_2_63_minus_1_is_a_number() {
    let bytes = tar_of(&[
        extended(&record("size", b"9223372036854775807")),
        document(b"a.md", b"x"),
    ]);

    assert_unreadable(&bytes, "ends");
}

#[test]
fn empty_path_and_size_records_are_unreadable() {
    for keyword in ["path", "size"] {
        let bytes = tar_of(&[extended(&record(keyword, b"")), document(b"a.md", b"x")]);

        assert_unreadable(&bytes, "empty");
    }
}

#[test]
fn a_global_header_with_other_records_is_read_past() {
    let bytes = tar_of(&[
        global(&record("comment", b"0123abcd")),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(read_all(&bytes), [ok("a.md", b"text")]);
}

#[test]
fn a_global_header_with_a_path_size_or_sparse_record_is_unreadable() {
    for (keyword, value) in [
        ("path", &b"global.md"[..]),
        ("path", b""),
        ("size", b"4"),
        ("size", b""),
        ("GNU.sparse.name", b"a.md"),
        ("GNU.sparse.major", b"1"),
    ] {
        let bytes = tar_of(&[global(&record(keyword, value)), document(b"a.md", b"x")]);

        assert_unreadable(&bytes, "global");
    }
}

#[test]
fn a_global_record_with_a_keyword_past_4_kib_is_read_past() {
    let long_keyword = "k".repeat(4097);
    let bytes = tar_of(&[
        global(&record(&long_keyword, b"v")),
        document(b"a.md", b"text"),
    ]);

    assert_eq!(names(&list(&bytes).unwrap()), ["a.md"]);
}

#[test]
fn a_malformed_record_in_a_global_header_is_unreadable() {
    let bytes = tar_of(&[global(b"9 no equals\n"), document(b"a.md", b"x")]);

    assert_unreadable(&bytes, "malformed");
}

#[test]
fn an_extended_header_cut_short_by_the_file_is_unreadable() {
    let bytes = [header(b"PaxHeader", b'x', 4096), record("comment", b"fine")].concat();

    assert_unreadable(&padded(&bytes), "ends");
}

// ---- the size of the list ----

#[test]
fn a_header_name_is_counted_up_to_the_limit() {
    let bytes = tar_of(&[document(b"name.md", b"x")]);

    assert_eq!(names(&list_limited(&bytes, 7).unwrap()), ["name.md"]);
    assert_too_large(list_limited(&bytes, 6));
}

#[test]
fn a_prefixed_header_name_is_counted_with_its_slash() {
    let mut block = with_magic(header(b"a.md", b'0', 0), b"ustar\x0000");
    block[345..349].copy_from_slice(b"docs");
    let bytes = tar_of(&[with_checksum(block)]);

    assert_eq!(names(&list_limited(&bytes, 9).unwrap()), ["docs/a.md"]);
    assert_too_large(list_limited(&bytes, 8));
}

#[test]
fn each_returned_item_leaves_only_its_own_name_in_the_total() {
    let bytes = tar_of(&[
        long_name(b"aaaaaaaa.md"),
        document(b"stand-in.md", b"x"),
        document(b"bb.md", b"y"),
    ]);

    assert_eq!(names(&list_limited(&bytes, 16).unwrap()).len(), 2);
    assert_too_large(list_limited(&bytes, 15));
}

#[test]
fn a_long_name_over_the_limit_stops_before_it_is_read_to_its_end() {
    // The name would run for 4096 bytes, but the file ends after 600 of them:
    // reading to the end of the name would report the file as cut short.
    let bytes = [
        header(b"././@LongLink", b'L', 4096),
        vec![b'n'; 512],
        vec![b'n'; 88],
    ]
    .concat();

    assert_too_large(list_limited(&bytes, 300));
}

#[test]
fn an_extended_path_over_the_limit_is_refused_before_it_is_read() {
    // The record says 4 KiB, but the file holds only its start.
    let start = b"4100 path=aaaaaaaa".to_vec();
    let bytes = [header(b"PaxHeader", b'x', 4100), padded(&start)].concat();

    assert_too_large(list_limited(&bytes, 300));
}

#[test]
fn a_sparse_name_over_the_limit_is_refused_before_it_is_read() {
    let start = b"4100 GNU.sparse.name=aaaaaaaa".to_vec();
    let bytes = [header(b"PaxHeader", b'x', 4100), padded(&start)].concat();

    assert_too_large(list_limited(&bytes, 300));
}

#[test]
fn all_names_held_for_one_item_count_until_its_name_is_settled() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("path", b"aaaaaaaaaa"),
            ("GNU.sparse.name", b"bbbbbb"),
            ("GNU.sparse.size", b"0"),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.map", b""),
        ])),
        document(b"stand-in.md", b""),
    ]);

    assert_eq!(names(&list_limited(&bytes, 16).unwrap()), ["bbbbbb"]);
    assert_too_large(list_limited(&bytes, 15));
}

#[test]
fn names_not_used_leave_the_total_once_the_item_is_settled() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("path", b"aaaaaaaaaa"),
            ("GNU.sparse.name", b"bbbbbb"),
            ("GNU.sparse.size", b"0"),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.map", b""),
        ])),
        document(b"stand-in.md", b""),
        // 10 more bytes fit beside the 6 that remain of the first item.
        document(b"cccccccccc", b""),
    ]);

    assert_eq!(names(&list_limited(&bytes, 16).unwrap()).len(), 2);
}

#[test]
fn a_repeated_path_counts_only_its_last_value() {
    let bytes = tar_of(&[
        extended(&records(&[("path", b"aaaaaaaa"), ("path", b"bbbbbbbb")])),
        document(b"stand-in.md", b""),
    ]);

    assert_eq!(names(&list_limited(&bytes, 8).unwrap()), ["bbbbbbbb"]);
}

#[test]
fn a_repeated_sparse_name_counts_only_its_last_value() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("GNU.sparse.name", b"aaaaaaaa"),
            ("GNU.sparse.name", b"bbbbbbbb"),
            ("GNU.sparse.size", b"0"),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.map", b""),
        ])),
        document(b"stand-in.md", b""),
    ]);

    assert_eq!(names(&list_limited(&bytes, 8).unwrap()), ["bbbbbbbb"]);
}

#[test]
fn a_value_that_does_not_fit_the_list_is_read_past_when_it_is_not_a_name() {
    let bytes = tar_of(&[
        extended(&record("comment", &[b'c'; 4096])),
        document(b"a.md", b"x"),
    ]);

    assert_eq!(names(&list_limited(&bytes, 4).unwrap()), ["a.md"]);
}

// ---- gzip ----

#[test]
fn a_tar_gz_is_read_through_gzip() {
    let bytes = gzip(&tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]));

    assert_eq!(
        read_all_limited(&bytes, true, MAX_CONTENT),
        [ok("a.md", b"x"), ok("b.md", b"y")]
    );
}

#[test]
fn a_tar_gz_made_of_two_members_is_read_through_both() {
    let tar = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    // The first member ends right after a.md's data block.
    let bytes = [gzip(&tar[..1024]), gzip(&tar[1024..])].concat();

    assert_eq!(
        read_all_limited(&bytes, true, MAX_CONTENT),
        [ok("a.md", b"x"), ok("b.md", b"y")]
    );
}

#[test]
fn the_end_of_archive_indicator_may_cross_a_member_boundary() {
    let tar = tar_of(&[document(b"a.md", b"x")]);
    let bytes = [gzip(&tar[..1536]), gzip(&tar[1536..])].concat();

    assert_eq!(names(&list_gz(&bytes).unwrap()), ["a.md"]);
}

#[test]
fn bytes_after_the_member_holding_the_end_of_archive_indicator_are_not_read() {
    let tar = tar_of(&[document(b"a.md", b"x")]);
    for after in [vec![0; 4096], b"not gzip".to_vec()] {
        let bytes = [gzip(&tar), after].concat();

        assert_eq!(names(&list_gz(&bytes).unwrap()), ["a.md"]);
    }
}

#[test]
fn a_wrong_crc_in_the_member_holding_the_end_of_archive_indicator_is_unreadable() {
    // GNU tar pads to 10 KiB, so the indicator is well inside the member.
    let mut tar = tar_of(&[document(b"a.md", b"x")]);
    tar.resize(10240, 0);
    let mut bytes = gzip(&tar);
    let crc = bytes.len() - 8;
    bytes[crc] ^= 0xff;

    assert!(matches!(
        list_gz(&bytes),
        Err(ListError::SourceUnreadable(_))
    ));
}

#[test]
fn a_wrong_crc_in_an_earlier_member_is_unreadable() {
    let tar = tar_of(&[document(b"a.md", b"x")]);
    let mut first = gzip(&tar[..1024]);
    let crc = first.len() - 8;
    first[crc] ^= 0xff;
    let bytes = [first, gzip(&tar[1024..])].concat();

    assert!(matches!(
        list_gz(&bytes),
        Err(ListError::SourceUnreadable(_))
    ));
}

#[test]
fn a_member_cut_short_is_unreadable() {
    let bytes = gzip(&tar_of(&[document(b"a.md", &[b'x'; 4096])]));

    assert!(matches!(
        list_gz(&bytes[..bytes.len() - 4]),
        Err(ListError::SourceUnreadable(_))
    ));
}

#[test]
fn a_tar_gz_with_no_end_of_archive_indicator_is_unreadable() {
    let bytes = gzip(&document(b"a.md", b"x"));

    assert_unreadable_result(list_gz(&bytes), "end-of-archive");
}

#[test]
fn a_tar_gz_that_ends_inside_an_items_data_is_unreadable() {
    let bytes = gzip(&[header(b"a.md", b'0', 4096), vec![b'x'; 512]].concat());

    assert_unreadable_result(list_gz(&bytes), "ends");
}

#[test]
fn a_file_that_is_not_gzip_is_unreadable_as_a_tar_gz() {
    assert!(matches!(
        list_gz(b"this is not gzip at all"),
        Err(ListError::SourceUnreadable(_))
    ));
}

#[test]
fn a_second_pass_over_two_members_can_stop_before_the_last_one() {
    let tar = tar_of(&[document(b"a.md", b"x"), document(b"b.md", b"y")]);
    // The second member is broken, but nothing in it is needed.
    let mut second = gzip(&tar[1024..]);
    second[0] = 0;
    let bytes = [gzip(&tar[..1024]), second].concat();

    let read = read_positions(file_holding(&bytes), true, MAX_CONTENT, &[Position(0)]);

    assert_eq!(read, [ok("a.md", b"x")]);
}

#[test]
fn the_second_pass_does_not_check_the_crc_of_the_member_it_stops_in() {
    let mut tar = tar_of(&[document(b"a.md", b"x")]);
    tar.resize(10240, 0);
    let mut bytes = gzip(&tar);
    let crc = bytes.len() - 8;
    bytes[crc] ^= 0xff;

    assert!(matches!(
        list_gz(&bytes),
        Err(ListError::SourceUnreadable(_))
    ));
    let read = read_positions(file_holding(&bytes), true, MAX_CONTENT, &[Position(0)]);
    assert_eq!(read, [ok("a.md", b"x")]);
}

// ---- sparse files: the old GNU format ----

fn octal12(number: u64) -> [u8; 12] {
    format!("{number:011o}\0").into_bytes().try_into().unwrap()
}

/// A header of typeflag `S` with up to four `struct sparse`.
fn old_sparse_header(
    name: &[u8],
    real: u64,
    slots: &[(u64, u64)],
    flag: u8,
    stored: u64,
) -> Vec<u8> {
    let mut block = header(name, b'S', stored);
    for (i, (offset, length)) in slots.iter().enumerate() {
        let at = 386 + i * 24;
        block[at..at + 12].copy_from_slice(&octal12(*offset));
        block[at + 12..at + 24].copy_from_slice(&octal12(*length));
    }
    block[482] = flag;
    block[483..495].copy_from_slice(&octal12(real));
    with_checksum(block)
}

/// A `struct sparse_header` block with up to 21 `struct sparse`.
fn sparse_header(slots: &[(u64, u64)], flag: u8) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    for (i, (offset, length)) in slots.iter().enumerate() {
        block[i * 24..i * 24 + 12].copy_from_slice(&octal12(*offset));
        block[i * 24 + 12..i * 24 + 24].copy_from_slice(&octal12(*length));
    }
    block[504] = flag;
    block
}

const SLOTS: [(u64, u64); 3] = [(0, 2), (1024, 2), (3000, 1)];

/// "ab" at 0, "cd" at 1024, "e" at 3000, in 3001 bytes.
fn sparse_content() -> Vec<u8> {
    let mut file = vec![0u8; 3001];
    file[..2].copy_from_slice(b"ab");
    file[1024..1026].copy_from_slice(b"cd");
    file[3000] = b'e';
    file
}

#[test]
fn an_old_sparse_file_with_its_map_in_the_header_is_assembled() {
    let bytes = tar_of(&[
        old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5),
        padded(b"abcde"),
        document(b"after.md", b"after"),
    ]);

    assert_eq!(
        read_all(&bytes),
        [ok("s.md", &sparse_content()), ok("after.md", b"after")]
    );
}

#[test]
fn an_old_sparse_map_continues_in_sparse_headers_whatever_the_nonzero_flag() {
    let mut five = SLOTS.to_vec();
    five.splice(2..2, [(2000, 0), (2500, 0)]);
    for flag in [1, 2] {
        let bytes = tar_of(&[
            old_sparse_header(b"s.md", 3001, &five[..4], flag, 5),
            sparse_header(&five[4..], 0),
            padded(b"abcde"),
            document(b"after.md", b"after"),
        ]);

        assert_eq!(
            read_all(&bytes),
            [ok("s.md", &sparse_content()), ok("after.md", b"after")],
            "flag {flag}"
        );
    }
}

#[test]
fn an_old_sparse_item_is_a_regular_file() {
    let bytes = tar_of(&[
        old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5),
        padded(b"abcde"),
    ]);

    assert!(list(&bytes).unwrap()[0].regular);
}

#[test]
fn typeflag_s_outside_the_gnu_format_is_unreadable() {
    let block = with_magic(
        old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5),
        b"ustar\x0000",
    );

    assert_unreadable(&tar_of(&[block, padded(b"abcde")]), "GNU format");
}

#[test]
fn a_sparse_map_that_ended_but_says_more_follows_is_unreadable() {
    let bytes = tar_of(&[
        old_sparse_header(b"s.md", 3001, &SLOTS, 1, 5),
        padded(b"abcde"),
    ]);

    assert_unreadable(&bytes, "sparse map");
}

#[test]
fn a_sparse_header_that_ended_but_says_more_follows_is_unreadable() {
    let mut five = SLOTS.to_vec();
    five.splice(2..2, [(2000, 0), (2500, 0)]);
    let bytes = tar_of(&[
        old_sparse_header(b"s.md", 3001, &five[..4], 1, 5),
        sparse_header(&five[4..], 1),
        padded(b"abcde"),
    ]);

    assert_unreadable(&bytes, "sparse map");
}

#[test]
fn old_sparse_numbers_that_cannot_be_read_make_only_that_document_fail() {
    let bad_offset = {
        let mut block = old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5);
        block[386..398].copy_from_slice(b"zzzzzzzzzzz\0");
        with_checksum(block)
    };
    let bad_real_size = {
        let mut block = old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5);
        block[483..495].copy_from_slice(b"zzzzzzzzzzz\0");
        with_checksum(block)
    };
    for block in [bad_offset, bad_real_size] {
        let bytes = tar_of(&[block, padded(b"abcde"), document(b"after.md", b"after")]);

        let read = read_all(&bytes);

        assert_document_fails(&read[0], "number");
        assert_eq!(read[1], ok("after.md", b"after"));
    }
}

#[test]
fn old_sparse_base_256_numbers_are_read() {
    let mut block = old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5);
    let mut real = [0u8; 12];
    real[0] = 0x80;
    real[4..].copy_from_slice(&3001u64.to_be_bytes());
    block[483..495].copy_from_slice(&real);
    let bytes = tar_of(&[with_checksum(block), padded(b"abcde")]);

    assert_eq!(read_all(&bytes), [ok("s.md", &sparse_content())]);
}

#[test]
fn a_tar_that_ends_before_a_sparse_header_it_promises_is_unreadable() {
    let mut five = SLOTS.to_vec();
    five.splice(2..2, [(2000, 0), (2500, 0)]);
    let bytes = old_sparse_header(b"s.md", 3001, &five[..4], 1, 5);

    assert_unreadable(&bytes, "end-of-archive");
}

#[test]
fn slots_after_the_end_of_an_old_sparse_map_are_not_read() {
    let mut block = old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5);
    // After the empty fourth slot's place: garbage in a slot nothing reads.
    let unused_slot = 386 + 3 * 24;
    block[unused_slot..unused_slot + 12].copy_from_slice(b"zzzzzzzzzzz\0");
    let bytes = tar_of(&[with_checksum(block), padded(b"abcde")]);

    assert_eq!(read_all(&bytes), [ok("s.md", &sparse_content())]);
}

#[test]
fn old_sparse_numbers_that_cannot_be_read_in_a_sparse_header_keep_the_walk_going() {
    let mut five = SLOTS.to_vec();
    five.splice(2..2, [(2000, 0), (2500, 0)]);
    let mut extension = sparse_header(&five[4..], 0);
    extension[..12].copy_from_slice(b"zzzzzzzzzzz\0");
    let bytes = tar_of(&[
        old_sparse_header(b"s.md", 3001, &five[..4], 1, 5),
        extension,
        padded(b"abcde"),
        document(b"after.md", b"after"),
    ]);

    let read = read_all(&bytes);

    assert_document_fails(&read[0], "number");
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn old_sparse_maps_over_the_data_block_limit_fail_and_the_next_item_is_found() {
    let blocks: Vec<(u64, u64)> = (0..MAX_DATA_BLOCKS as u64 + 1)
        .map(|i| (i * 512, 0))
        .collect();
    let mut parts = vec![old_sparse_header(b"s.md", 1 << 30, &blocks[..4], 1, 0)];
    let chunks: Vec<&[(u64, u64)]> = blocks[4..].chunks(21).collect();
    for (i, slots) in chunks.iter().enumerate() {
        parts.push(sparse_header(slots, u8::from(i + 1 < chunks.len())));
    }
    parts.push(document(b"after.md", b"after"));

    let read = read_all(&tar_of(&parts));

    assert_document_fails(&read[0], "data blocks");
    assert_eq!(read[1], ok("after.md", b"after"));
}

// ---- sparse files: extended header records ----

fn sparse_0_0() -> Vec<u8> {
    records(&[
        ("GNU.sparse.size", b"3001"),
        ("GNU.sparse.numblocks", b"3"),
        ("GNU.sparse.offset", b"0"),
        ("GNU.sparse.numbytes", b"2"),
        ("GNU.sparse.offset", b"1024"),
        ("GNU.sparse.numbytes", b"2"),
        ("GNU.sparse.offset", b"3000"),
        ("GNU.sparse.numbytes", b"1"),
    ])
}

fn sparse_0_1() -> Vec<u8> {
    records(&[
        ("GNU.sparse.size", b"3001"),
        ("GNU.sparse.numblocks", b"3"),
        ("GNU.sparse.name", b"s.md"),
        ("GNU.sparse.map", b"0,2,1024,2,3000,1"),
    ])
}

fn sparse_1_0_member(name: &[u8], map: &str, stored: &[u8]) -> Vec<u8> {
    let extended_records = records(&[
        ("GNU.sparse.major", b"1"),
        ("GNU.sparse.minor", b"0"),
        ("GNU.sparse.name", name),
        ("GNU.sparse.realsize", b"3001"),
    ]);
    let data = [padded(map.as_bytes()), stored.to_vec()].concat();
    [
        extended(&extended_records),
        document(b"GNUSparseFile.0/stand-in.md", &data),
    ]
    .concat()
}

#[test]
fn pax_sparse_files_of_each_form_are_assembled_under_their_real_name() {
    for bytes in [
        tar_of(&[
            extended(&sparse_0_0()),
            document(b"s.md", b"abcde"),
            document(b"after.md", b"after"),
        ]),
        tar_of(&[
            extended(&sparse_0_1()),
            document(b"GNUSparseFile.0/stand-in.md", b"abcde"),
            document(b"after.md", b"after"),
        ]),
        tar_of(&[
            sparse_1_0_member(b"s.md", "3\n0\n2\n1024\n2\n3000\n1\n", b"abcde"),
            document(b"after.md", b"after"),
        ]),
    ] {
        assert_eq!(
            read_all(&bytes),
            [ok("s.md", &sparse_content()), ok("after.md", b"after")]
        );
    }
}

#[test]
fn a_pax_sparse_item_is_a_regular_file() {
    let bytes = tar_of(&[extended(&sparse_0_1()), document(b"stand-in.md", b"abcde")]);

    assert!(list(&bytes).unwrap()[0].regular);
}

#[test]
fn a_1_0_map_may_cross_blocks_and_what_pads_it_is_not_looked_at() {
    let mut map = format!("2\n{:0>508}\n2\n600\n1\n", 4).into_bytes();
    let map_len = map.len().div_ceil(512) * 512;
    map.resize(map_len, b'#');
    let data = [map, b"xyz".to_vec()].concat();
    let bytes = tar_of(&[
        extended(&records(&[
            ("GNU.sparse.major", b"1"),
            ("GNU.sparse.minor", b"0"),
            ("GNU.sparse.name", b"s.md"),
            ("GNU.sparse.realsize", b"601"),
        ])),
        document(b"stand-in.md", &data),
    ]);

    let mut expected = vec![0u8; 601];
    expected[4..6].copy_from_slice(b"xy");
    expected[600] = b'z';
    assert_eq!(read_all(&bytes), [ok("s.md", &expected)]);
}

#[test]
fn later_records_replace_earlier_ones_except_the_0_0_pairs() {
    let bytes = tar_of(&[
        extended(&records(&[
            ("GNU.sparse.size", b"1"),
            ("GNU.sparse.size", b"3001"),
            ("GNU.sparse.numblocks", b"3"),
            ("GNU.sparse.name", b"s.md"),
            ("GNU.sparse.map", b"0,1"),
            ("GNU.sparse.map", b"0,2,1024,2,3000,1"),
        ])),
        document(b"stand-in.md", b"abcde"),
    ]);

    assert_eq!(read_all(&bytes), [ok("s.md", &sparse_content())]);
}

#[test]
fn sparse_records_that_cannot_be_settled_make_only_that_document_fail() {
    const NOT_A_NUMBER: &str = "something that is not a number";
    const NO_LENGTH: &str = "offset with no length";
    const SIZE_NOT_A_NUMBER: &str = "GNU.sparse.size record is not a number";
    const SIZE_MISSING: &str = "GNU.sparse.size record is missing";
    const NO_NUMBYTES: &str = "no GNU.sparse.numbytes after";

    // Each case names its reason. The data is the same five bytes whatever
    // the records say, so most of these would also fail for not adding up.
    type Pairs = &'static [(&'static str, &'static [u8])];
    let cases: &[(Pairs, &str)] = &[
        // The 0.0 pairs out of step.
        (
            &[
                ("GNU.sparse.size", b"3001"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.numbytes", b"1"),
            ],
            "no GNU.sparse.offset before",
        ),
        (
            &[
                ("GNU.sparse.size", b"3001"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.offset", b"0"),
            ],
            NO_NUMBYTES,
        ),
        (
            &[
                ("GNU.sparse.size", b"3001"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.offset", b"0"),
                ("GNU.sparse.offset", b"0"),
                ("GNU.sparse.numbytes", b"1"),
            ],
            NO_NUMBYTES,
        ),
        // The number of data blocks does not match the pairs.
        (
            &[
                ("GNU.sparse.size", b"3001"),
                ("GNU.sparse.numblocks", b"2"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,1"),
            ],
            "declares",
        ),
        // A version other than 1.0.
        (
            &[
                ("GNU.sparse.major", b"2"),
                ("GNU.sparse.minor", b"0"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.realsize", b"1"),
            ],
            "version",
        ),
        (
            &[
                ("GNU.sparse.major", b"1"),
                ("GNU.sparse.minor", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.realsize", b"1"),
            ],
            "version",
        ),
        // 1.0 mixed with 0.x records.
        (
            &[
                ("GNU.sparse.major", b"1"),
                ("GNU.sparse.minor", b"0"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.realsize", b"1"),
                ("GNU.sparse.numblocks", b"0"),
            ],
            "mix",
        ),
        // Both 0.0 pairs and a 0.1 map.
        (
            &[
                ("GNU.sparse.size", b"3001"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,1"),
                ("GNU.sparse.offset", b"0"),
                ("GNU.sparse.numbytes", b"2"),
            ],
            "mix",
        ),
        // Values that are not numbers.
        (
            &[("GNU.sparse.size", b"+1"), ("GNU.sparse.numblocks", b"0")],
            SIZE_NOT_A_NUMBER,
        ),
        (
            &[
                ("GNU.sparse.size", b"9223372036854775808"),
                ("GNU.sparse.numblocks", b"0"),
            ],
            SIZE_NOT_A_NUMBER,
        ),
        // A number record with no value.
        (
            &[("GNU.sparse.size", b""), ("GNU.sparse.numblocks", b"0")],
            SIZE_NOT_A_NUMBER,
        ),
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"x"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b""),
            ],
            "GNU.sparse.numblocks record is not a number",
        ),
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,9223372036854775808"),
            ],
            NOT_A_NUMBER,
        ),
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.offset", b"x"),
                ("GNU.sparse.numbytes", b"1"),
            ],
            NOT_A_NUMBER,
        ),
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,x"),
            ],
            NOT_A_NUMBER,
        ),
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,,1"),
            ],
            NOT_A_NUMBER,
        ),
        // A map whose numbers do not pair up.
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,1,2"),
            ],
            NO_LENGTH,
        ),
        // A comma after the last number leaves an empty element.
        (
            &[
                ("GNU.sparse.size", b"1"),
                ("GNU.sparse.numblocks", b"1"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b"0,1,"),
            ],
            NO_LENGTH,
        ),
        // A record is missing.
        (
            &[("GNU.sparse.numblocks", b"0"), ("GNU.sparse.name", b"s.md")],
            SIZE_MISSING,
        ),
        (
            &[("GNU.sparse.size", b"0"), ("GNU.sparse.name", b"s.md")],
            "GNU.sparse.numblocks record is missing",
        ),
        // An unknown record.
        (
            &[
                ("GNU.sparse.size", b"0"),
                ("GNU.sparse.numblocks", b"0"),
                ("GNU.sparse.name", b"s.md"),
                ("GNU.sparse.map", b""),
                ("GNU.sparse.bogus", b"1"),
            ],
            "not known",
        ),
        // A name alone.
        (&[("GNU.sparse.name", b"s.md")], SIZE_MISSING),
    ];

    for (pairs, reason) in cases {
        let bytes = tar_of(&[
            extended(&records(pairs)),
            document(b"stand-in.md", b"abcde"),
            document(b"after.md", b"after"),
        ]);

        let read = read_all(&bytes);

        assert_document_fails(&read[0], reason);
        assert_eq!(read[1], ok("after.md", b"after"), "{pairs:?}");
    }
}

#[test]
fn a_typeflag_s_with_sparse_records_fails_only_that_document() {
    let bytes = tar_of(&[
        extended(&sparse_0_0()),
        old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5),
        padded(b"abcde"),
        document(b"after.md", b"after"),
    ]);

    let read = read_all(&bytes);

    assert_document_fails(&read[0], "two sparse maps");
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn sparse_records_on_an_item_that_is_not_regular_are_ignored() {
    let bytes = tar_of(&[
        extended(&sparse_0_0()),
        header(b"dir/", b'5', 0),
        document(b"after.md", b"after"),
    ]);

    let entries = list(&bytes).unwrap();

    assert_eq!(names(&entries), ["dir/", "after.md"]);
    assert!(!entries[0].regular);
}

#[test]
fn a_1_0_map_that_cannot_be_read_makes_only_that_document_fail() {
    for map in [
        "",
        "1\n0\n",
        "x\n",
        "\n",
        "1\n0\n+1\n",
        "99999999999999999999\n",
    ] {
        let bytes = tar_of(&[
            sparse_1_0_member(b"s.md", map, b""),
            document(b"after.md", b"after"),
        ]);

        let read = read_all(&bytes);

        assert_document_fails(&read[0], "sparse map");
        assert_eq!(read[1], ok("after.md", b"after"), "{map:?}");
    }
}

#[test]
fn a_1_0_map_is_read_only_inside_the_items_data() {
    // The map's last line would need bytes past the item's own data.
    let records_1_0 = records(&[
        ("GNU.sparse.major", b"1"),
        ("GNU.sparse.minor", b"0"),
        ("GNU.sparse.name", b"s.md"),
        ("GNU.sparse.realsize", b"3001"),
    ]);
    let bytes = tar_of(&[
        extended(&records_1_0),
        header(b"stand-in.md", b'0', 4),
        padded(b"1\n0\n2\n"),
        document(b"after.md", b"after"),
    ]);

    let read = read_all(&bytes);

    assert_document_fails(&read[0], "ends early");
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn a_1_0_map_whose_padding_passes_the_data_makes_only_that_document_fail() {
    let records_1_0 = records(&[
        ("GNU.sparse.major", b"1"),
        ("GNU.sparse.minor", b"0"),
        ("GNU.sparse.name", b"s.md"),
        ("GNU.sparse.realsize", b"3001"),
    ]);
    // The item's data is the map alone, and shorter than a block.
    let bytes = tar_of(&[
        extended(&records_1_0),
        document(b"stand-in.md", b"1\n0\n1\n"),
        document(b"after.md", b"after"),
    ]);

    let read = read_all(&bytes);

    assert_document_fails(&read[0], "past");
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn a_sparse_item_that_ends_before_its_stored_data_stops_the_rest() {
    let bytes = [
        old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5),
        padded(b"abcde"),
        vec![0; 1024],
    ]
    .concat();
    let entries = list(&bytes).unwrap();
    let cut = old_sparse_header(b"s.md", 3001, &SLOTS, 0, 5);

    let read = read_positions(
        file_holding(&cut),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_1_0_item_cut_inside_its_map_stops_the_rest() {
    let bytes = tar_of(&[sparse_1_0_member(b"s.md", "1\n0\n1\n", b"x")]);
    let entries = list(&bytes).unwrap();
    // Up to and including the stand-in header: nothing of the map.
    let cut = &bytes[..512 + 512 + 512];

    let read = read_positions(
        file_holding(cut),
        false,
        MAX_CONTENT,
        &positions_of(&entries),
    );

    assert!(matches!(&read[0], Got::RestUnreadable(_)), "{read:?}");
}

#[test]
fn a_sparse_file_needs_its_real_name_with_0_1_or_1_0_records() {
    for pairs in [
        &[
            ("GNU.sparse.size", &b"1"[..]),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.map", b""),
        ][..],
        &[
            ("GNU.sparse.size", b"1"),
            ("GNU.sparse.numblocks", b"0"),
            ("GNU.sparse.map", b""),
            ("GNU.sparse.name", b""),
        ],
        &[
            ("GNU.sparse.major", b"1"),
            ("GNU.sparse.minor", b"0"),
            ("GNU.sparse.realsize", b"1"),
        ],
        &[("GNU.sparse.major", b"1")],
        &[("GNU.sparse.minor", b"0")],
        &[("GNU.sparse.realsize", b"1")],
    ] {
        let bytes = tar_of(&[
            extended(&records(pairs)),
            document(b"GNUSparseFile.0/s.md", b""),
        ]);

        assert_unreadable(&bytes, "real name");
    }
}

#[test]
fn an_empty_sparse_name_is_unreadable_even_beside_a_path() {
    let v00: [(&str, &[u8]); 4] = [
        ("GNU.sparse.size", b"5"),
        ("GNU.sparse.numblocks", b"1"),
        ("GNU.sparse.offset", b"0"),
        ("GNU.sparse.numbytes", b"5"),
    ];
    for others in [&[][..], &v00[..]] {
        let mut pairs: Vec<(&str, &[u8])> = vec![("GNU.sparse.name", b""), ("path", b"real.md")];
        pairs.extend_from_slice(others);
        let bytes = tar_of(&[extended(&records(&pairs)), document(b"header.md", b"abcde")]);

        assert_unreadable(&bytes, "GNU.sparse.name");
    }
}

#[test]
fn the_0_0_form_needs_no_sparse_name() {
    let bytes = tar_of(&[extended(&sparse_0_0()), document(b"s.md", b"abcde")]);

    assert_eq!(names(&list(&bytes).unwrap()), ["s.md"]);
}

fn old_sparse_document(slots: &[(u64, u64)], real: u64, stored: &[u8]) -> Vec<u8> {
    tar_of(&[
        old_sparse_header(b"s.md", real, slots, 0, stored.len() as u64),
        padded(stored),
        document(b"after.md", b"after"),
    ])
}

#[test]
fn data_blocks_that_do_not_describe_the_stored_data_make_only_that_document_fail() {
    for (slots, stored, reason) in [
        // Out of order.
        (vec![(8, 2), (2, 3)], &b"abcde"[..], "order"),
        // Overlapping.
        (vec![(2, 3), (4, 2)], b"abcde", "order"),
        // Past the file's size.
        (vec![(2, 3), (11, 2)], b"abcde", "past"),
        // More data than the blocks take.
        (vec![(2, 3)], b"abcde", "add up"),
        // Less data than the blocks take.
        (vec![(2, 3), (8, 4)], b"abcde", "add up"),
    ] {
        let read = read_all(&old_sparse_document(&slots, 12, stored));

        assert_document_fails(&read[0], reason);
        assert_eq!(read[1], ok("after.md", b"after"), "{slots:?}");
    }
}

#[test]
fn stored_data_goes_to_the_data_blocks_in_order_and_the_gaps_are_zeros() {
    let read = read_all(&old_sparse_document(
        &[(2, 3), (8, 2), (12, 0)],
        12,
        b"abcde",
    ));

    assert_eq!(read[0], ok("s.md", b"\0\0abc\0\0\0de\0\0"));
}

#[test]
fn an_old_sparse_map_that_fills_the_header_ends_with_it() {
    let read = read_all(&old_sparse_document(
        &[(0, 1), (100, 1), (200, 1), (300, 2)],
        302,
        b"abcde",
    ));

    let mut expected = vec![0u8; 302];
    expected[0] = b'a';
    expected[100] = b'b';
    expected[200] = b'c';
    expected[300..].copy_from_slice(b"de");
    assert_eq!(read[0], ok("s.md", &expected));
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn data_blocks_whose_lengths_are_not_whole_blocks_are_read_as_packed() {
    let read = read_all(&old_sparse_document(&[(0, 3), (1000, 2)], 2000, b"abcde"));

    let mut expected = vec![0u8; 2000];
    expected[..3].copy_from_slice(b"abc");
    expected[1000..1002].copy_from_slice(b"de");
    assert_eq!(read[0], ok("s.md", &expected));
}

#[test]
fn a_sparse_file_whose_whole_length_is_zeros_is_read() {
    // GNU tar writes one empty data block at the end of the file.
    let read = read_all(&old_sparse_document(&[(8, 0)], 8, b""));

    assert_eq!(read[0], ok("s.md", &[0u8; 8]));
}

#[test]
fn a_sparse_file_over_the_content_limit_makes_only_that_document_fail() {
    let bytes = old_sparse_document(&SLOTS, 3001, b"abcde");

    let read = read_all_limited(&bytes, false, 3000);

    assert_document_fails(&read[0], "larger than");
    assert_eq!(read[1], ok("after.md", b"after"));
}

#[test]
fn a_sparse_file_at_the_content_limit_is_read() {
    let bytes = old_sparse_document(&SLOTS, 3001, b"abcde");

    assert_eq!(
        read_all_limited(&bytes, false, 3001)[0],
        ok("s.md", &sparse_content())
    );
}

/// 0.0 records for `count` empty data blocks.
fn sparse_0_0_records(count: usize) -> Vec<u8> {
    let mut all = records(&[
        ("GNU.sparse.size", b"1073741824"),
        ("GNU.sparse.numblocks", count.to_string().as_bytes()),
    ]);
    for i in 0..count as u64 {
        all.extend(record(
            "GNU.sparse.offset",
            (i * 512).to_string().as_bytes(),
        ));
        all.extend(record("GNU.sparse.numbytes", b"0"));
    }
    all
}

#[test]
fn pax_maps_over_the_data_block_limit_fail_but_later_records_still_count() {
    let over = MAX_DATA_BLOCKS + 1;
    let mut named = sparse_0_0_records(over);
    named.extend(record("path", b"named.md"));
    let map: Vec<String> = (0..over as u64)
        .flat_map(|i| [(i * 512).to_string(), "0".to_string()])
        .collect();
    let v01 = [
        records(&[
            ("GNU.sparse.size", b"1073741824"),
            ("GNU.sparse.numblocks", over.to_string().as_bytes()),
            ("GNU.sparse.name", b"map.md"),
        ]),
        record("GNU.sparse.map", map.join(",").as_bytes()),
    ]
    .concat();
    let bytes = tar_of(&[
        extended(&named),
        header(b"stand-in.md", b'0', 0),
        extended(&v01),
        header(b"GNUSparseFile.0/map.md", b'0', 0),
        sparse_1_0_member(b"in-data.md", &format!("{over}\n"), b""),
        document(b"after.md", b"after"),
    ]);

    let read = read_all(&bytes);

    let entries = list(&bytes).unwrap();
    assert_eq!(
        names(&entries),
        ["named.md", "map.md", "in-data.md", "after.md"]
    );
    for item in &read[..3] {
        assert_document_fails(item, "data blocks");
    }
    assert_eq!(read[3], ok("after.md", b"after"));
}

#[test]
fn a_malformed_record_after_a_map_over_the_limit_is_unreadable() {
    let mut all = sparse_0_0_records(MAX_DATA_BLOCKS + 1);
    all.extend(b"9 x y z\n");
    let bytes = tar_of(&[extended(&all), header(b"s.md", b'0', 0)]);

    assert_unreadable(&bytes, "malformed");
}

#[test]
fn a_map_at_the_data_block_limit_is_accepted() {
    let bytes = tar_of(&[
        extended(&sparse_0_0_records(MAX_DATA_BLOCKS)),
        header(b"s.md", b'0', 0),
    ]);

    // Only the real size, 1 GiB, is left to fail.
    assert_document_fails(&read_all(&bytes)[0], "larger than");
    let read = read_all_limited(&bytes, false, 1 << 30);
    assert!(matches!(&read[0], Got::Item { content: Ok(_), .. }));
}

// ---- the real archives ----

const SHORT: &[u8] = b"# Short\n\nA plain document.\n";
const LONG: &[u8] = b"# Long\n\nA document with a path over 100 bytes.\n";
const JAPANESE: &str = "# 日本語\n\n名前が UTF-8 の文書です。\n";
const LONG_PATH: &str = "deep/segment-with-a-long-name-0001/segment-with-a-long-name-0002/\
                         segment-with-a-long-name-0003/segment-with-a-long-name-0004/\
                         segment-with-a-long-name-0005/long.md";

fn repeated() -> Vec<u8> {
    let mut text = b"# Repeated\n\n".to_vec();
    text.extend(b"A line that deflates well.\n".repeat(200));
    text
}

#[test]
fn tars_written_by_gnu_tar_and_bsdtar_are_read() {
    let tree = [
        ok(LONG_PATH, LONG),
        ok("hard.md", SHORT),
        ok("日本語.md", JAPANESE.as_bytes()),
    ];
    for name in [
        "gtar-gnu.tar.gz",
        "gtar-posix.tar.gz",
        "gtar-ustar.tar.gz",
        "bsdtar-default.tar.gz",
    ] {
        assert_eq!(fixture_documents(name), tree, "{name}");
    }
}

#[test]
fn links_directories_and_fifos_in_real_tars_are_not_regular() {
    let entries = list_fixture("gtar-gnu.tar.gz").unwrap();
    for name in [
        "notes/",
        "notes/short.md",
        "link-to-short.md",
        "long-link.md",
        "pipe.md",
    ] {
        assert_not_regular(&entries, name);
    }
    let entries = list_fixture("bsdtar-default.tar.gz").unwrap();
    for name in [
        "notes/",
        "notes/short.md",
        "link-to-short.md",
        "long-link.md",
        "pipe.md",
    ] {
        assert_not_regular(&entries, name);
    }
}

#[test]
fn a_v7_tar_is_read() {
    assert_eq!(fixture_documents("gtar-v7.tar.gz"), [ok("hard.md", SHORT)]);
}

#[test]
fn tars_written_by_git_are_read() {
    let tree = [
        ok(LONG_PATH, LONG),
        ok("notes/repeated.md", &repeated()),
        ok("notes/short.md", SHORT),
        ok("日本語.md", JAPANESE.as_bytes()),
    ];
    assert_eq!(fixture_documents("git-archive.tar.gz"), tree);
    assert_eq!(fixture_documents("git-archive-gzip.tar.gz"), tree);
}

#[test]
fn tars_with_a_global_comment_are_read() {
    assert_eq!(
        fixture_documents("gtar-posix-global.tar.gz"),
        [ok("notes/short.md", SHORT)]
    );
}

#[test]
fn a_tar_with_a_global_path_is_unreadable() {
    assert_unreadable_result(list_fixture("python-pax-global-path.tar.gz"), "global");
}

#[test]
fn what_each_tool_writes_with_gzip_is_read() {
    for name in [
        "gtar-gzip.tar.gz",
        "bsdtar-gzip.tar.gz",
        "python-gzip.tar.gz",
    ] {
        assert_eq!(
            fixture_documents(name),
            [ok("notes/short.md", SHORT)],
            "{name}"
        );
    }
}

#[test]
fn tars_written_by_python_are_read() {
    let two = [ok("notes/short.md", SHORT), ok(LONG_PATH, LONG)];
    assert_eq!(fixture_documents("python-gnu.tar.gz"), two);
    assert_eq!(fixture_documents("python-ustar.tar.gz"), two);
}

#[test]
fn a_python_pax_tar_gives_names_that_are_not_utf_8_as_bytes() {
    let entries = list_fixture("python-pax.tar.gz").unwrap();
    assert!(entries.iter().any(|entry| entry.name == b"latin1-\xe9.md"));
    let documents = fixture_documents("python-pax.tar.gz");
    let expected_sized = ok("sized.md", b"# Sized\n\nCarries a pax size record.\n");
    assert_eq!(documents.last(), Some(&expected_sized));
    assert_eq!(documents[0], ok("notes/short.md", SHORT));
    assert_eq!(documents[1], ok(LONG_PATH, LONG));
    assert_eq!(documents[2], ok("日本語.md", JAPANESE.as_bytes()));
}

#[test]
fn a_tar_with_a_very_large_extended_header_is_read() {
    assert_eq!(
        fixture_documents("bsdtar-large-xattr.tar.gz"),
        [
            ok("big-xattr.md", b"# Big extended attribute\n"),
            ok("after.md", b"# After\n\nComes after the big pax header.\n"),
        ]
    );
}

#[test]
fn the_mac_metadata_item_is_returned_like_any_other() {
    let names = names(&list_fixture("bsdtar-mac-metadata.tar.gz").unwrap());

    assert!(names.contains(&"._tagged.md".to_string()), "{names:?}");
    assert!(names.contains(&"tagged.md".to_string()), "{names:?}");
}

/// A file holding `head`, 8 GiB of zeros (a hole, so taking no disk) and `tail`.
fn file_with_8_gib_of_zeros(head: &[u8], tail: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(head).unwrap();
    file.set_len(head.len() as u64 + (8 << 30)).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(tail).unwrap();
    file
}

#[test]
fn an_8_gib_item_is_stepped_over_by_its_extended_or_base_256_size() {
    let tail = fixture_bytes("python-8gib.tail");
    for head in ["python-8gib-pax.head", "python-8gib-gnu.head"] {
        let head = fixture_bytes(head);

        let entries = list_file(file_with_8_gib_of_zeros(&head, &tail), false, NO_LIMIT).unwrap();
        assert_eq!(names(&entries), ["big.bin", "after.md"]);

        let read = read_positions(
            file_with_8_gib_of_zeros(&head, &tail),
            false,
            MAX_CONTENT,
            &[entries[1].position],
        );
        assert_eq!(
            read,
            [ok(
                "after.md",
                b"# After\n\nComes after the 8 GiB member.\n"
            )]
        );
    }
}

fn sparse_md() -> Vec<u8> {
    let mut file = vec![0u8; 101 * 65536];
    for i in 0..100 {
        let part = format!("# Part {i}\n");
        file[i * 65536..i * 65536 + part.len()].copy_from_slice(part.as_bytes());
    }
    file
}

fn few_md() -> Vec<u8> {
    let mut file = vec![0u8; 65536 + 7];
    file[..7].copy_from_slice(b"# Head\n");
    file[65536..].copy_from_slice(b"# Tail\n");
    file
}

#[test]
fn sparse_files_written_by_gnu_tar_and_bsdtar_are_assembled() {
    let expected = [
        ok("all-hole.md", &[0u8; 65536]),
        ok("few.md", &few_md()),
        ok("sparse.md", &sparse_md()),
        ok("zz-after.md", b"# After\n\nComes after the sparse files.\n"),
    ];
    for name in [
        "gtar-sparse-old.tar.gz",
        "gtar-sparse-0.0.tar.gz",
        "gtar-sparse-0.1.tar.gz",
        "gtar-sparse-1.0.tar.gz",
        "bsdtar-sparse.tar.gz",
    ] {
        let documents = fixture_documents(name);

        assert!(documents == expected, "{name} is not what was expected");
    }
}

#[test]
fn a_real_tar_is_read_in_full_on_the_second_pass_too() {
    let entries = list_fixture("gtar-gnu.tar.gz").unwrap();
    let file = File::open(fixture_path("gtar-gnu.tar.gz")).unwrap();

    let read = read_positions(file, true, MAX_CONTENT, &positions_of(&entries));

    assert_eq!(read.len(), entries.len());
    assert!(read.iter().all(|item| matches!(item, Got::Item { .. })));
}
