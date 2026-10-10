use super::*;
use std::io::{BufRead, Cursor, Write};
use std::time::Duration;

use ::zip::CompressionMethod;
use ::zip::write::SimpleFileOptions;
use tempfile::TempDir;

use super::sparse::MAX_FRAGMENTS;

fn document(path: &str) -> EntryName {
    EntryName::Document(PathBuf::from(path))
}

// ---- name rules ----

#[test]
fn plain_document_names_keep_their_path() {
    assert_eq!(classify_name(b"docs/a.md"), document("docs/a.md"));
    assert_eq!(classify_name(b"A.MD"), document("A.MD"));
    assert_eq!(classify_name(b"notes.markdown"), document("notes.markdown"));
    assert_eq!(classify_name(b"paper.pdf"), document("paper.pdf"));
}

#[test]
fn names_without_an_ingestable_extension_are_ignored_even_when_not_utf8() {
    assert_eq!(classify_name(b"img.png"), EntryName::Ignored);
    assert_eq!(classify_name(b"README"), EntryName::Ignored);
    assert_eq!(classify_name(b"docs/.md"), EntryName::Ignored);
    assert_eq!(classify_name(b""), EntryName::Ignored);
    // Shift_JIS for "あ" followed by `.png`: not a document, so not counted.
    assert_eq!(classify_name(b"\x82\xa0.png"), EntryName::Ignored);
}

#[test]
fn non_utf8_document_names_are_rejected() {
    assert_eq!(
        classify_name(b"\x82\xa0.md"),
        EntryName::Rejected(NameProblem::NotUtf8)
    );
}

#[test]
fn macos_metadata_is_ignored_wherever_the_prefix_is_written() {
    for raw in [
        &b"__MACOSX/docs/._a.md"[..],
        b"./__MACOSX/a.md",
        b"././__MACOSX/a.md",
        b"__MACOSX\\docs\\a.md",
        b"__MACOSX/\x82\xa0.md",
        // `\` still separates `__MACOSX` from a name that is not UTF-8.
        b"__MACOSX\\\x82\xa0.md",
        b"__MACOSX/../a.md",
        b".//__MACOSX/a.md",
    ] {
        assert_eq!(classify_name(raw), EntryName::Ignored, "{raw:?}");
    }
    assert_eq!(
        classify_name(b"docs/__MACOSX/a.md"),
        document("docs/__MACOSX/a.md")
    );
}

#[test]
fn appledouble_files_next_to_documents_are_ignored() {
    for raw in [
        &b"._a.md"[..],
        b"docs/._a.md",
        b"docs\\._a.md",
        b"./docs/._a.pdf",
        b"docs/../._a.md",
        b"docs/._\x82\xa0.md",
    ] {
        assert_eq!(classify_name(raw), EntryName::Ignored, "{raw:?}");
    }
    assert_eq!(classify_name(b"._docs/a.md"), document("._docs/a.md"));
    assert_eq!(classify_name(b"docs/a._b.md"), document("docs/a._b.md"));
}

#[test]
fn backslashes_become_slashes_and_dot_and_empty_segments_are_dropped() {
    assert_eq!(classify_name(b"docs\\a.md"), document("docs/a.md"));
    assert_eq!(classify_name(b"./docs//a.md"), document("docs/a.md"));
    assert_eq!(classify_name(b"docs/./sub/a.md"), document("docs/sub/a.md"));
}

#[test]
fn unsafe_document_names_are_rejected() {
    for (raw, problem) in [
        (&b"/abs/a.md"[..], NameProblem::Absolute),
        (b"\\abs\\a.md", NameProblem::Absolute),
        (b"C:/a.md", NameProblem::Absolute),
        (b"c:a.md", NameProblem::Absolute),
        (b"../a.md", NameProblem::ParentDir),
        (b"docs/../a.md", NameProblem::ParentDir),
        (b"docs\\..\\a.md", NameProblem::ParentDir),
        (b"a\0b.md", NameProblem::ControlCharacter),
        (b"a\x1b[31mb.md", NameProblem::ControlCharacter),
        (b"docs\n/a.md", NameProblem::ControlCharacter),
        // U+009B, the one-character form of ESC `[`.
        (b"a\xc2\x9bb.md", NameProblem::ControlCharacter),
        // Only absolute once `./` is dropped.
        (b"./C:/a.md", NameProblem::Absolute),
        (b".\\c:x.md", NameProblem::Absolute),
    ] {
        assert_eq!(classify_name(raw), EntryName::Rejected(problem), "{raw:?}");
    }
}

#[test]
fn shift_jis_names_whose_bytes_contain_a_backslash_are_rejected_not_ignored() {
    // "ソ.md" in Shift_JIS: the second byte of ソ is 0x5C, the code of `\`.
    assert_eq!(
        classify_name(b"\x83\x5c.md"),
        EntryName::Rejected(NameProblem::NotUtf8)
    );
}

#[test]
fn archive_kind_is_decided_by_extension_ignoring_case() {
    let kind = |name: &str| ArchiveKind::from_path(Path::new(name));
    assert_eq!(kind("export.zip"), Some(ArchiveKind::Zip));
    assert_eq!(kind("EXPORT.ZIP"), Some(ArchiveKind::Zip));
    assert_eq!(kind("notes.tar"), Some(ArchiveKind::Tar));
    assert_eq!(kind("repo.tar.gz"), Some(ArchiveKind::TarGz));
    assert_eq!(kind("repo.TGZ"), Some(ArchiveKind::TarGz));
    assert_eq!(kind("data.gz"), None);
    assert_eq!(kind("notes.md"), None);
    assert_eq!(kind("notes"), None);
}

// ---- the listing ----

/// A listing built from `(position, name, is_regular_file)` entries.
fn listing_of(entries: &[(usize, &[u8], bool)]) -> ArchiveListing {
    let mut listing = PartialListing::new(MAX_LISTED_NAME_BYTES);
    for &(position, name, is_regular_file) in entries {
        listing
            .add(RawEntry {
                order: position as u64,
                position,
                name,
                is_regular_file,
            })
            .expect("add");
    }
    listing.finish(NO_FILE)
}

/// The stamp of a listing built without a file.
const NO_FILE: ArchiveStamp = ArchiveStamp {
    modified: SystemTime::UNIX_EPOCH,
    len: 0,
};

#[test]
fn listing_keeps_the_last_entry_per_path_in_archive_order() {
    let listing = listing_of(&[
        (0, b"b.md", true),
        (1, b"a.md", true),
        (2, b"dir/", false),
        (3, b"link.md", false),
        (4, b"./b.md", true),
        (5, b"\x82\xa0.md", true),
        (6, b"img.png", true),
    ]);
    let paths: Vec<(&Path, usize)> = listing
        .documents
        .iter()
        .map(|d| (d.path.as_path(), d.position))
        .collect();
    assert_eq!(paths, [(Path::new("a.md"), 1), (Path::new("b.md"), 4)]);
    assert_eq!(listing.rejected.len(), 1);
    assert_eq!(listing.rejected[0].problem, NameProblem::NotUtf8);
}

#[test]
fn the_names_a_listing_keeps_are_capped_counting_an_overhead_each() {
    // "a.md" costs 4 + 128 bytes; two fit in 300, a third does not.
    let mut listing = PartialListing::new(300);
    let add = |listing: &mut PartialListing, name: &[u8], position| {
        listing.add(RawEntry {
            order: position as u64,
            position,
            name,
            is_regular_file: true,
        })
    };
    add(&mut listing, b"a.md", 0).expect("first");
    add(&mut listing, b"b.md", 1).expect("second");
    // Not documents, and a path already kept, cost nothing.
    add(&mut listing, b"img.png", 2).expect("not a document");
    add(&mut listing, b"./a.md", 3).expect("same path");

    let err = add(&mut listing, b"c.md", 4).expect_err("over the limit");

    assert!(err.to_string().contains("names"), "{err}");
}

// ---- archives built in the test ----

fn write_file(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write archive");
    path
}

fn zip_bytes(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
    let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        if name.ends_with('/') {
            writer.add_directory(*name, options).expect("dir");
            continue;
        }
        writer.start_file(*name, options).expect("start");
        writer.write_all(data).expect("data");
    }
    writer.finish().expect("finish").into_inner()
}

/// Overwrite a little-endian u16 at `offset` past every header with
/// `signature`, for corrupting single-entry test archives.
fn patch_u16(bytes: &mut [u8], signature: &[u8; 4], offset: usize, value: u16) {
    let starts: Vec<usize> = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == signature)
        .map(|(i, _)| i)
        .collect();
    for start in starts {
        bytes[start + offset..start + offset + 2].copy_from_slice(&value.to_le_bytes());
    }
}

const LOCAL_HEADER: &[u8; 4] = b"PK\x03\x04";
const CENTRAL_HEADER: &[u8; 4] = b"PK\x01\x02";

/// A tar of `(name, type, data)` entries with GNU headers, ending with the
/// two zero blocks and the padding the `tar` crate's builder writes.
fn tar_bytes(entries: &[(&[u8], ::tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut builder = ::tar::Builder::new(Vec::new());
    for (name, kind, data) in entries {
        let mut header = ::tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_entry_type(*kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, *data).expect("append");
    }
    builder.into_inner().expect("finish tar")
}

/// One header block, with a size field of `size` whatever data follows.
fn header(name: &[u8], typeflag: u8, size: u64) -> Vec<u8> {
    let mut header = ::tar::Header::new_gnu();
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.as_old_mut().linkflag = [typeflag];
    header.set_size(size);
    header.set_mode(0o644);
    header.set_cksum();
    header.as_bytes().to_vec()
}

/// `data` padded to whole blocks.
fn padded(data: &[u8]) -> Vec<u8> {
    let mut bytes = data.to_vec();
    bytes.resize(data.len().div_ceil(512) * 512, 0);
    bytes
}

/// A header and its padded data.
fn member(name: &[u8], typeflag: u8, data: &[u8]) -> Vec<u8> {
    [header(name, typeflag, data.len() as u64), padded(data)].concat()
}

/// `parts` followed by the end-of-archive marker.
fn tar_of(parts: &[Vec<u8>]) -> Vec<u8> {
    [parts.concat(), vec![0; 1024]].concat()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip");
    encoder.finish().expect("finish gzip")
}

type Delivered = Vec<(PathBuf, Result<Vec<u8>, String>)>;

fn read_all(archive: &Path, kind: ArchiveKind, limit: u64) -> Delivered {
    let listing = list_documents(archive, kind).expect("list");
    let mut read = Vec::new();
    read_documents_capped(
        archive,
        kind,
        listing.stamp,
        &listing.documents,
        limit,
        |doc, result| {
            read.push((doc.path.clone(), result.map_err(|e| e.to_string())));
            true
        },
    )
    .expect("read");
    read
}

fn read_tar_bytes(dir: &TempDir, bytes: &[u8]) -> Delivered {
    let archive = write_file(dir, "x.tar", bytes);
    read_all(&archive, ArchiveKind::Tar, MAX_DOCUMENT_BYTES)
}

fn list_tar_bytes(dir: &TempDir, bytes: &[u8]) -> Result<ArchiveListing, ArchiveError> {
    let archive = write_file(dir, "x.tar", bytes);
    list_documents(&archive, ArchiveKind::Tar)
}

fn ok(path: &str, data: &[u8]) -> (PathBuf, Result<Vec<u8>, String>) {
    (PathBuf::from(path), Ok(data.to_vec()))
}

/// One PAX record, "<length> <key>=<value>\n", where the length counts
/// the whole record including its own digits.
fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let rest = key.len() + value.len() + 3;
    let mut length = rest;
    while length != rest + length.to_string().len() {
        length = rest + length.to_string().len();
    }
    let mut record = format!("{length} {key}=").into_bytes();
    record.extend_from_slice(value);
    record.push(b'\n');
    record
}

fn pax_records(records: &[(&str, &[u8])]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|(key, value)| pax_record(key, value))
        .collect()
}

