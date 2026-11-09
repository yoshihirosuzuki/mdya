use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::test_zip::{
    Built, FLAG_DATA_DESCRIPTOR, FLAG_ENCRYPTED, FLAG_UTF8, TestEntry, TestZip, Zip64Field, crc32,
    end_record, extra_field,
};
use super::{zip_contents, zip_listing};
use crate::archive_ingest::transfer::{Contents, Entry, ListError, ListSize, Position, ReadResult};
use crate::types::BoxError;

const NO_LIMIT: u64 = u64::MAX;
const REPEATED: &[u8] = b"A line that deflates well.\nA line that deflates well.\nA line.\n";
/// What the test archives hold in `notes/short.md` and `tagged.md`.
const SHORT: &[u8] = b"# Short\n\nA plain document.\n";
const TAGGED: &[u8] = b"# Tagged\n\nCarries an extended attribute.\n";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archives")
        .join(name);
    std::fs::read(path).unwrap()
}

fn file_with(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

fn list_with_limit(bytes: &[u8], limit: u64) -> Result<Vec<Entry>, ListError> {
    let mut listing = zip_listing(file_with(bytes));
    let mut size = ListSize::new(limit);
    let mut entries = Vec::new();
    while let Some(entry) = listing.next(&mut size)? {
        entries.push(entry);
    }
    Ok(entries)
}

fn list(bytes: &[u8]) -> Vec<Entry> {
    list_with_limit(bytes, NO_LIMIT).unwrap()
}

fn names(bytes: &[u8]) -> Vec<Vec<u8>> {
    list(bytes).into_iter().map(|entry| entry.name).collect()
}

fn open(bytes: &[u8], max_content: u64) -> Box<dyn Contents> {
    match zip_contents(file_with(bytes), max_content) {
        Ok(contents) => contents,
        Err(error) => panic!("{error}"),
    }
}

/// The name, whether regular, and content of the entry at `index`.
fn read_at(
    bytes: &[u8],
    index: u64,
    max_content: u64,
) -> (Vec<u8>, bool, Result<Vec<u8>, BoxError>) {
    match open(bytes, max_content).read(Position(index)) {
        ReadResult::Entry {
            name,
            regular,
            content,
        } => (name, regular, content),
        ReadResult::RestUnreadable(error) => panic!("{error}"),
    }
}

fn content_at(bytes: &[u8], index: u64) -> Vec<u8> {
    read_at(bytes, index, NO_LIMIT).2.unwrap()
}

fn index_of(bytes: &[u8], name: &str) -> u64 {
    names(bytes)
        .iter()
        .position(|candidate| candidate == name.as_bytes())
        .unwrap() as u64
}

/// Neither pass accepts the file: the second pass repeats every check.
#[track_caller]
fn assert_source_unreadable(bytes: &[u8]) {
    assert!(matches!(
        list_with_limit(bytes, NO_LIMIT),
        Err(ListError::SourceUnreadable(_))
    ));
    assert!(zip_contents(file_with(bytes), NO_LIMIT).is_err());
}

#[track_caller]
fn assert_document_unreadable(bytes: &[u8]) {
    assert!(read_at(bytes, 0, NO_LIMIT).2.is_err());
}

fn one_entry() -> TestZip {
    TestZip::of(vec![TestEntry::stored("a.md", b"alpha")])
}

fn two_entries() -> TestZip {
    TestZip::of(vec![
        TestEntry::stored("a.md", b"alpha"),
        TestEntry::deflated("b.md", REPEATED),
    ])
}

// What archivers write.

#[test]
fn a_git_archive_lists_links_and_directories_as_not_regular() {
    let bytes = fixture("git-archive.zip");
    let entries = list(&bytes);
    let regular = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name.as_bytes())
            .unwrap()
            .regular
    };

    assert!(!regular("link-to-short.md"));
    assert!(!regular("notes/"));
    assert!(regular("notes/short.md"));
}