#[test]
fn zip_documents_are_listed_and_read() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = zip_bytes(
        &[
            ("docs/", b""),
            ("docs/a.md", b"# A\n"),
            ("docs/img.png", b"\x89PNG"),
            ("docs/._a.md", b"\x00\x05\x16\x07"),
            ("__MACOSX/docs/._a.md", b"\x00\x05\x16\x07"),
            ("b.pdf", b"%PDF-1.4"),
        ],
        CompressionMethod::Deflated,
    );
    let archive = write_file(&dir, "x.zip", &bytes);

    let read = read_all(&archive, ArchiveKind::Zip, MAX_DOCUMENT_BYTES);

    assert_eq!(read, [ok("docs/a.md", b"# A\n"), ok("b.pdf", b"%PDF-1.4")]);
}

#[test]
fn tar_and_tar_gz_documents_are_listed_and_read_skipping_non_regular_entries() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = tar_bytes(&[
        (b"./docs/", ::tar::EntryType::Directory, b""),
        (b"./docs/a.md", ::tar::EntryType::Regular, b"first"),
        (b"./docs/link.md", ::tar::EntryType::Symlink, b""),
        (b"./docs/hard.md", ::tar::EntryType::Link, b""),
        (b"./docs/a.md", ::tar::EntryType::Regular, b"second"),
        (
            b"./docs/._a.md",
            ::tar::EntryType::Regular,
            b"\x00\x05\x16\x07",
        ),
    ]);
    let tar_path = write_file(&dir, "x.tar", &bytes);
    let tgz_path = write_file(&dir, "x.tgz", &gzip(&bytes));
    let expected = [ok("docs/a.md", b"second")];

    assert_eq!(
        read_all(&tar_path, ArchiveKind::Tar, MAX_DOCUMENT_BYTES),
        expected
    );
    assert_eq!(
        read_all(&tgz_path, ArchiveKind::TarGz, MAX_DOCUMENT_BYTES),
        expected
    );
}

#[test]
fn non_utf8_names_are_rejected_only_for_documents() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = tar_bytes(&[
        (b"\x82\xa0.md", ::tar::EntryType::Regular, b"x"),
        (b"\x82\xa0.png", ::tar::EntryType::Regular, b"x"),
        (b"ok.md", ::tar::EntryType::Regular, b"x"),
    ]);

    let listing = list_tar_bytes(&dir, &bytes).expect("list");

    assert_eq!(listing.documents.len(), 1);
    assert_eq!(listing.rejected.len(), 1);
    assert_eq!(listing.rejected[0].problem, NameProblem::NotUtf8);
}

#[test]
fn documents_over_the_limit_fail_individually() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = zip_bytes(
        &[("big.md", &[b'a'; 64]), ("small.md", b"ok")],
        CompressionMethod::Deflated,
    );
    let archive = write_file(&dir, "x.zip", &bytes);

    let read = read_all(&archive, ArchiveKind::Zip, 16);

    assert!(
        matches!(&read[0].1, Err(e) if e.contains("limit")),
        "{read:?}"
    );
    assert_eq!(read[1], ok("small.md", b"ok"));
}

#[test]
fn zip_entries_with_an_unsupported_method_fail_individually() {
    let dir = TempDir::new().expect("tempdir");
    let mut bytes = zip_bytes(&[("a.md", b"text")], CompressionMethod::Stored);
    // 12 = bzip2, which this build does not decompress.
    patch_u16(&mut bytes, LOCAL_HEADER, 8, 12);
    patch_u16(&mut bytes, CENTRAL_HEADER, 10, 12);
    let archive = write_file(&dir, "x.zip", &bytes);

    let read = read_all(&archive, ArchiveKind::Zip, MAX_DOCUMENT_BYTES);

    assert_eq!(read.len(), 1);
    assert!(read[0].1.is_err(), "{read:?}");
}

#[test]
fn encrypted_zip_entries_fail_individually() {
    let dir = TempDir::new().expect("tempdir");
    let mut bytes = zip_bytes(&[("a.md", b"text")], CompressionMethod::Stored);
    // General-purpose flag bit 0 marks the entry as encrypted.
    patch_u16(&mut bytes, LOCAL_HEADER, 6, 1);
    patch_u16(&mut bytes, CENTRAL_HEADER, 8, 1);
    let archive = write_file(&dir, "x.zip", &bytes);

    let read = read_all(&archive, ArchiveKind::Zip, MAX_DOCUMENT_BYTES);

    assert_eq!(read.len(), 1);
    assert!(read[0].1.is_err(), "{read:?}");
}

fn set_mtime(archive: &Path, mtime: SystemTime) {
    File::options()
        .write(true)
        .open(archive)
        .and_then(|f| f.set_modified(mtime))
        .expect("set mtime");
}

/// Overwrite `archive` with `bytes` of the same length and give it back its
/// mtime, so the stamp the listing took still matches: only the checks on
/// the entries themselves can catch such a replacement.
fn replace_keeping_stamp(archive: &Path, bytes: &[u8]) {
    let before = std::fs::metadata(archive).expect("metadata");
    assert_eq!(
        before.len(),
        bytes.len() as u64,
        "the replacement must keep the size"
    );
    std::fs::write(archive, bytes).expect("replace");
    set_mtime(archive, before.modified().expect("mtime"));
}

fn read_after_replacing(
    dir: &TempDir,
    kind: ArchiveKind,
    first: &[u8],
    second: &[u8],
) -> Delivered {
    let name = match kind {
        ArchiveKind::Zip => "x.zip",
        ArchiveKind::Tar => "x.tar",
        ArchiveKind::TarGz => "x.tgz",
    };
    let archive = write_file(dir, name, first);
    let listing = list_documents(&archive, kind).expect("list");
    replace_keeping_stamp(&archive, second);
    let mut read = Vec::new();
    read_documents(
        &archive,
        kind,
        listing.stamp,
        &listing.documents,
        |doc, result| {
            read.push((doc.path.clone(), result.map_err(|e| e.to_string())));
            true
        },
    )
    .expect("read");
    read
}

#[test]
fn a_document_whose_entry_changed_between_passes_fails() {
    let dir = TempDir::new().expect("tempdir");
    let first = tar_bytes(&[(b"a.md", ::tar::EntryType::Regular, b"x")]);
    let renamed = tar_bytes(&[(b"b.md", ::tar::EntryType::Regular, b"y")]);
    // A block short without the data; a zero block after the end marker
    // is not read.
    let now_a_link = [
        tar_bytes(&[(b"a.md", ::tar::EntryType::Symlink, b"")]),
        vec![0; 512],
    ]
    .concat();
    for second in [renamed, now_a_link] {
        let read = read_after_replacing(&dir, ArchiveKind::Tar, &first, &second);

        assert_eq!(read.len(), 1);
        assert!(
            matches!(&read[0].1, Err(e) if e.contains("changed")),
            "{read:?}"
        );
    }
}

#[test]
fn a_zip_document_replaced_by_a_link_between_passes_fails() {
    let dir = TempDir::new().expect("tempdir");
    let first = hand_zip(&[(b"a.md", b"elsewhere.md")], b"");
    // The same entry, marked a Unix symbolic link in its external
    // attributes.
    let mut second = first.clone();
    let central = second
        .windows(4)
        .position(|w| w == CENTRAL_HEADER)
        .expect("central header");
    second[central + 38..central + 42].copy_from_slice(&(0o120_777u32 << 16).to_le_bytes());

    let read = read_after_replacing(&dir, ArchiveKind::Zip, &first, &second);

    assert_eq!(read.len(), 1);
    assert!(
        matches!(&read[0].1, Err(e) if e.contains("changed")),
        "{read:?}"
    );
}

#[test]
fn a_document_cut_short_by_a_replacement_after_listing_fails() {
    let dir = TempDir::new().expect("tempdir");
    let data = [b'b'; 1000];
    let first = tar_of(&[member(b"a.md", b'0', b"x"), member(b"b.md", b'0', &data)]);
    // As long as the first, but `b.md` now runs past the end of the file.
    let second = tar_of(&[
        member(b"a.md", b'0', b"x"),
        header(b"b.md", b'0', 5000),
        padded(&data),
    ]);

    let read = read_after_replacing(&dir, ArchiveKind::Tar, &first, &second);

    assert_eq!(read[0], ok("a.md", b"x"));
    assert!(
        matches!(&read[1].1, Err(e) if e.contains("cut short")),
        "{read:?}"
    );
}

// ---- an archive rewritten while it is read ----

/// Give `archive` a later mtime, its bytes unchanged.
fn touch(archive: &Path) {
    let mtime = std::fs::metadata(archive)
        .and_then(|m| m.modified())
        .expect("mtime");
    set_mtime(archive, mtime + Duration::from_secs(60));
}

/// Add a byte after the end of `archive`, keeping its mtime. A tar and a
/// tar.gz are not read past their end, so only the size tells.
fn lengthen(archive: &Path) {
    let mtime = std::fs::metadata(archive)
        .and_then(|m| m.modified())
        .expect("mtime");
    File::options()
        .append(true)
        .open(archive)
        .and_then(|mut f| f.write_all(b"\0"))
        .expect("append");
    set_mtime(archive, mtime);
}

/// A way to rewrite an archive, and what it changes.
type Rewrite = (&'static str, fn(&Path));

const REWRITES: [Rewrite; 2] = [("mtime", touch), ("size", lengthen)];

/// Two documents, `a.md` and `b.md`, as a zip, a tar and a tar.gz.
fn two_documents_in_each_kind(dir: &TempDir) -> [(PathBuf, ArchiveKind); 3] {
    let zip = zip_bytes(&[("a.md", b"A"), ("b.md", b"B")], CompressionMethod::Stored);
    let tar = tar_of(&[member(b"a.md", b'0', b"A"), member(b"b.md", b'0', b"B")]);
    [
        (write_file(dir, "x.zip", &zip), ArchiveKind::Zip),
        (write_file(dir, "x.tar", &tar), ArchiveKind::Tar),
        (write_file(dir, "x.tgz", &gzip(&tar)), ArchiveKind::TarGz),
    ]
}

/// Reads `file`, running `rewrite` right before the first read.
struct RewrittenOnFirstRead<'a, F> {
    file: &'a File,
    rewrite: Option<F>,
}

impl<F: FnOnce()> Read for RewrittenOnFirstRead<'_, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(rewrite) = self.rewrite.take() {
            rewrite();
        }
        let mut file = self.file;
        file.read(buf)
    }
}

impl<F> Seek for RewrittenOnFirstRead<'_, F> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let mut file = self.file;
        file.seek(pos)
    }
}

#[test]
fn an_archive_rewritten_while_it_is_listed_is_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    for (archive, kind) in two_documents_in_each_kind(&dir) {
        for (change, rewrite) in REWRITES {
            let file = File::open(&archive).expect("open");
            let reader = RewrittenOnFirstRead {
                file: &file,
                rewrite: Some(|| rewrite(&archive)),
            };

            let err =
                list_unchanged(&file, reader, kind, MAX_LISTED_NAME_BYTES).expect_err("must fail");

            assert!(
                err.to_string().contains("changed while"),
                "{kind:?}, {change}: {err}"
            );
        }
    }
}

#[test]
fn an_archive_listed_unchanged_is_stamped_with_its_mtime_and_size() {
    let dir = TempDir::new().expect("tempdir");
    for (archive, kind) in two_documents_in_each_kind(&dir) {
        let metadata = std::fs::metadata(&archive).expect("metadata");

        let listing = list_documents(&archive, kind).expect("list");

        assert_eq!(
            listing.stamp,
            ArchiveStamp {
                modified: metadata.modified().expect("mtime"),
                len: metadata.len(),
            },
            "{kind:?}"
        );
    }
}

/// Read every listed document of `archive`, running `on_delivery` after
/// each is handed over; returns the paths handed over and the outcome.
fn read_listed(
    archive: &Path,
    kind: ArchiveKind,
    listing: &ArchiveListing,
    on_delivery: impl Fn(),
) -> (Vec<PathBuf>, Result<(), ArchiveError>) {
    let mut delivered = Vec::new();
    let result = read_documents(
        archive,
        kind,
        listing.stamp,
        &listing.documents,
        |doc, _| {
            delivered.push(doc.path.clone());
            on_delivery();
            true
        },
    );
    (delivered, result)
}

/// Replace `archive` with a few bytes that are no archive at all.
fn replace_with_junk(archive: &Path) {
    std::fs::write(archive, b"gone").expect("replace");
}

#[test]
fn an_archive_rewritten_before_the_second_pass_hands_nothing_over() {
    let dir = TempDir::new().expect("tempdir");
    // Junk would fail the walk with a reason of its own, so its reason
    // shows the stamp is checked as soon as the archive is opened.
    let rewrites: [Rewrite; 3] = [REWRITES[0], REWRITES[1], ("junk", replace_with_junk)];
    for (archive, kind) in two_documents_in_each_kind(&dir) {
        for (change, rewrite) in rewrites {
            let listing = list_documents(&archive, kind).expect("list");
            rewrite(&archive);

            let (delivered, result) = read_listed(&archive, kind, &listing, || {});

            let err = result.expect_err("must fail");
            assert!(
                err.reason.contains("changed while"),
                "{kind:?}, {change}: {err}"
            );
            assert!(delivered.is_empty(), "{kind:?}, {change}: {delivered:?}");
        }
    }
}

#[test]
fn an_archive_rewritten_during_the_second_pass_hands_nothing_more_over() {
    let dir = TempDir::new().expect("tempdir");
    for (archive, kind) in two_documents_in_each_kind(&dir) {
        for (change, rewrite) in REWRITES {
            let listing = list_documents(&archive, kind).expect("list");

            let (delivered, result) = read_listed(&archive, kind, &listing, || rewrite(&archive));

            let err = result.expect_err("must fail");
            assert!(
                err.reason.contains("changed while"),
                "{kind:?}, {change}: {err}"
            );
            assert_eq!(delivered, [PathBuf::from("a.md")], "{kind:?}, {change}");
        }
    }
}

// ---- tar: extension headers and PAX records ----

#[test]
fn tar_gnu_long_names_and_pax_paths_are_resolved() {
    let dir = TempDir::new().expect("tempdir");
    let long = format!("{}/doc.md", "d".repeat(120));
    let long_name = format!("{long}\0");
    let bytes = tar_bytes(&[
        (
            b"././@LongLink",
            ::tar::EntryType::GNULongName,
            long_name.as_bytes(),
        ),
        (b"short.md", ::tar::EntryType::Regular, b"from long name"),
        (
            b"PaxHeader",
            ::tar::EntryType::XHeader,
            &pax_record("path", b"pax/long.md"),
        ),
        (b"short2.md", ::tar::EntryType::Regular, b"from pax"),
        (b"plain.md", ::tar::EntryType::Regular, b"plain"),
    ]);

    let read = read_tar_bytes(&dir, &bytes);

    assert_eq!(
        read,
        [
            ok(&long, b"from long name"),
            ok("pax/long.md", b"from pax"),
            ok("plain.md", b"plain"),
        ]
    );
}

#[test]
fn a_pax_header_holding_large_attributes_is_read_past() {
    let dir = TempDir::new().expect("tempdir");
    // macOS `tar` keeps a file's extended attributes in its PAX header,
    // which takes far more than a path once they are large.
    let pax = pax_records(&[
        ("SCHILY.xattr.com.example.big", &[b'x'; 300_000]),
        ("path", b"docs/a.md"),
    ]);
    let bytes = tar_bytes(&[
        (b"PaxHeader", ::tar::EntryType::XHeader, &pax),
        (b"a.md", ::tar::EntryType::Regular, b"text"),
    ]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("docs/a.md", b"text")]);
}

#[test]
fn pax_records_are_split_by_their_lengths_not_at_newlines() {
    let dir = TempDir::new().expect("tempdir");
    // A raw attribute value may hold newlines, even end in one.
    let pax = pax_records(&[
        ("SCHILY.xattr.com.example.bin", b"a\nb\n"),
        ("path", b"docs/a.md"),
    ]);
    let bytes = tar_bytes(&[
        (b"PaxHeader", ::tar::EntryType::XHeader, &pax),
        (b"a.md", ::tar::EntryType::Regular, b"text"),
    ]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("docs/a.md", b"text")]);
}

#[test]
fn malformed_pax_headers_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let mut nul_before_a_record = vec![0u8; 4];
    nul_before_a_record.extend(pax_record("path", b"docs/a.md"));
    let mut bad_after_comment = pax_record("comment", b"fine");
    bad_after_comment.extend(b"9 x y z\n");
    for (name, pax) in [
        // The length runs past the header's end.
        ("too-long", &b"99 path=a.md\n"[..]),
        // The length is right but the record has no `=`.
        ("no-equals", b"13 path a.md\n"),
        // Digits only: Rust's own parser would take a leading `+`.
        ("plus-length", b"+14 path=a.md\n"),
        // Only a NUL tail is let through, not NULs ahead of a record.
        ("nul-before-a-record", &nul_before_a_record),
        // A record the walk does not use must still parse.
        ("bad-after-comment", &bad_after_comment),
    ] {
        let bytes = tar_bytes(&[
            (b"PaxHeader", ::tar::EntryType::XHeader, pax),
            (b"a.md", ::tar::EntryType::Regular, b"text"),
        ]);
        let err = list_tar_bytes(&dir, &bytes).expect_err(name);
        assert!(err.reason.contains("malformed PAX record"), "{name}: {err}");
    }
}

#[test]
fn pax_sizes_that_are_not_numbers_or_empty_values_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    for (name, pax) in [
        ("size-not-a-number", pax_record("size", b"abc")),
        ("plus-size", pax_record("size", b"+4")),
        ("size-too-large", pax_record("size", b"9223372036854775808")),
        // 2^64, which an unchecked last addition would wrap around to 0.
        (
            "size-wraps-around",
            pax_record("size", b"18446744073709551616"),
        ),
        // An empty value clears the header's field in POSIX, leaving no
        // name or no size.
        ("empty-size", pax_record("size", b"")),
        ("empty-path", pax_record("path", b"")),
    ] {
        let bytes = tar_bytes(&[
            (b"PaxHeader", ::tar::EntryType::XHeader, &pax),
            (b"a.md", ::tar::EntryType::Regular, b"text"),
        ]);
        assert!(list_tar_bytes(&dir, &bytes).is_err(), "{name}");
    }
}

#[test]
fn the_last_pax_path_and_size_win_and_nul_bytes_after_the_records_are_let_through() {
    let dir = TempDir::new().expect("tempdir");
    let mut pax = pax_records(&[
        ("path", b"first.md"),
        ("size", b"9999"),
        ("path", b"docs/a.md"),
        ("size", b"4"),
    ]);
    pax.extend([0u8; 12]);
    let bytes = tar_bytes(&[
        (b"PaxHeader", ::tar::EntryType::XHeader, &pax),
        (b"a.md", ::tar::EntryType::Regular, b"text"),
    ]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("docs/a.md", b"text")]);
}

#[test]
fn a_pax_size_decides_where_an_entrys_data_ends() {
    let dir = TempDir::new().expect("tempdir");
    // As Python's `tarfile` writes a member of 8 GiB or more: the header
    // says 0 and only the PAX record has the real size.
    let bytes = tar_of(&[
        member(b"PaxHeader", b'x', &pax_record("size", b"600")),
        header(b"big.md", b'0', 0),
        padded(&[b'a'; 600]),
        member(b"after.md", b'0', b"after"),
    ]);

    let read = read_tar_bytes(&dir, &bytes);

    assert_eq!(read, [ok("big.md", &[b'a'; 600]), ok("after.md", b"after")]);
}

#[test]
fn extension_headers_that_leave_a_name_or_size_unsettled_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let path = pax_record("path", b"pax/long.md");
    for (name, parts) in [
        (
            "two-pax-headers",
            vec![
                member(b"PaxHeader", b'x', &pax_record("size", b"4")),
                member(b"PaxHeader", b'x', &path),
            ],
        ),
        (
            "two-long-names",
            vec![
                member(b"././@LongLink", b'L', b"x.md\0"),
                member(b"././@LongLink", b'L', b"y.md\0"),
            ],
        ),
        (
            "two-long-link-names",
            vec![
                member(b"././@LongLink", b'K', b"x\0"),
                member(b"././@LongLink", b'K', b"y\0"),
            ],
        ),
        (
            "long-name-and-pax-path",
            vec![
                member(b"././@LongLink", b'L', b"x.md\0"),
                member(b"PaxHeader", b'x', &path),
            ],
        ),
    ] {
        let mut parts = parts;
        parts.push(member(b"a.md", b'0', b"text"));
        let err = list_tar_bytes(&dir, &tar_of(&parts)).expect_err(name);
        assert!(
            err.reason.contains("same kind") || err.reason.contains("not settled"),
            "{name}: {err}"
        );
    }
}

#[test]
fn extension_headers_with_no_entry_before_the_end_marker_are_dropped() {
    let dir = TempDir::new().expect("tempdir");
    for extension in [
        member(b"PaxHeader", b'x', &pax_record("path", b"pax/long.md")),
        member(b"././@LongLink", b'L', b"x.md\0"),
        member(b"././@LongLink", b'K', b"x\0"),
    ] {
        let bytes = tar_of(&[member(b"a.md", b'0', b"text"), extension]);

        assert_eq!(read_tar_bytes(&dir, &bytes), [ok("a.md", b"text")]);
    }
}

#[test]
fn global_pax_headers_may_carry_records_that_apply_to_no_one_entry() {
    let dir = TempDir::new().expect("tempdir");
    // `git archive` puts the commit in a global `comment`.
    let bytes = tar_of(&[
        member(
            b"pax_global_header",
            b'g',
            &pax_record("comment", b"0123abcd"),
        ),
        member(b"a.md", b'0', b"text"),
    ]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("a.md", b"text")]);
}

#[test]
fn global_pax_headers_with_a_path_size_or_sparse_record_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    for (key, value) in [
        ("path", &b"global.md"[..]),
        ("path", b""),
        ("size", b"4"),
        ("GNU.sparse.name", b"a.md"),
    ] {
        let bytes = tar_of(&[
            member(b"pax_global_header", b'g', &pax_record(key, value)),
            member(b"a.md", b'0', b"text"),
        ]);
        let err = list_tar_bytes(&dir, &bytes).expect_err(key);
        assert!(err.reason.contains("global"), "{key}: {err}");
    }
}