#[test]
fn a_git_archive_with_a_comment_reads_stored_and_deflated_documents() {
    let bytes = fixture("git-archive.zip");

    assert_eq!(
        content_at(&bytes, index_of(&bytes, "notes/short.md")),
        SHORT
    );
    let repeated = content_at(&bytes, index_of(&bytes, "notes/repeated.md"));
    assert!(repeated.starts_with(b"# Repeated\n\nA line that deflates well.\n"));
}

#[test]
fn a_name_flagged_as_utf8_is_read_as_it_is() {
    let bytes = fixture("git-archive.zip");

    assert!(names(&bytes).contains(&"日本語.md".as_bytes().to_vec()));
}

#[test]
fn ditto_lists_macos_entries_and_a_utf8_name_without_the_flag() {
    let bytes = fixture("ditto.zip");
    let names = names(&bytes);

    assert!(names.contains(&b"__MACOSX/zipsrc/._tagged.md".to_vec()));
    assert!(names.contains(&"zipsrc/日本語.md".as_bytes().to_vec()));
    assert_eq!(
        content_at(&bytes, index_of(&bytes, "zipsrc/notes/short.md")),
        SHORT
    );
}

#[test]
fn the_zip_command_lists_a_utf8_name_without_the_flag() {
    let bytes = fixture("zip-command.zip");

    assert!(names(&bytes).contains(&"日本語.md".as_bytes().to_vec()));
    assert_eq!(content_at(&bytes, index_of(&bytes, "tagged.md")), TAGGED);
}

#[test]
fn entries_with_the_same_name_are_listed_in_order_with_their_own_content() {
    let bytes = fixture("python-duplicate-names.zip");

    assert_eq!(
        names(&bytes),
        vec![b"a.md".to_vec(), b"./a.md".to_vec(), b"a.md".to_vec()]
    );
    assert_eq!(content_at(&bytes, 0), b"# First\n");
    assert_eq!(content_at(&bytes, 2), b"# Third\n");
}

#[test]
fn zip64_end_records_next_to_a_plain_end_record_are_read() {
    let bytes = fixture("zip-command-stdin.zip");

    assert_eq!(names(&bytes), vec![b"tagged.md".to_vec(), b"-".to_vec()]);
    assert_eq!(content_at(&bytes, 1), SHORT);
}

#[test]
fn sizes_are_read_from_the_zip64_extra_field() {
    let bytes = fixture("zip-command-zip64.zip");

    assert_eq!(
        content_at(&bytes, index_of(&bytes, "notes/short.md")),
        SHORT
    );
}

#[test]
fn a_nested_zip_is_read_as_the_outer_zip() {
    let bytes = fixture("zip-command-nested.zip");

    assert_eq!(
        names(&bytes),
        vec![
            b"notes/".to_vec(),
            b"notes/short.md".to_vec(),
            b"zz-bundle.zip".to_vec()
        ]
    );
}

#[test]
fn a_nested_zip_cut_at_the_end_is_unreadable() {
    let mut bytes = fixture("zip-command-nested.zip");
    bytes.truncate(bytes.len() - 30);

    assert_source_unreadable(&bytes);
}

// Forms that are read.

#[test]
fn a_zip_without_entries_lists_nothing() {
    let bytes = TestZip::default().build().bytes;

    assert!(list(&bytes).is_empty());
}

#[test]
fn asking_again_after_the_end_gives_nothing() {
    let bytes = one_entry().build().bytes;
    let mut listing = zip_listing(file_with(&bytes));
    let mut size = ListSize::new(NO_LIMIT);

    assert!(listing.next(&mut size).unwrap().is_some());
    assert!(listing.next(&mut size).unwrap().is_none());
    assert!(listing.next(&mut size).unwrap().is_none());
}