#[test]
fn an_empty_long_name_is_no_document_and_a_nul_in_a_path_is_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = tar_of(&[
        member(b"././@LongLink", b'L', b"\0"),
        member(b"a.md", b'0', b"text"),
        member(b"PaxHeader", b'x', &pax_record("path", b"b\0c.md")),
        member(b"b.md", b'0', b"text"),
    ]);

    let listing = list_tar_bytes(&dir, &bytes).expect("list");

    assert!(listing.documents.is_empty(), "{listing:?}");
    assert_eq!(listing.rejected.len(), 1);
    assert_eq!(listing.rejected[0].problem, NameProblem::ControlCharacter);
}

#[test]
fn the_prefix_is_used_only_in_ustar_headers() {
    let dir = TempDir::new().expect("tempdir");
    let with_prefix = |magic: &[u8; 6], version: &[u8; 2]| {
        let mut header = ::tar::Header::new_ustar();
        header.as_old_mut().name[..4].copy_from_slice(b"a.md");
        header.set_size(4);
        header.set_mode(0o644);
        let ustar = header.as_ustar_mut().expect("ustar");
        ustar.prefix[..4].copy_from_slice(b"docs");
        ustar.magic = *magic;
        ustar.version = *version;
        header.set_cksum();
        tar_of(&[header.as_bytes().to_vec(), padded(b"text")])
    };

    assert_eq!(
        read_tar_bytes(&dir, &with_prefix(b"ustar\0", b"00")),
        [ok("docs/a.md", b"text")]
    );
    assert_eq!(
        read_tar_bytes(&dir, &with_prefix(b"ustar\0", b"  ")),
        [ok("a.md", b"text")]
    );
}

// ---- tar: headers, types and the end of the archive ----

#[test]
fn links_devices_directories_and_fifos_carry_no_data_whatever_their_size() {
    let dir = TempDir::new().expect("tempdir");
    for typeflag in b'2'..=b'6' {
        // A size field of 5 with no data after it, or one that is not a
        // number: either way the next header follows.
        let mut size_not_a_number = header(b"odd.md", typeflag, 0);
        size_not_a_number[124..136].copy_from_slice(b"zzzzzzzzzzz\0");
        for odd in [
            header(b"odd.md", typeflag, 5),
            fix_checksum(size_not_a_number),
        ] {
            let bytes = tar_of(&[odd, member(b"a.md", b'0', b"text")]);

            assert_eq!(
                read_tar_bytes(&dir, &bytes),
                [ok("a.md", b"text")],
                "type {}",
                typeflag as char
            );
        }
    }
}

#[test]
fn hard_links_that_carry_data_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let in_header = tar_of(&[
        member(b"hard.md", b'1', b"data"),
        member(b"a.md", b'0', b"text"),
    ]);
    let in_pax = tar_of(&[
        member(b"PaxHeader", b'x', &pax_record("size", b"4")),
        header(b"hard.md", b'1', 0),
        padded(b"data"),
        member(b"a.md", b'0', b"text"),
    ]);

    for bytes in [in_header, in_pax] {
        let err = list_tar_bytes(&dir, &bytes).expect_err("must fail");
        assert!(err.reason.contains("hard link"), "{err}");
    }
}

#[test]
fn other_types_are_stepped_over_by_their_size() {
    let dir = TempDir::new().expect("tempdir");
    // GNU tar's incremental directory record carries data.
    let bytes = tar_of(&[
        member(b"dir.md", b'D', b"Ya.md\0\0"),
        member(b"a.md", b'0', b"text"),
    ]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("a.md", b"text")]);
}

#[test]
fn contiguous_tar_entries_are_read_as_regular_files() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = tar_bytes(&[(b"a.md", ::tar::EntryType::Continuous, b"text")]);

    assert_eq!(read_tar_bytes(&dir, &bytes), [ok("a.md", b"text")]);
}

#[test]
fn damaged_headers_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let mut wrong_checksum = member(b"a.md", b'0', b"text");
    wrong_checksum[148] = b'7';
    let mut size_not_a_number = header(b"a.md", b'0', 4);
    size_not_a_number[124..136].copy_from_slice(b"zzzzzzzzzzz\0");
    let mut size_with_a_sign = header(b"a.md", b'0', 4);
    size_with_a_sign[124..136].copy_from_slice(b"-0000000004\0");
    for (name, block) in [
        ("wrong-checksum", wrong_checksum),
        ("size-not-a-number", fix_checksum(size_not_a_number)),
        ("size-with-a-sign", fix_checksum(size_with_a_sign)),
    ] {
        let bytes = tar_of(&[block, padded(b"text")]);
        assert!(list_tar_bytes(&dir, &bytes).is_err(), "{name}");
    }
}

/// Recompute a header block's checksum after editing it.
fn fix_checksum(mut block: Vec<u8>) -> Vec<u8> {
    block[148..156].copy_from_slice(b"        ");
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    block
}

#[test]
fn archive_errors_carry_no_control_characters() {
    let dir = TempDir::new().expect("tempdir");
    let mut block = header(b"a\x1b[2J.md", b'0', 4);
    block[124..136].copy_from_slice(b"zzzzzzzzzzz\0");
    let bytes = tar_of(&[fix_checksum(block)]);

    let err = list_tar_bytes(&dir, &bytes).expect_err("must fail");

    assert!(!err.reason.contains('\x1b'), "{err}");
}

#[test]
fn documents_named_with_control_characters_are_rejected_with_the_name_escaped() {
    let dir = TempDir::new().expect("tempdir");
    let zip = write_file(
        &dir,
        "x.zip",
        &zip_bytes(
            &[("a\x1b[31m.md", b"x"), ("ok.md", b"x")],
            CompressionMethod::Deflated,
        ),
    );
    let tar = write_file(
        &dir,
        "x.tar",
        &tar_bytes(&[
            (b"a\x1b[31m.md", ::tar::EntryType::Regular, b"x"),
            (b"ok.md", ::tar::EntryType::Regular, b"x"),
        ]),
    );

    for (archive, kind) in [(zip, ArchiveKind::Zip), (tar, ArchiveKind::Tar)] {
        let listing = list_documents(&archive, kind).expect("list");
        let paths: Vec<&Path> = listing.documents.iter().map(|d| d.path.as_path()).collect();
        assert_eq!(paths, [Path::new("ok.md")], "{kind:?}");
        assert_eq!(
            listing.rejected,
            [RejectedDocument {
                name: "a\\u{1b}[31m.md".to_string(),
                problem: NameProblem::ControlCharacter,
            }],
            "{kind:?}"
        );
    }
}

#[test]
fn missing_or_truncated_archives_cannot_be_listed() {
    let dir = TempDir::new().expect("tempdir");
    let tar = tar_bytes(&[
        (b"a.md", ::tar::EntryType::Regular, &[b'a'; 2048]),
        (b"b.md", ::tar::EntryType::Regular, &[b'b'; 2048]),
    ]);
    let gz = gzip(&tar);
    let truncated = write_file(&dir, "cut.tgz", &gz[..gz.len() / 2]);
    let not_zip = write_file(&dir, "bad.zip", b"not a zip");
    let mid_block = write_file(&dir, "mid-block.tar", &tar[..700]);

    assert!(list_documents(&dir.path().join("missing.zip"), ArchiveKind::Zip).is_err());
    assert!(list_documents(&truncated, ArchiveKind::TarGz).is_err());
    assert!(list_documents(&not_zip, ArchiveKind::Zip).is_err());
    assert!(list_documents(&mid_block, ArchiveKind::Tar).is_err());
}

#[test]
fn tars_must_end_with_two_zero_blocks() {
    let dir = TempDir::new().expect("tempdir");
    let entries = [member(b"a.md", b'0', b"x"), member(b"b.md", b'0', b"y")].concat();
    let one_zero_block = [entries.clone(), vec![0; 512]].concat();
    let zero_block_then_entry = [
        member(b"a.md", b'0', b"x"),
        vec![0; 512],
        member(b"b.md", b'0', b"y"),
        vec![0; 1024],
    ]
    .concat();
    for (name, bytes, reason) in [
        ("empty", &b""[..], "end-of-archive marker"),
        ("no-marker", &entries, "end-of-archive marker"),
        ("cut-after-a", &entries[..1024], "end-of-archive marker"),
        ("one-zero-block", &one_zero_block, "end-of-archive marker"),
        (
            "zero-block-then-entry",
            &zero_block_then_entry,
            "zero block",
        ),
    ] {
        let err = list_tar_bytes(&dir, bytes).expect_err(name);
        assert!(err.reason.contains(reason), "{name}: {err}");
    }
    let tgz = write_file(&dir, "no-marker.tgz", &gzip(&entries));
    let err = list_documents(&tgz, ArchiveKind::TarGz).expect_err("must fail");
    assert!(err.reason.contains("end-of-archive marker"), "{err}");
}

#[test]
fn nothing_after_the_end_of_archive_marker_is_read() {
    let dir = TempDir::new().expect("tempdir");
    let first = tar_bytes(&[(b"a.md", ::tar::EntryType::Regular, b"x")]);
    let second = tar_bytes(&[(b"b.md", ::tar::EntryType::Regular, b"y")]);
    let garbage = b"not a tar header".repeat(64);

    for after in [second, garbage] {
        let bytes = [first.clone(), after].concat();
        assert_eq!(read_tar_bytes(&dir, &bytes), [ok("a.md", b"x")]);
    }
}

// ---- tar: the name limit ----

#[test]
fn a_tar_whose_document_names_pass_the_limit_is_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = tar_of(&[
        member(b"a.md", b'0', b"x"),
        member(b"b.md", b'0', b"x"),
        member(b"c.md", b'0', b"x"),
    ]);
    let archive = write_file(&dir, "x.tar", &bytes);

    let err = list_documents_limited(&archive, ArchiveKind::Tar, 300).expect_err("over");

    assert!(err.reason.contains("names"), "{err}");
    assert!(list_documents_limited(&archive, ArchiveKind::Tar, 400).is_ok());
}

#[test]
fn a_zip_whose_document_names_pass_the_limit_is_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let entries: [(&[u8], &[u8]); 3] = [(b"a.md", b"x"), (b"b.md", b"x"), (b"c.md", b"x")];
    let archive = write_file(&dir, "x.zip", &hand_zip(&entries, b""));

    let err = list_documents_limited(&archive, ArchiveKind::Zip, 300).expect_err("over");

    assert!(err.reason.contains("names"), "{err}");
    assert!(list_documents_limited(&archive, ArchiveKind::Zip, 400).is_ok());
}

#[test]
fn names_with_millions_of_segments_normalize_to_the_path_they_name() {
    let mut raw = "./".repeat(1_000_000).into_bytes();
    raw.extend(b"docs\\\\a.md");

    let EntryName::Document(path) = classify_name(&raw) else {
        panic!("not a document");
    };

    assert_eq!(path, Path::new("docs/a.md"));
    // The listing counts the path's length, so it must not keep the long
    // name's allocation.
    assert!(path.capacity() < 64, "capacity {}", path.capacity());
}

#[test]
fn names_that_are_not_documents_do_not_count_toward_the_limit() {
    let dir = TempDir::new().expect("tempdir");
    let parts: Vec<Vec<u8>> = (0..20)
        .map(|i| member(format!("img-{i}.png").as_bytes(), b'0', b"x"))
        .collect();
    let archive = write_file(&dir, "x.tar", &tar_of(&parts));

    assert!(list_documents_limited(&archive, ArchiveKind::Tar, 300).is_ok());
}