#[test]
fn reading_does_not_depend_on_where_the_file_was_positioned() {
    let bytes = two_entries().build().bytes;
    let mut for_listing = file_with(&bytes);
    for_listing.seek(SeekFrom::Start(3)).unwrap();
    let mut for_contents = file_with(&bytes);
    for_contents.seek(SeekFrom::Start(3)).unwrap();

    let mut size = ListSize::new(NO_LIMIT);
    let first = zip_listing(for_listing).next(&mut size).unwrap().unwrap();
    let Ok(mut contents) = zip_contents(for_contents, NO_LIMIT) else {
        panic!("the second pass failed");
    };

    assert_eq!(first.name, b"a.md");
    assert!(matches!(
        contents.read(Position(0)),
        ReadResult::Entry { content: Ok(content), .. } if content == b"alpha"
    ));
}

#[test]
fn positions_grow_in_the_order_of_the_entries() {
    let entries = list(&two_entries().build().bytes);

    assert!(entries[0].position < entries[1].position);
}

#[test]
fn end_record_fields_at_their_maximum_are_read_from_the_zip64_record() {
    let zip = TestZip {
        zip64: true,
        end_record_at_maximum: true,
        ..two_entries()
    };
    let bytes = zip.build().bytes;

    assert_eq!(names(&bytes), vec![b"a.md".to_vec(), b"b.md".to_vec()]);
    assert_eq!(content_at(&bytes, 1), REPEATED);
}

#[test]
fn a_signature_in_the_comment_that_cannot_be_a_record_is_ignored() {
    let too_near_the_end = b"PK\x05\x06 short".to_vec();
    let mut comment_too_long = end_record(0, 0, 0);
    comment_too_long[20] = 0xFF;
    comment_too_long[21] = 0xFF;

    for comment in [too_near_the_end, comment_too_long] {
        let zip = TestZip {
            comment,
            ..one_entry()
        };
        assert_eq!(names(&zip.build().bytes), vec![b"a.md".to_vec()]);
    }
}

#[test]
fn bytes_after_the_comment_are_accepted_within_the_search_range() {
    let mut bytes = one_entry().build().bytes;
    bytes.extend_from_slice(b"trailing bytes");

    assert_eq!(names(&bytes), vec![b"a.md".to_vec()]);
}

#[test]
fn an_end_record_at_the_far_end_of_the_search_range_is_read() {
    let mut bytes = one_entry().build().bytes;
    bytes.extend_from_slice(&vec![0; 65_535]);

    assert_eq!(names(&bytes), vec![b"a.md".to_vec()]);
}

#[test]
fn of_two_zips_joined_together_the_first_is_read() {
    let mut bytes = one_entry().build().bytes;
    let second = TestZip::of(vec![TestEntry::stored("b.md", b"beta")]);
    bytes.extend_from_slice(&second.build().bytes);

    assert_eq!(names(&bytes), vec![b"a.md".to_vec()]);
}

#[test]
fn every_combination_of_zip64_fields_is_read_in_order() {
    use Zip64Field::{Compressed, DiskStart, LocalOffset, Uncompressed};
    for mask in 0..16_u8 {
        let in_zip64: Vec<Zip64Field> = [Uncompressed, Compressed, LocalOffset, DiskStart]
            .into_iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, field)| field)
            .collect();
        let mut entries = two_entries().entries;
        entries[1].in_zip64 = in_zip64;
        let bytes = TestZip::of(entries).build().bytes;

        assert_eq!(content_at(&bytes, 1), REPEATED, "mask {mask}");
    }
}

#[test]
fn a_digital_signature_after_the_headers_is_accepted() {
    let zip = TestZip {
        digital_signature: Some(b"signed".to_vec()),
        ..one_entry()
    };

    assert_eq!(names(&zip.build().bytes), vec![b"a.md".to_vec()]);
}

fn unicode_path(version: u8, name_crc: u32, name: &str) -> Vec<u8> {
    let mut data = vec![version];
    data.extend_from_slice(&name_crc.to_le_bytes());
    data.extend_from_slice(name.as_bytes());
    extra_field(0x7075, &data)
}

fn entry_with_unicode_path(flags: u16, extra: Vec<u8>) -> Vec<u8> {
    let mut entry = TestEntry::stored("cp932.md", b"alpha");
    entry.flags = flags;
    entry.extra = extra;
    TestZip::of(vec![entry]).build().bytes
}

#[test]
fn the_unicode_path_is_used_when_its_version_and_crc_match() {
    let extra = unicode_path(1, crc32(b"cp932.md"), "日本語.md");
    let bytes = entry_with_unicode_path(0, extra);

    assert_eq!(names(&bytes), vec!["日本語.md".as_bytes().to_vec()]);
    assert_eq!(read_at(&bytes, 0, NO_LIMIT).0, "日本語.md".as_bytes());
}

#[test]
fn the_unicode_path_is_found_after_another_extra_field() {
    let mut extra = extra_field(0x1234, b"other");
    extra.extend_from_slice(&unicode_path(1, crc32(b"cp932.md"), "日本語.md"));
    let bytes = entry_with_unicode_path(0, extra);

    assert_eq!(names(&bytes), vec!["日本語.md".as_bytes().to_vec()]);
}

#[test]
fn the_unicode_path_is_not_used_otherwise() {
    let matching_crc = crc32(b"cp932.md");
    for (flags, extra) in [
        (FLAG_UTF8, unicode_path(1, matching_crc, "日本語.md")),
        (0, unicode_path(2, matching_crc, "日本語.md")),
        (0, unicode_path(1, matching_crc ^ 1, "日本語.md")),
        (0, extra_field(0x7075, &[1, 0, 0])),
    ] {
        let bytes = entry_with_unicode_path(flags, extra);

        assert_eq!(names(&bytes), vec![b"cp932.md".to_vec()]);
    }
}

#[test]
fn a_unicode_path_only_in_the_local_header_is_not_used() {
    let mut entry = TestEntry::stored("cp932.md", b"alpha");
    entry.local_extra = unicode_path(1, crc32(b"cp932.md"), "日本語.md");
    let bytes = TestZip::of(vec![entry]).build().bytes;

    assert_eq!(names(&bytes), vec![b"cp932.md".to_vec()]);
    assert_eq!(content_at(&bytes, 0), b"alpha");
}

#[test]
fn a_data_descriptor_leaves_the_local_header_empty_of_sizes() {
    let mut entry = TestEntry::deflated("a.md", REPEATED);
    entry.flags = FLAG_DATA_DESCRIPTOR;
    let bytes = TestZip::of(vec![entry, TestEntry::stored("b.md", b"beta")])
        .build()
        .bytes;

    assert_eq!(content_at(&bytes, 0), REPEATED);
    assert_eq!(content_at(&bytes, 1), b"beta");
}

#[test]
fn unix_kinds_other_than_regular_are_not_regular() {
    for kind in [0o040000, 0o010000, 0o020000, 0o060000, 0o120000, 0o140000] {
        let entry = TestEntry::stored("a.md", b"alpha").with_unix_kind(kind);
        let bytes = TestZip::of(vec![entry]).build().bytes;

        assert!(!list(&bytes)[0].regular, "kind {kind:o}");
    }
}

#[test]
fn an_unknown_or_regular_unix_kind_is_regular() {
    for kind in [0, 0o100000] {
        let entry = TestEntry::stored("a.md", b"alpha").with_unix_kind(kind);
        let bytes = TestZip::of(vec![entry]).build().bytes;

        assert!(list(&bytes)[0].regular, "kind {kind:o}");
    }
}

#[test]
fn a_name_ending_with_a_slash_is_not_regular() {
    let bytes = TestZip::of(vec![TestEntry::stored("dir/", b"")])
        .build()
        .bytes;

    assert!(!list(&bytes)[0].regular);
    assert!(!read_at(&bytes, 0, NO_LIMIT).1);
}