#[test]
fn long_names_being_read_stop_at_the_limit() {
    let dir = TempDir::new().expect("tempdir");
    let long = format!("{}.md", "n".repeat(400));
    for (name, extension) in [
        ("long-name", member(b"././@LongLink", b'L', long.as_bytes())),
        (
            "pax-path",
            member(b"PaxHeader", b'x', &pax_record("path", long.as_bytes())),
        ),
        (
            "sparse-name",
            member(
                b"PaxHeader",
                b'x',
                &pax_record("GNU.sparse.name", long.as_bytes()),
            ),
        ),
    ] {
        let archive = write_file(
            &dir,
            "x.tar",
            &tar_of(&[extension, member(b"a.md", b'0', b"x")]),
        );
        let err = list_documents_limited(&archive, ArchiveKind::Tar, 300).expect_err(name);
        assert!(err.reason.contains("names"), "{name}: {err}");
    }
}

// ---- tar: sparse files ----

fn octal12(n: u64) -> [u8; 12] {
    format!("{n:011o}\0")
        .into_bytes()
        .try_into()
        .expect("12 bytes")
}

/// An old GNU sparse header (type `S`) with up to four slots.
fn old_sparse_header(
    name: &[u8],
    real_size: u64,
    slots: &[(u64, u64)],
    extended: u8,
    stored: u64,
) -> Vec<u8> {
    let mut header = ::tar::Header::new_gnu();
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_entry_type(::tar::EntryType::GNUSparse);
    header.set_size(stored);
    header.set_mode(0o644);
    let gnu = header.as_gnu_mut().expect("gnu");
    for (slot, &(offset, length)) in gnu.sparse.iter_mut().zip(slots) {
        slot.offset = octal12(offset);
        slot.numbytes = octal12(length);
    }
    gnu.isextended = [extended];
    gnu.realsize = octal12(real_size);
    header.set_cksum();
    header.as_bytes().to_vec()
}

/// An old GNU sparse extension block with up to 21 slots.
fn sparse_extension(slots: &[(u64, u64)], extended: u8) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    for (i, &(offset, length)) in slots.iter().enumerate() {
        block[i * 24..i * 24 + 12].copy_from_slice(&octal12(offset));
        block[i * 24 + 12..i * 24 + 24].copy_from_slice(&octal12(length));
    }
    block[504] = extended;
    block
}

/// The file the sparse tests store: "ab" at 0, "cd" at 1024, "e" at 3000
/// in 3001 bytes.
fn sparse_file() -> Vec<u8> {
    let mut file = vec![0u8; 3001];
    file[..2].copy_from_slice(b"ab");
    file[1024..1026].copy_from_slice(b"cd");
    file[3000] = b'e';
    file
}

const SPARSE_SLOTS: [(u64, u64); 3] = [(0, 2), (1024, 2), (3000, 1)];

#[test]
fn old_gnu_sparse_files_are_rebuilt_with_or_without_extension_blocks() {
    let dir = TempDir::new().expect("tempdir");
    let in_header = tar_of(&[
        old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 0, 5),
        padded(b"abcde"),
        member(b"after.md", b'0', b"after"),
    ]);
    // Four slots fill the header; the map then ends in an extension block.
    // GNU tar writes 1 for "another block follows"; any nonzero value is
    // read that way.
    let mut five = SPARSE_SLOTS.to_vec();
    five.splice(2..2, [(2000, 0), (2500, 0)]);
    let extended = |flag| {
        tar_of(&[
            old_sparse_header(b"s.md", 3001, &five[..4], flag, 5),
            sparse_extension(&five[4..], 0),
            padded(b"abcde"),
            member(b"after.md", b'0', b"after"),
        ])
    };

    for bytes in [in_header, extended(1), extended(2)] {
        assert_eq!(
            read_tar_bytes(&dir, &bytes),
            [ok("s.md", &sparse_file()), ok("after.md", b"after")]
        );
    }
}

#[test]
fn old_sparse_maps_whose_layout_is_unsettled_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    // Type `S` in a header that is not in the GNU format.
    let mut not_gnu = old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 0, 5);
    not_gnu[257..265].copy_from_slice(b"ustar\x0000");
    // The map ends in the header, which still says a block follows.
    let ended_but_extended = old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 1, 5);
    for (name, block) in [
        ("not-gnu", fix_checksum(not_gnu)),
        ("ended-but-extended", ended_but_extended),
    ] {
        let bytes = tar_of(&[block, padded(b"abcde")]);
        assert!(list_tar_bytes(&dir, &bytes).is_err(), "{name}");
    }
}

#[test]
fn old_sparse_maps_that_do_not_describe_the_data_fail_the_document_only() {
    let dir = TempDir::new().expect("tempdir");
    let mut bad_number = old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 0, 5);
    bad_number[386..398].copy_from_slice(b"zzzzzzzzzzz\0");
    for (name, block) in [
        ("slot-not-a-number", fix_checksum(bad_number)),
        (
            "out-of-order",
            old_sparse_header(b"s.md", 3001, &[(1024, 2), (0, 2), (3000, 1)], 0, 5),
        ),
        (
            "too-little-data",
            old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 0, 4),
        ),
    ] {
        let bytes = tar_of(&[block, padded(b"abcde"), member(b"after.md", b'0', b"after")]);
        let read = read_tar_bytes(&dir, &bytes);
        assert_eq!(read.len(), 2, "{name}");
        assert_eq!(read[0].0, PathBuf::from("s.md"), "{name}");
        assert!(read[0].1.is_err(), "{name}: {read:?}");
        assert_eq!(read[1], ok("after.md", b"after"), "{name}");
    }
}

/// A PAX 1.0 sparse member: records, the stand-in header, then the map and
/// the stored data.
fn pax_1_0_member(name: &[u8], map: &str, stored: &[u8]) -> Vec<u8> {
    let records = pax_records(&[
        ("GNU.sparse.major", b"1"),
        ("GNU.sparse.minor", b"0"),
        ("GNU.sparse.name", name),
        ("GNU.sparse.realsize", b"3001"),
    ]);
    let data = [padded(map.as_bytes()), stored.to_vec()].concat();
    [
        member(b"PaxHeader", b'x', &records),
        member(b"GNUSparseFile.0/stand-in.md", b'0', &data),
    ]
    .concat()
}

#[test]
fn pax_sparse_files_are_rebuilt_under_their_real_name() {
    let dir = TempDir::new().expect("tempdir");
    let v00 = pax_records(&[
        ("GNU.sparse.size", b"3001"),
        ("GNU.sparse.numblocks", b"3"),
        ("GNU.sparse.offset", b"0"),
        ("GNU.sparse.numbytes", b"2"),
        ("GNU.sparse.offset", b"1024"),
        ("GNU.sparse.numbytes", b"2"),
        ("GNU.sparse.offset", b"3000"),
        ("GNU.sparse.numbytes", b"1"),
    ]);
    let v01 = pax_records(&[
        ("GNU.sparse.size", b"3001"),
        ("GNU.sparse.numblocks", b"3"),
        ("GNU.sparse.name", b"s.md"),
        ("GNU.sparse.map", b"0,2,1024,2,3000,1"),
    ]);
    for bytes in [
        tar_of(&[
            member(b"PaxHeader", b'x', &v00),
            member(b"s.md", b'0', b"abcde"),
            member(b"after.md", b'0', b"after"),
        ]),
        tar_of(&[
            member(b"PaxHeader", b'x', &v01),
            member(b"GNUSparseFile.0/stand-in.md", b'0', b"abcde"),
            member(b"after.md", b'0', b"after"),
        ]),
        tar_of(&[
            pax_1_0_member(b"s.md", "3\n0\n2\n1024\n2\n3000\n1\n", b"abcde"),
            member(b"after.md", b'0', b"after"),
        ]),
    ] {
        assert_eq!(
            read_tar_bytes(&dir, &bytes),
            [ok("s.md", &sparse_file()), ok("after.md", b"after")]
        );
    }
}

#[test]
fn pax_sparse_records_that_fit_no_form_fail_the_document_only() {
    let dir = TempDir::new().expect("tempdir");
    let mixed = pax_records(&[
        ("GNU.sparse.size", b"3001"),
        ("GNU.sparse.numblocks", b"3"),
        ("GNU.sparse.name", b"s.md"),
        ("GNU.sparse.map", b"0,2,1024,2,3000,1"),
        ("GNU.sparse.offset", b"0"),
        ("GNU.sparse.numbytes", b"2"),
    ]);
    let bytes = tar_of(&[
        member(b"PaxHeader", b'x', &mixed),
        member(b"GNUSparseFile.0/stand-in.md", b'0', b"abcde"),
        pax_1_0_member(b"t.md", "3\n0\n2\nx\n2\n3000\n1\n", b"abcde"),
        member(b"after.md", b'0', b"after"),
    ]);

    let read = read_tar_bytes(&dir, &bytes);

    let names: Vec<&Path> = read.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(
        names,
        [Path::new("s.md"), Path::new("t.md"), Path::new("after.md")]
    );
    assert!(read[0].1.is_err(), "{read:?}");
    assert!(read[1].1.is_err(), "{read:?}");
    assert_eq!(read[2], ok("after.md", b"after"));
}

#[test]
fn pax_sparse_files_without_their_real_name_make_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    for name in [None, Some(&b""[..])] {
        let mut records = vec![
            ("GNU.sparse.major", &b"1"[..]),
            ("GNU.sparse.minor", b"0"),
            ("GNU.sparse.realsize", b"3001"),
        ];
        if let Some(name) = name {
            records.push(("GNU.sparse.name", name));
        }
        let bytes = tar_of(&[
            member(b"PaxHeader", b'x', &pax_records(&records)),
            member(b"GNUSparseFile.0/s.md", b'0', &padded(b"0\n")),
        ]);
        let err = list_tar_bytes(&dir, &bytes).expect_err("must fail");
        assert!(err.reason.contains("real name"), "{name:?}: {err}");
    }
}

#[test]
fn an_empty_sparse_name_makes_the_archive_unreadable_even_beside_a_path() {
    let dir = TempDir::new().expect("tempdir");
    let v00: [(&str, &[u8]); 4] = [
        ("GNU.sparse.size", b"5"),
        ("GNU.sparse.numblocks", b"1"),
        ("GNU.sparse.offset", b"0"),
        ("GNU.sparse.numbytes", b"5"),
    ];
    // No 0.1 or 1.0 records, so the header's name is no stand-in; the empty
    // real name would still come first and leave the entry nameless.
    for others in [&[][..], &v00[..]] {
        let mut records: Vec<(&str, &[u8])> = vec![("GNU.sparse.name", b""), ("path", b"real.md")];
        records.extend_from_slice(others);
        let bytes = tar_of(&[
            member(b"PaxHeader", b'x', &pax_records(&records)),
            member(b"header.md", b'0', b"abcde"),
        ]);

        let err = list_tar_bytes(&dir, &bytes).expect_err("must fail");

        assert!(err.reason.contains("GNU.sparse.name"), "{others:?}: {err}");
    }
}

#[test]
fn a_sparse_file_larger_than_the_limit_fails() {
    let dir = TempDir::new().expect("tempdir");
    let archive = write_file(
        &dir,
        "x.tar",
        &tar_of(&[
            old_sparse_header(b"s.md", 3001, &SPARSE_SLOTS, 0, 5),
            padded(b"abcde"),
        ]),
    );

    let read = read_all(&archive, ArchiveKind::Tar, 3000);

    assert!(
        matches!(&read[0].1, Err(e) if e.contains("limit")),
        "{read:?}"
    );
}

/// `count` fragments of length 0 a block apart, which take no data.
fn empty_fragments(count: usize) -> impl Iterator<Item = (u64, u64)> {
    (0..count as u64).map(|i| (i * 512, 0))
}

#[test]
fn old_sparse_maps_past_the_fragment_limit_fail_and_the_walk_goes_on() {
    let dir = TempDir::new().expect("tempdir");
    let fragments: Vec<(u64, u64)> = empty_fragments(MAX_FRAGMENTS + 1).collect();
    let mut parts = vec![old_sparse_header(b"s.md", 1 << 30, &fragments[..4], 1, 0)];
    let blocks: Vec<&[(u64, u64)]> = fragments[4..].chunks(21).collect();
    for (i, slots) in blocks.iter().enumerate() {
        parts.push(sparse_extension(slots, u8::from(i + 1 < blocks.len())));
    }
    parts.push(member(b"after.md", b'0', b"after"));

    let read = read_tar_bytes(&dir, &tar_of(&parts));

    assert!(
        matches!(&read[0].1, Err(e) if e.contains("fragments")),
        "{:?}",
        read[0]
    );
    assert_eq!(read[1], ok("after.md", b"after"));
}

/// PAX 0.0 records for `count` fragments, written straight into one buffer.
fn pax_0_0_records(count: usize) -> Vec<u8> {
    let mut records = pax_records(&[
        ("GNU.sparse.size", b"1073741824"),
        ("GNU.sparse.numblocks", count.to_string().as_bytes()),
    ]);
    for (offset, length) in empty_fragments(count) {
        records.extend(pax_record(
            "GNU.sparse.offset",
            offset.to_string().as_bytes(),
        ));
        records.extend(pax_record(
            "GNU.sparse.numbytes",
            length.to_string().as_bytes(),
        ));
    }
    records
}

#[test]
fn pax_sparse_maps_past_the_fragment_limit_fail_but_later_records_still_count() {
    let dir = TempDir::new().expect("tempdir");
    let mut named = pax_0_0_records(MAX_FRAGMENTS + 1);
    named.extend(pax_record("path", b"named.md"));
    let mut v01 = pax_records(&[
        ("GNU.sparse.size", b"1073741824"),
        (
            "GNU.sparse.numblocks",
            (MAX_FRAGMENTS + 1).to_string().as_bytes(),
        ),
        ("GNU.sparse.name", b"map.md"),
    ]);
    let map: Vec<String> = empty_fragments(MAX_FRAGMENTS + 1)
        .flat_map(|(o, l)| [o.to_string(), l.to_string()])
        .collect();
    v01.extend(pax_record("GNU.sparse.map", map.join(",").as_bytes()));
    let bytes = tar_of(&[
        member(b"PaxHeader", b'x', &named),
        header(b"stand-in.md", b'0', 0),
        member(b"PaxHeader", b'x', &v01),
        header(b"GNUSparseFile.0/map.md", b'0', 0),
        pax_1_0_member(b"in-data.md", &format!("{}\n", MAX_FRAGMENTS + 1), b""),
        member(b"after.md", b'0', b"after"),
    ]);

    let read = read_tar_bytes(&dir, &bytes);

    let names: Vec<&Path> = read.iter().map(|(p, _)| p.as_path()).collect();
    assert_eq!(
        names,
        [
            Path::new("named.md"),
            Path::new("map.md"),
            Path::new("in-data.md"),
            Path::new("after.md")
        ]
    );
    for (path, result) in &read[..3] {
        assert!(
            matches!(result, Err(e) if e.contains("fragments")),
            "{path:?}: {result:?}"
        );
    }
    assert_eq!(read[3], ok("after.md", b"after"));
}

#[test]
fn a_malformed_record_after_a_map_past_the_limit_still_makes_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let mut records = pax_0_0_records(MAX_FRAGMENTS + 1);
    records.extend(b"9 x y z\n");
    let bytes = tar_of(&[
        member(b"PaxHeader", b'x', &records),
        header(b"s.md", b'0', 0),
    ]);

    let err = list_tar_bytes(&dir, &bytes).expect_err("must fail");

    assert!(err.reason.contains("malformed PAX record"), "{err}");
}

// ---- gzip ----

#[test]
fn a_tar_gz_made_of_several_gzip_members_is_read_through_all_of_them() {
    let dir = TempDir::new().expect("tempdir");
    let tar = tar_of(&[member(b"a.md", b'0', b"x"), member(b"b.md", b'0', b"y")]);
    // The first member ends right after `a.md`'s data block.
    let bytes = [gzip(&tar[..1024]), gzip(&tar[1024..])].concat();
    // The two blocks of the end-of-archive marker in two members.
    let marker_split = [gzip(&tar[..2560]), gzip(&tar[2560..])].concat();
    let padded_after = [gzip(&tar), vec![0; 4096]].concat();
    let garbage_after = [gzip(&tar), b"not gzip".to_vec()].concat();

    for (name, bytes) in [
        ("two-members", bytes),
        ("marker-split", marker_split),
        ("nul-padding-after", padded_after),
        ("garbage-after", garbage_after),
    ] {
        let archive = write_file(&dir, "x.tgz", &bytes);
        assert_eq!(
            read_all(&archive, ArchiveKind::TarGz, MAX_DOCUMENT_BYTES),
            [ok("a.md", b"x"), ok("b.md", b"y")],
            "{name}"
        );
    }
}

#[test]
fn a_wrong_crc_in_the_member_holding_the_end_marker_makes_the_archive_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    // GNU tar pads to 10 KiB, so the marker is well inside the member.
    let mut tar = tar_of(&[member(b"a.md", b'0', b"x")]);
    tar.resize(10240, 0);
    let mut bytes = gzip(&tar);
    let crc = bytes.len() - 8;
    bytes[crc] ^= 0xff;
    let archive = write_file(&dir, "x.tgz", &bytes);

    assert!(list_documents(&archive, ArchiveKind::TarGz).is_err());
}

// ---- zip ----

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

/// The local headers with their data, and the central directory, of a zip
/// of stored `entries`, local header positions counted from `base`.
fn zip_parts(entries: &[(&[u8], &[u8])], base: u64) -> (Vec<u8>, Vec<u8>) {
    let (mut locals, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = base + locals.len() as u64;
        let sizes = [crc32(data), data.len() as u32, data.len() as u32];
        locals.extend(LOCAL_HEADER);
        locals.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        sizes.iter().for_each(|v| locals.extend(v.to_le_bytes()));
        locals.extend((name.len() as u16).to_le_bytes());
        locals.extend(0u16.to_le_bytes());
        locals.extend(*name);
        locals.extend(*data);
        central.extend(CENTRAL_HEADER);
        central.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
        central.extend((name.len() as u16).to_le_bytes());
        // Extra and comment lengths, disk, internal and external attributes.
        central.extend([0u8; 12]);
        central.extend((offset as u32).to_le_bytes());
        central.extend(*name);
    }
    (locals, central)
}

/// An end-of-central-directory record.
fn end_record(entries: u16, size: u32, start: u32, comment: &[u8]) -> Vec<u8> {
    let mut record = b"PK\x05\x06".to_vec();
    record.extend([0u8; 4]);
    record.extend(entries.to_le_bytes());
    record.extend(entries.to_le_bytes());
    record.extend(size.to_le_bytes());
    record.extend(start.to_le_bytes());
    record.extend((comment.len() as u16).to_le_bytes());
    record.extend(comment);
    record
}

/// A zip64 end record at `at` and the locator pointing to it.
fn zip64_end(entries: u64, size: u64, start: u64, at: u64) -> Vec<u8> {
    let mut records = b"PK\x06\x06".to_vec();
    records.extend(44u64.to_le_bytes());
    records.extend([45, 0, 45, 0]);
    records.extend([0u8; 8]);
    records.extend(entries.to_le_bytes());
    records.extend(entries.to_le_bytes());
    records.extend(size.to_le_bytes());
    records.extend(start.to_le_bytes());
    records.extend(b"PK\x06\x07");
    records.extend(0u32.to_le_bytes());
    records.extend(at.to_le_bytes());
    records.extend(1u32.to_le_bytes());
    records
}

/// A zip of stored `entries` with `comment`, built by hand.
fn hand_zip(entries: &[(&[u8], &[u8])], comment: &[u8]) -> Vec<u8> {
    let (locals, central) = zip_parts(entries, 0);
    let end = end_record(
        entries.len() as u16,
        central.len() as u32,
        locals.len() as u32,
        comment,
    );
    [locals, central, end].concat()
}

const TWO_DOCS: [(&[u8], &[u8]); 2] = [(b"a.md", b"# A\n"), (b"b.md", b"# B\n")];

fn read_zip_bytes(dir: &TempDir, bytes: &[u8]) -> Result<Delivered, ArchiveError> {
    let archive = write_file(dir, "x.zip", bytes);
    let listing = list_documents(&archive, ArchiveKind::Zip)?;
    let mut read = Vec::new();
    read_documents(
        &archive,
        ArchiveKind::Zip,
        listing.stamp,
        &listing.documents,
        |doc, result| {
            read.push((doc.path.clone(), result.map_err(|e| e.to_string())));
            true
        },
    )?;
    Ok(read)
}

fn two_docs_read() -> Delivered {
    vec![ok("a.md", b"# A\n"), ok("b.md", b"# B\n")]
}

#[test]
fn a_hand_built_zip_is_read() {
    let dir = TempDir::new().expect("tempdir");

    let read = read_zip_bytes(&dir, &hand_zip(&TWO_DOCS, b"")).expect("read");

    assert_eq!(read, two_docs_read());
}

#[test]
fn a_zip_cut_short_is_not_read_as_the_zip_stored_inside_it() {
    let dir = TempDir::new().expect("tempdir");
    let inner = zip_bytes(
        &[("inner-1.md", b"inner one"), ("inner-2.md", b"inner two")],
        CompressionMethod::Stored,
    );
    let empty = zip_bytes(&[], CompressionMethod::Stored);
    for inner in [inner, empty] {
        let outer = zip_bytes(
            &[
                ("outer-a.md", b"outer a"),
                ("bundle.zip", &inner),
                ("outer-b.md", b"outer b"),
            ],
            CompressionMethod::Stored,
        );
        assert_eq!(read_zip_bytes(&dir, &outer).expect("whole").len(), 2);
        let cut = &outer[..outer.len() - 10];

        let err = read_zip_bytes(&dir, cut).expect_err("cut short");

        assert!(err.reason.contains("end-of-central-directory"), "{err}");
    }
}