// Sources that cannot be read: the end records.

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_read_is_unreadable() {
    use std::os::fd::OwnedFd;

    let pipe_end = || {
        let (reader, _writer) = std::io::pipe().unwrap();
        File::from(OwnedFd::from(reader))
    };
    let mut size = ListSize::new(NO_LIMIT);

    assert!(matches!(
        zip_listing(pipe_end()).next(&mut size),
        Err(ListError::SourceUnreadable(_))
    ));
    assert!(zip_contents(pipe_end(), NO_LIMIT).is_err());
}

#[test]
fn a_file_without_an_end_record_is_unreadable() {
    assert_source_unreadable(b"");
    assert_source_unreadable(b"this is not a zip file at all, only some bytes");
}

#[test]
fn a_fake_end_record_in_the_comment_is_unreadable() {
    let end_of_real_record = one_entry().build().end_record + 22;
    let zip = TestZip {
        comment: end_record(0, 0, end_of_real_record as u32),
        ..one_entry()
    };

    assert_source_unreadable(&zip.build().bytes);
}

#[test]
fn an_end_record_beyond_the_search_range_is_unreadable() {
    let mut bytes = one_entry().build().bytes;
    bytes.extend_from_slice(&vec![0; 65_536]);

    assert_source_unreadable(&bytes);
}