#[test]
fn files_built_to_make_a_backward_search_slow_are_rejected() {
    let dir = TempDir::new().expect("tempdir");
    // End records claiming one entry at offset 0, with no directory.
    let lone = end_record(1, 46, 0, b"");
    let records = lone.repeat((1 << 20) / lone.len());
    // A directory whose last header is broken, claimed by many records.
    let entry: (&[u8], &[u8]) = (b"a.md", b"x");
    let (_, mut central) = zip_parts(&[entry; 1000], 0);
    let last = central.len() - (46 + 4);
    central[last] = b'X';
    let claim = end_record(1000, central.len() as u32, 0, b"");
    let broken = [central.clone(), claim.repeat(2000)].concat();

    for bytes in [records, broken] {
        let started = std::time::Instant::now();
        assert!(read_zip_bytes(&dir, &bytes).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}

#[test]
fn end_record_candidates_must_settle_on_exactly_one() {
    let dir = TempDir::new().expect("tempdir");
    let (locals, central) = zip_parts(&TWO_DOCS, 0);
    let cd_start = locals.len() as u32;
    let cd_size = central.len() as u32;
    let body = [locals, central].concat();
    let end_at = body.len() as u32;
    // A record shaped to pass the checks, hidden in the real one's
    // comment: no entries, an empty directory starting at itself.
    let fake = end_record(0, 0, end_at + 22, b"");
    let with_fake = [body.clone(), end_record(2, cd_size, cd_start, &fake)].concat();
    // A signature in the comment whose comment would run past the file.
    let mut stray = b"PK\x05\x06".to_vec();
    stray.extend([0u8; 16]);
    stray.extend(u16::MAX.to_le_bytes());
    let with_stray = [body.clone(), end_record(2, cd_size, cd_start, &stray)].concat();
    let trailing = [hand_zip(&TWO_DOCS, b""), vec![0; 100]].concat();
    let far_trailing = [hand_zip(&TWO_DOCS, b""), vec![0; 70_000]].concat();
    let small_zip = hand_zip(&[(b"c.md", b"# C\n")], b"");
    let concatenated = [hand_zip(&TWO_DOCS, b""), small_zip].concat();

    assert!(read_zip_bytes(&dir, &with_fake).is_err());
    assert_eq!(
        read_zip_bytes(&dir, &with_stray).expect("stray"),
        two_docs_read()
    );
    assert_eq!(
        read_zip_bytes(&dir, &trailing).expect("trailing"),
        two_docs_read()
    );
    assert!(read_zip_bytes(&dir, &far_trailing).is_err());
    assert_eq!(
        read_zip_bytes(&dir, &concatenated).expect("concatenated"),
        two_docs_read()
    );
    assert!(read_zip_bytes(&dir, b"not a zip at all").is_err());
}

#[test]
fn split_zips_are_rejected() {
    let dir = TempDir::new().expect("tempdir");
    let base = hand_zip(&TWO_DOCS, b"");
    let end = base.len() - 22;
    for (field, value) in [(4, 1u16), (6, 1), (8, 1)] {
        let mut bytes = base.clone();
        bytes[end + field..end + field + 2].copy_from_slice(&value.to_le_bytes());
        assert!(read_zip_bytes(&dir, &bytes).is_err(), "field {field}");
    }
    // An entry that starts on another disk.
    let mut bytes = base.clone();
    let central = bytes
        .windows(4)
        .position(|w| w == CENTRAL_HEADER)
        .expect("central header");
    bytes[central + 34..central + 36].copy_from_slice(&1u16.to_le_bytes());
    assert!(read_zip_bytes(&dir, &bytes).is_err());
}

#[test]
fn zip64_end_records_are_read_whether_or_not_the_end_record_points_to_them() {
    let dir = TempDir::new().expect("tempdir");
    let (locals, central) = zip_parts(&TWO_DOCS, 0);
    let (start, size) = (locals.len() as u64, central.len() as u64);
    let zip64_at = start + size;
    let records = zip64_end(2, size, start, zip64_at);
    let maxed = end_record(u16::MAX, u32::MAX, u32::MAX, b"");
    let plain = end_record(2, size as u32, start as u32, b"");
    for end in [maxed, plain] {
        let bytes = [locals.clone(), central.clone(), records.clone(), end].concat();
        assert_eq!(
            read_zip_bytes(&dir, &bytes).expect("zip64"),
            two_docs_read()
        );
    }
    // A locator that points somewhere without a zip64 end record.
    let mut bad = records.clone();
    bad[0] = b'X';
    let bytes = [
        locals,
        central,
        bad,
        end_record(2, size as u32, start as u32, b""),
    ]
    .concat();
    assert!(read_zip_bytes(&dir, &bytes).is_err());
}

fn deflated(data: &[u8]) -> Vec<u8> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("deflate");
    encoder.finish().expect("finish deflate")
}

/// A zip of `a.md`, then a deflated `b.md` whose central header holds the
/// fields marked in `maxed` (size, compressed size, local header position,
/// disk) at their maximum, with their values in a zip64 extra field.
fn zip_with_zip64_fields(maxed: [bool; 4], b_md: &[u8]) -> Vec<u8> {
    let (mut locals, mut central) = zip_parts(&[(b"a.md", b"# A\n")], 0);
    let name = b"b.md";
    let data = deflated(b_md);
    let (size, compressed, position) = (b_md.len() as u64, data.len() as u64, locals.len() as u64);
    locals.extend(LOCAL_HEADER);
    locals.extend([20, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
    locals.extend(crc32(b_md).to_le_bytes());
    locals.extend((compressed as u32).to_le_bytes());
    locals.extend((size as u32).to_le_bytes());
    locals.extend((name.len() as u16).to_le_bytes());
    locals.extend(0u16.to_le_bytes());
    locals.extend(name);
    locals.extend(&data);
    // APPNOTE 4.5.3: only the fields at their maximum, in this order.
    let mut zip64 = Vec::new();
    for (value, at_max) in [
        (size, maxed[0]),
        (compressed, maxed[1]),
        (position, maxed[2]),
    ] {
        if at_max {
            zip64.extend(value.to_le_bytes());
        }
    }
    if maxed[3] {
        zip64.extend(0u32.to_le_bytes());
    }
    let field = |value: u64, at_max: bool| if at_max { u32::MAX } else { value as u32 };
    central.extend(CENTRAL_HEADER);
    central.extend([45, 0, 45, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
    central.extend(crc32(b_md).to_le_bytes());
    central.extend(field(compressed, maxed[1]).to_le_bytes());
    central.extend(field(size, maxed[0]).to_le_bytes());
    central.extend((name.len() as u16).to_le_bytes());
    central.extend((zip64.len() as u16 + 4).to_le_bytes());
    central.extend(0u16.to_le_bytes());
    central.extend((if maxed[3] { u16::MAX } else { 0 }).to_le_bytes());
    central.extend([0u8; 6]);
    central.extend(field(position, maxed[2]).to_le_bytes());
    central.extend(name);
    central.extend(1u16.to_le_bytes());
    central.extend((zip64.len() as u16).to_le_bytes());
    central.extend(&zip64);
    let end = end_record(2, central.len() as u32, locals.len() as u32, b"");
    [locals, central, end].concat()
}

#[test]
fn central_header_fields_at_their_maximum_are_read_from_the_zip64_extra_field() {
    let dir = TempDir::new().expect("tempdir");
    // Deflated, so the size and the compressed size differ.
    let b_md = b"# B\n\n".repeat(100);
    for maxed in [
        [true, true, true, true],
        [true, false, false, false],
        [false, true, false, false],
        [false, false, true, false],
        [false, false, false, true],
        [true, false, true, false],
    ] {
        let read = read_zip_bytes(&dir, &zip_with_zip64_fields(maxed, &b_md)).expect("read");

        assert_eq!(
            read,
            vec![ok("a.md", b"# A\n"), ok("b.md", &b_md)],
            "{maxed:?}"
        );
    }
}

#[test]
fn the_directory_must_end_right_before_its_end_record() {
    let dir = TempDir::new().expect("tempdir");
    let (locals, central) = zip_parts(&TWO_DOCS, 0);
    let (start, size) = (locals.len() as u32, central.len() as u32);
    let body = [locals.clone(), central.clone()].concat();
    // Data in front, positions left as they were.
    let prefixed = [b"#!/bin/sh\nexit 0\n".to_vec(), hand_zip(&TWO_DOCS, b"")].concat();
    // Data in front, positions adjusted to count from the file's start.
    let prefix = b"#!/bin/sh\nexit 0\n".to_vec();
    let (adjusted_locals, adjusted_central) = zip_parts(&TWO_DOCS, prefix.len() as u64);
    let adjusted = [
        prefix.clone(),
        adjusted_locals.clone(),
        adjusted_central.clone(),
        end_record(
            2,
            adjusted_central.len() as u32,
            (prefix.len() + adjusted_locals.len()) as u32,
            b"",
        ),
    ]
    .concat();

    assert!(read_zip_bytes(&dir, &prefixed).is_err());
    assert_eq!(
        read_zip_bytes(&dir, &adjusted).expect("adjusted"),
        two_docs_read()
    );
    for (name, end) in [
        ("start-forged", end_record(2, size, start - 1, b"")),
        ("size-forged", end_record(2, size + 46, start, b"")),
        ("too-many-entries", end_record(3, size, start, b"")),
        ("too-few-entries", end_record(1, size, start, b"")),
    ] {
        let bytes = [body.clone(), end].concat();
        assert!(read_zip_bytes(&dir, &bytes).is_err(), "{name}");
    }
    // A digital signature record after the last header is let through.
    let mut signed = central.clone();
    signed.extend(b"PK\x05\x05");
    signed.extend(3u16.to_le_bytes());
    signed.extend(b"sig");
    let bytes = [
        locals,
        signed.clone(),
        end_record(2, signed.len() as u32, start, b""),
    ]
    .concat();
    assert_eq!(
        read_zip_bytes(&dir, &bytes).expect("signed"),
        two_docs_read()
    );
}

#[test]
fn damaged_central_headers_make_the_zip_unreadable() {
    let dir = TempDir::new().expect("tempdir");
    let base = hand_zip(&TWO_DOCS, b"");
    let central = base
        .windows(4)
        .position(|w| w == CENTRAL_HEADER)
        .expect("central header");
    let patch = |at: usize, bytes: &[u8]| {
        let mut zip = base.clone();
        zip[central + at..central + at + bytes.len()].copy_from_slice(bytes);
        zip
    };
    for (name, bytes) in [
        ("wrong-signature", patch(0, b"XK")),
        // A name running past the directory.
        ("name-too-long", patch(28, &500u16.to_le_bytes())),
        // Sizes that need zip64 information the header lacks.
        ("zip64-missing", patch(20, &u32::MAX.to_le_bytes())),
    ] {
        assert!(read_zip_bytes(&dir, &bytes).is_err(), "{name}");
    }
}

#[test]
fn local_headers_must_be_where_the_directory_says_and_data_must_end_before_it() {
    let dir = TempDir::new().expect("tempdir");
    let entries: [(&[u8], &[u8]); 2] = [(b"img.png", b"\x89PNG"), (b"a.md", b"# A\n")];
    let base = hand_zip(&entries, b"");
    // The first entry is not a document, so this is the whole archive's
    // problem, not one document's.
    let mut no_signature = base.clone();
    no_signature[0] = b'X';
    // The second entry's data claimed to run on into the directory.
    let mut into_directory = base.clone();
    let central = base
        .windows(4)
        .rposition(|w| w == CENTRAL_HEADER)
        .expect("second central header");
    into_directory[central + 20..central + 24].copy_from_slice(&400u32.to_le_bytes());

    assert!(read_zip_bytes(&dir, &no_signature).is_err());
    assert!(read_zip_bytes(&dir, &into_directory).is_err());
}

#[test]
fn entries_listed_out_of_file_order_are_read_from_their_own_data() {
    let dir = TempDir::new().expect("tempdir");
    let (locals, central) = zip_parts(&TWO_DOCS, 0);
    // Both headers are the same length: swap them.
    let half = central.len() / 2;
    let swapped = [&central[half..], &central[..half]].concat();
    let end = end_record(2, swapped.len() as u32, locals.len() as u32, b"");
    let bytes = [locals, swapped, end].concat();

    let read = read_zip_bytes(&dir, &bytes).expect("read");

    assert_eq!(read, vec![ok("b.md", b"# B\n"), ok("a.md", b"# A\n")]);
}

#[test]
fn documents_whose_size_or_crc_do_not_match_fail_individually() {
    let dir = TempDir::new().expect("tempdir");
    let base = hand_zip(&TWO_DOCS, b"");
    let central = base
        .windows(4)
        .position(|w| w == CENTRAL_HEADER)
        .expect("central header");
    let mut wrong_size = base.clone();
    wrong_size[central + 24..central + 28].copy_from_slice(&5u32.to_le_bytes());
    let mut wrong_crc = base.clone();
    wrong_crc[central + 16] ^= 0xff;

    for (name, bytes) in [("size", wrong_size), ("crc", wrong_crc)] {
        let read = read_zip_bytes(&dir, &bytes).expect(name);
        assert!(read[0].1.is_err(), "{name}: {read:?}");
        assert_eq!(read[1], ok("b.md", b"# B\n"), "{name}");
    }
}

#[test]
fn a_zip_replaced_between_passes_is_checked_again() {
    let dir = TempDir::new().expect("tempdir");
    let first = hand_zip(&TWO_DOCS, b"");
    // Both entries pointed at the first one's data.
    let mut shared = first.clone();
    let second = shared
        .windows(4)
        .rposition(|w| w == CENTRAL_HEADER)
        .expect("second central header");
    shared[second + 42..second + 46].copy_from_slice(&0u32.to_le_bytes());
    // No end record left to find.
    let mut no_end = first.clone();
    let end = no_end
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .expect("end record");
    no_end[end..end + 4].fill(0);
    for (replacement, reason) in [
        (shared, "share compressed data"),
        (no_end, "no usable end-of-central-directory record"),
    ] {
        let archive = write_file(&dir, "x.zip", &first);
        let listing = list_documents(&archive, ArchiveKind::Zip).expect("list");
        replace_keeping_stamp(&archive, &replacement);

        let (delivered, result) = read_listed(&archive, ArchiveKind::Zip, &listing, || {});

        let err = result.expect_err(reason);
        assert!(err.reason.contains(reason), "{err}");
        assert!(delivered.is_empty(), "{delivered:?}");
    }
}

#[test]
fn zips_whose_entries_share_compressed_data_cannot_be_listed() {
    let dir = TempDir::new().expect("tempdir");
    let mut bytes = zip_bytes(
        &[("a.md", b"text"), ("b.md", b"text"), ("empty.md", b"")],
        CompressionMethod::Stored,
    );
    let unshared = write_file(&dir, "unshared.zip", &bytes);
    // Point the second entry's central directory record at the first
    // entry's local header, at offset 0, so both read the same data.
    let second_record = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, window)| *window == CENTRAL_HEADER)
        .nth(1)
        .map(|(start, _)| start)
        .expect("second central directory record");
    bytes[second_record + 42..second_record + 46].copy_from_slice(&0u32.to_le_bytes());
    let shared = write_file(&dir, "shared.zip", &bytes);

    assert_eq!(
        list_documents(&unshared, ArchiveKind::Zip)
            .expect("list")
            .documents
            .len(),
        3
    );
    let err = list_documents(&shared, ArchiveKind::Zip).expect_err("must fail");
    assert!(err.reason.contains("share compressed data"), "{err}");
}

// ---- archives written by real tools (tests/fixtures/archives) ----

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archives")
        .join(name)
}

fn read_fixture(name: &str) -> Delivered {
    let path = fixture(name);
    let kind = ArchiveKind::from_path(&path).expect("archive kind");
    read_all(&path, kind, MAX_DOCUMENT_BYTES)
}

const SHORT: &[u8] = b"# Short\n\nA plain document.\n";
const LONG: &[u8] = b"# Long\n\nA document with a path over 100 bytes.\n";
const JAPANESE: &str = "# 日本語\n\n名前が UTF-8 の文書です。\n";
const TAGGED: &[u8] = b"# Tagged\n\nCarries an extended attribute.\n";
const LONG_PATH: &str = "deep/segment-with-a-long-name-0001/segment-with-a-long-name-0002/\
                         segment-with-a-long-name-0003/segment-with-a-long-name-0004/\
                         segment-with-a-long-name-0005/long.md";

fn repeated() -> Vec<u8> {
    let mut text = b"# Repeated\n\n".to_vec();
    text.extend(b"A line that deflates well.\n".repeat(200));
    text
}

#[test]
fn tars_written_by_gnu_tar_bsdtar_and_git_are_read() {
    // The tree: a long path, `hard.md` (stored as the file, with
    // `notes/short.md` a hard link to it, as `--sort=name` puts it
    // first), symbolic links, a FIFO named `pipe.md` and a UTF-8 name.
    let tree = vec![
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
        assert_eq!(read_fixture(name), tree, "{name}");
    }
    assert_eq!(read_fixture("gtar-v7.tar.gz"), [ok("hard.md", SHORT)]);
    let git = vec![
        ok(LONG_PATH, LONG),
        ok("notes/repeated.md", &repeated()),
        ok("notes/short.md", SHORT),
        ok("日本語.md", JAPANESE.as_bytes()),
    ];
    assert_eq!(read_fixture("git-archive.tar.gz"), git);
    assert_eq!(read_fixture("git-archive-gzip.tar.gz"), git);
    for name in [
        "gtar-posix-global.tar.gz",
        "gtar-gzip.tar.gz",
        "bsdtar-gzip.tar.gz",
        "python-gzip.tar.gz",
    ] {
        assert_eq!(read_fixture(name), [ok("notes/short.md", SHORT)], "{name}");
    }
    assert_eq!(
        read_fixture("bsdtar-mac-metadata.tar.gz"),
        [ok("tagged.md", TAGGED)]
    );
    assert_eq!(
        read_fixture("bsdtar-large-xattr.tar.gz"),
        [
            ok("big-xattr.md", b"# Big extended attribute\n"),
            ok("after.md", b"# After\n\nComes after the big pax header.\n"),
        ]
    );
}

#[test]
fn tars_written_by_python_are_read() {
    assert_eq!(
        read_fixture("python-gnu.tar.gz"),
        [ok("notes/short.md", SHORT), ok(LONG_PATH, LONG)]
    );
    assert_eq!(
        read_fixture("python-ustar.tar.gz"),
        [ok("notes/short.md", SHORT), ok(LONG_PATH, LONG)]
    );
    assert_eq!(
        read_fixture("python-pax.tar.gz"),
        [
            ok("notes/short.md", SHORT),
            ok(LONG_PATH, LONG),
            ok("日本語.md", JAPANESE.as_bytes()),
            ok("sized.md", b"# Sized\n\nCarries a pax size record.\n"),
        ]
    );
    // `hdrcharset=BINARY` with a Latin-1 name: not UTF-8, so failed.
    let listing = list_documents(&fixture("python-pax.tar.gz"), ArchiveKind::TarGz).expect("list");
    assert_eq!(listing.rejected.len(), 1);
    assert_eq!(listing.rejected[0].problem, NameProblem::NotUtf8);
}

#[test]
fn a_python_tar_with_a_global_path_is_unreadable() {
    let err = list_documents(
        &fixture("python-pax-global-path.tar.gz"),
        ArchiveKind::TarGz,
    )
    .expect_err("must fail");

    assert!(err.reason.contains("global"), "{err}");
}

/// `0` zero bytes, handed out from one shared block so that stepping over
/// gigabytes of them copies nothing.
struct Zeros(u64);

static ZERO_BLOCK: [u8; 1 << 16] = [0; 1 << 16];

impl io::Read for Zeros {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = buf.len().min(self.fill_buf()?.len());
        buf[..read].fill(0);
        self.consume(read);
        Ok(read)
    }
}

impl io::BufRead for Zeros {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let len = usize::try_from(self.0).map_or(ZERO_BLOCK.len(), |n| n.min(ZERO_BLOCK.len()));
        Ok(&ZERO_BLOCK[..len])
    }

    fn consume(&mut self, amount: usize) {
        self.0 -= amount as u64;
    }
}

#[test]
fn an_8_gib_member_is_stepped_over_by_its_pax_or_base256_size() {
    let tail = std::fs::read(fixture("python-8gib.tail")).expect("tail");
    for head in ["python-8gib-pax.head", "python-8gib-gnu.head"] {
        let head = std::fs::read(fixture(head)).expect("head");
        let stream = || head.as_slice().chain(Zeros(8 << 30)).chain(tail.as_slice());
        let mut listing = PartialListing::new(MAX_LISTED_NAME_BYTES);
        tar::list_tar(stream(), &mut listing).expect("list");
        let listing = listing.finish(NO_FILE);
        let mut read = Vec::new();
        tar::read_tar(
            stream(),
            &listing.documents,
            MAX_DOCUMENT_BYTES,
            |doc, result| {
                read.push((doc.path.clone(), result.map_err(|e| e.to_string())));
                true
            },
        )
        .expect("read");

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
fn sparse_files_written_by_gnu_tar_and_bsdtar_are_rebuilt() {
    let expected = vec![
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
        let read = read_fixture(name);
        let summary: Vec<(PathBuf, Option<usize>)> = read
            .iter()
            .map(|(p, r)| (p.clone(), r.as_ref().ok().map(Vec::len)))
            .collect();
        assert!(read == expected, "{name}: {summary:?}");
    }
}

#[test]
fn zips_written_by_ditto_zip_git_and_python_are_read() {
    let japanese = JAPANESE.as_bytes();
    assert_eq!(
        read_fixture("ditto.zip"),
        [
            ok("zipsrc/tagged.md", TAGGED),
            ok("zipsrc/notes/repeated.md", &repeated()),
            ok("zipsrc/notes/short.md", SHORT),
            ok("zipsrc/日本語.md", japanese),
        ]
    );
    assert_eq!(
        read_fixture("zip-command.zip"),
        [
            ok("notes/repeated.md", &repeated()),
            ok("notes/short.md", SHORT),
            ok("日本語.md", japanese),
            ok("tagged.md", TAGGED),
        ]
    );
    // The symbolic link `link-to-short.md` is not a document.
    assert_eq!(
        read_fixture("git-archive.zip"),
        [
            ok(LONG_PATH, LONG),
            ok("notes/repeated.md", &repeated()),
            ok("notes/short.md", SHORT),
            ok("日本語.md", japanese),
        ]
    );
    // `a.md`, `./a.md`, `a.md`: the last one wins.
    assert_eq!(
        read_fixture("python-duplicate-names.zip"),
        [ok("a.md", b"# Third\n")]
    );
    // zip64 end records next to an end record with plain values; the entry
    // read from standard input is named `-`, not a document.
    assert_eq!(
        read_fixture("zip-command-stdin.zip"),
        [ok("tagged.md", TAGGED)]
    );
    // Each file's central header holds 0xFFFFFFFF for its size and the size
    // itself in its zip64 extra field.
    assert_eq!(
        read_fixture("zip-command-zip64.zip"),
        [
            ok("notes/repeated.md", &repeated()),
            ok("notes/short.md", SHORT),
        ]
    );
    // The stored inner zip's end record is near the end, but only the
    // outer one passes the checks; the inner zip is not a document.
    assert_eq!(
        read_fixture("zip-command-nested.zip"),
        [ok("notes/short.md", SHORT)]
    );
}

#[test]
fn a_zip_written_by_zip_with_a_stored_zip_inside_is_unreadable_once_cut_short() {
    let dir = TempDir::new().expect("tempdir");
    let bytes = std::fs::read(fixture("zip-command-nested.zip")).expect("fixture");

    let err = read_zip_bytes(&dir, &bytes[..bytes.len() - 10]).expect_err("cut short");

    assert!(err.reason.contains("end-of-central-directory"), "{err}");
}