#[test]
fn a_split_zip_is_unreadable() {
    for field in [4, 6] {
        let mut built = one_entry().build();
        let at = built.end_record + field;
        built.put_u16(at, 1);
        assert_source_unreadable(&built.bytes);
    }
    let mut built = two_entries().build();
    let at = built.end_record + 8;
    built.put_u16(at, 1);
    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_split_zip64_is_unreadable() {
    let zip64 = || {
        TestZip {
            zip64: true,
            ..two_entries()
        }
        .build()
    };
    for field in [16, 20] {
        let mut built = zip64();
        let at = built.zip64_record + field;
        built.put_u32(at, 1);
        assert_source_unreadable(&built.bytes);
    }
    let mut built = zip64();
    let at = built.zip64_record + 24;
    built.put_u64(at, 1);
    assert_source_unreadable(&built.bytes);
    let mut built = zip64();
    let at = built.zip64_locator + 4;
    built.put_u32(at, 1);
    assert_source_unreadable(&built.bytes);
    let mut built = zip64();
    let at = built.zip64_locator + 16;
    built.put_u32(at, 2);
    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_broken_zip64_record_is_unreadable() {
    let zip64 = || {
        TestZip {
            zip64: true,
            ..one_entry()
        }
        .build()
    };
    let mut bad_signature = zip64();
    let at = bad_signature.zip64_record;
    bad_signature.put_u8(at, 0);
    assert_source_unreadable(&bad_signature.bytes);

    let mut bad_size = zip64();
    let at = bad_size.zip64_record + 4;
    bad_size.put_u64(at, 50);
    assert_source_unreadable(&bad_size.bytes);

    let mut bad_offset = zip64();
    let at = bad_offset.zip64_locator + 8;
    bad_offset.put_u64(at, u64::MAX);
    assert_source_unreadable(&bad_offset.bytes);
}

#[test]
fn data_put_before_a_zip_without_fixing_its_offsets_is_unreadable() {
    let mut bytes = b"a self-extracting program".to_vec();
    bytes.extend_from_slice(&one_entry().build().bytes);

    assert_source_unreadable(&bytes);
}

#[test]
fn a_false_directory_offset_or_size_is_unreadable() {
    for field in [12, 16] {
        let mut built = one_entry().build();
        let at = built.end_record + field;
        let value = u32::from_le_bytes(built.bytes[at..at + 4].try_into().unwrap());
        built.put_u32(at, value + 1);
        assert_source_unreadable(&built.bytes);
    }
}

#[test]
fn end_records_side_by_side_are_unreadable_without_reading_outside_the_search_range() {
    let no_directories: Vec<u8> = end_record(0, 0, 0).repeat(4_000);
    assert_source_unreadable(&no_directories);

    let mut pointing_at_themselves = Vec::new();
    for index in 0..4_000_u32 {
        pointing_at_themselves.extend_from_slice(&end_record(1, 0, index * 22));
    }
    assert_source_unreadable(&pointing_at_themselves);
}

// Sources that cannot be read: the central directory.

#[test]
fn a_false_entry_count_is_unreadable() {
    for count in [1, 3] {
        let mut built = two_entries().build();
        let (here, total) = (built.end_record + 8, built.end_record + 10);
        built.put_u16(here, count);
        built.put_u16(total, count);
        assert_source_unreadable(&built.bytes);
    }
}

#[test]
fn a_huge_false_entry_count_does_not_allocate_for_it() {
    let mut built = TestZip {
        zip64: true,
        ..one_entry()
    }
    .build();
    let (here, total) = (built.zip64_record + 24, built.zip64_record + 32);
    built.put_u64(here, u64::MAX);
    built.put_u64(total, u64::MAX);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_wrong_central_header_signature_is_unreadable() {
    let mut built = one_entry().build();
    let at = built.central_headers[0];
    built.put_u8(at, 0);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_name_that_runs_past_the_directory_is_unreadable() {
    let mut built = one_entry().build();
    let at = built.central_headers[0] + 28;
    built.put_u16(at, 1_000);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn bytes_left_after_the_headers_are_unreadable() {
    let mut bad_signature = TestZip {
        digital_signature: Some(b"signed".to_vec()),
        ..one_entry()
    }
    .build();
    let at = bad_signature.directory_start + 46 + 4;
    bad_signature.put_u8(at, 0);
    assert_source_unreadable(&bad_signature.bytes);

    let mut bad_length = TestZip {
        digital_signature: Some(b"signed".to_vec()),
        ..one_entry()
    }
    .build();
    let at = bad_length.directory_start + 46 + 4 + 4;
    bad_length.put_u16(at, 2);
    assert_source_unreadable(&bad_length.bytes);
}

#[test]
fn a_malformed_extra_field_is_unreadable() {
    let cut_short = vec![1, 2, 3];
    let too_long = {
        let mut field = extra_field(0x1234, b"abc");
        field[2] = 200;
        field
    };
    for extra in [cut_short, too_long] {
        let mut entry = TestEntry::stored("a.md", b"alpha");
        entry.extra = extra;

        assert_source_unreadable(&TestZip::of(vec![entry]).build().bytes);
    }
}

#[test]
fn a_maximum_field_without_a_zip64_extra_is_unreadable() {
    for field in [20, 24, 42] {
        let mut built = one_entry().build();
        let at = built.central_headers[0] + field;
        built.put_u32(at, u32::MAX);
        assert_source_unreadable(&built.bytes);
    }
    let mut built = one_entry().build();
    let at = built.central_headers[0] + 34;
    built.put_u16(at, u16::MAX);
    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_zip64_extra_that_is_too_short_is_unreadable() {
    let mut entry = TestEntry::stored("a.md", b"alpha");
    entry.extra = extra_field(0x0001, &5_u64.to_le_bytes());
    let mut built = TestZip::of(vec![entry]).build();
    for field in [20, 24] {
        let at = built.central_headers[0] + field;
        built.put_u32(at, u32::MAX);
    }

    assert_source_unreadable(&built.bytes);
}

#[test]
fn values_after_the_ones_needed_in_the_zip64_extra_are_not_read() {
    let mut entry = TestEntry::stored("a.md", b"alpha");
    let mut values = (entry.data.len() as u64).to_le_bytes().to_vec();
    values.extend_from_slice(&[0xFF; 8]);
    entry.extra = extra_field(0x0001, &values);
    let mut built = TestZip::of(vec![entry]).build();
    let at = built.central_headers[0] + 20;
    built.put_u32(at, u32::MAX);

    assert_eq!(content_at(&built.bytes, 0), b"alpha");
}

#[test]
fn a_disk_start_that_is_not_zero_is_unreadable() {
    let mut in_header = TestEntry::stored("a.md", b"alpha");
    in_header.disk_start = 1;
    assert_source_unreadable(&TestZip::of(vec![in_header]).build().bytes);

    let mut in_extra = TestEntry::stored("a.md", b"alpha");
    in_extra.disk_start = 1;
    in_extra.in_zip64 = vec![Zip64Field::DiskStart];
    assert_source_unreadable(&TestZip::of(vec![in_extra]).build().bytes);
}

// Sources that cannot be read: the local headers and the data.

#[test]
fn a_missing_local_header_signature_is_unreadable_even_for_a_directory() {
    let mut built = TestZip::of(vec![TestEntry::stored("dir/", b"")]).build();
    let at = built.local_headers[0];
    built.put_u8(at, 0);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn data_that_runs_into_the_directory_is_unreadable() {
    let mut built = one_entry().build();
    let at = built.central_headers[0] + 20;
    built.put_u32(at, 1_000);

    assert_source_unreadable(&built.bytes);
}

/// Two entries whose local headers are the same one.
fn overlapping(content: &[u8]) -> Built {
    let mut built = TestZip::of(vec![
        TestEntry::stored("a.md", content),
        TestEntry::stored("a.md", content),
    ])
    .build();
    let at = built.central_headers[1] + 42;
    built.put_u32(at, 0);
    built
}

#[test]
fn entries_whose_data_overlaps_are_unreadable() {
    assert_source_unreadable(&overlapping(b"alpha").bytes);
}

#[test]
fn entries_of_size_zero_do_not_overlap() {
    assert_eq!(list(&overlapping(b"").bytes).len(), 2);
}

#[test]
fn entries_side_by_side_do_not_overlap() {
    let bytes = two_entries().build().bytes;

    assert_eq!(list(&bytes).len(), 2);
}

#[test]
fn a_cut_zip_holding_an_empty_inner_zip_is_unreadable() {
    let inner = end_record(0, 0, 0);
    let mut built = TestZip::of(vec![
        TestEntry::stored("a.md", b"alpha"),
        TestEntry::stored("inner.zip", &inner),
    ])
    .build();
    let directory_start = built.directory_start;
    built.bytes.truncate(directory_start);

    assert_source_unreadable(&built.bytes);
}

// The size of the list.

#[test]
fn names_up_to_the_limit_exactly_are_listed() {
    let bytes = two_entries().build().bytes;

    assert_eq!(list_with_limit(&bytes, 8).unwrap().len(), 2);
}

#[test]
fn names_over_the_limit_stop_the_listing() {
    let bytes = two_entries().build().bytes;

    assert!(matches!(
        list_with_limit(&bytes, 7),
        Err(ListError::ListSizeExceeded)
    ));
}

#[test]
fn the_limit_counts_the_name_that_is_used() {
    let extra = unicode_path(1, crc32(b"cp932.md"), "a-longer-unicode-name.md");
    let bytes = entry_with_unicode_path(0, extra);

    assert!(matches!(
        list_with_limit(&bytes, 8),
        Err(ListError::ListSizeExceeded)
    ));
    assert!(list_with_limit(&bytes, 24).is_ok());
}

// Documents that cannot be read.

#[test]
fn a_wrong_uncompressed_size_makes_the_document_unreadable() {
    for mut entry in [
        TestEntry::stored("a.md", b"alpha"),
        TestEntry::deflated("a.md", REPEATED),
    ] {
        entry.uncompressed_size += 1;
        assert_document_unreadable(&TestZip::of(vec![entry]).build().bytes);
    }
}

#[test]
fn a_wrong_crc_makes_the_document_unreadable() {
    for mut entry in [
        TestEntry::stored("a.md", b"alpha"),
        TestEntry::deflated("a.md", REPEATED),
    ] {
        entry.crc32 ^= 1;
        assert_document_unreadable(&TestZip::of(vec![entry]).build().bytes);
    }
}

#[test]
fn an_unsupported_method_makes_the_document_unreadable() {
    let mut entry = TestEntry::stored("a.md", b"alpha");
    entry.method = 12;

    assert_document_unreadable(&TestZip::of(vec![entry]).build().bytes);
}

#[test]
fn an_encrypted_entry_makes_the_document_unreadable() {
    let mut entry = TestEntry::stored("a.md", b"alpha");
    entry.flags = FLAG_ENCRYPTED;

    assert_document_unreadable(&TestZip::of(vec![entry]).build().bytes);
}

#[test]
fn a_document_over_the_limit_is_unreadable() {
    for entry in [
        TestEntry::stored("a.md", REPEATED),
        TestEntry::deflated("a.md", REPEATED),
    ] {
        let bytes = TestZip::of(vec![entry]).build().bytes;
        let limit = REPEATED.len() as u64;

        assert!(read_at(&bytes, 0, limit - 1).2.is_err());
        assert_eq!(read_at(&bytes, 0, limit).2.unwrap(), REPEATED);
    }
}

#[test]
fn an_unreadable_document_does_not_stop_the_next_one() {
    let mut broken = TestEntry::stored("a.md", b"alpha");
    broken.crc32 ^= 1;
    let bytes = TestZip::of(vec![broken, TestEntry::stored("b.md", b"beta")])
        .build()
        .bytes;
    let mut contents = open(&bytes, NO_LIMIT);

    assert!(matches!(
        contents.read(Position(0)),
        ReadResult::Entry {
            content: Err(_),
            ..
        }
    ));
    assert!(matches!(
        contents.read(Position(1)),
        ReadResult::Entry { content: Ok(content), .. } if content == b"beta"
    ));
}

#[test]
fn a_position_past_the_last_entry_cannot_be_read_nor_anything_after_it() {
    let bytes = one_entry().build().bytes;

    assert!(matches!(
        open(&bytes, NO_LIMIT).read(Position(1)),
        ReadResult::RestUnreadable(_)
    ));
}

#[test]
fn a_directory_position_that_overflows_is_unreadable() {
    let mut built = TestZip {
        zip64: true,
        ..one_entry()
    }
    .build();
    let at = built.zip64_record + 48;
    built.put_u64(at, u64::MAX);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn bytes_too_few_for_a_digital_signature_are_unreadable() {
    let mut entry = TestEntry::stored("a.md", b"alpha");
    entry.extra = extra_field(0x1234, b"");
    entry.extra.extend_from_slice(&[1, 2, 3]);
    let mut built = TestZip::of(vec![entry]).build();
    let extra_length_at = built.central_headers[0] + 30;
    built.put_u16(extra_length_at, 4);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn a_local_header_past_the_directory_is_unreadable() {
    let mut built = one_entry().build();
    let at = built.central_headers[0] + 42;
    built.put_u32(at, built.directory_start as u32);

    assert_source_unreadable(&built.bytes);
}

#[test]
fn deflate_data_that_cannot_be_inflated_makes_the_document_unreadable() {
    let mut entry = TestEntry::deflated("a.md", REPEATED);
    entry.data = vec![0xFF; 16];

    assert_document_unreadable(&TestZip::of(vec![entry]).build().bytes);
}

#[test]
fn a_header_that_can_no_longer_be_read_stops_the_rest() {
    let large = vec![b'x'; 20_000];
    let built = TestZip::of(vec![
        TestEntry::stored("a.md", b"alpha"),
        TestEntry::stored("b.md", &large),
    ])
    .build();
    let file = file_with(&built.bytes);
    let mut writer = file.try_clone().unwrap();
    let Ok(mut contents) = zip_contents(file, NO_LIMIT) else {
        panic!("the second pass failed");
    };

    writer
        .seek(SeekFrom::Start(built.central_headers[0] as u64))
        .unwrap();
    writer.write_all(&[0]).unwrap();

    assert!(matches!(
        contents.read(Position(0)),
        ReadResult::RestUnreadable(_)
    ));
}
