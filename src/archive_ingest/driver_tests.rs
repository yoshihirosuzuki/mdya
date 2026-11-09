//! Unit tests of the driver, with test doubles for the zip and tar readers,
//! `format`, and `index`.

use std::fs::{self, File, FileTimes};
use std::future::Future;
use std::io::{ErrorKind, Read};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use futures::FutureExt;
use serde_json::json;
use tempfile::TempDir;
use tracing::Level;

use super::kind::ArchiveKind;
use super::test_readers::{TestReaders, document, not_regular, raw_document};
use super::transfer::{Position, ReadResult};
use super::{ArchiveContents, ArchiveIngest, Limits};
use crate::format::test_double::{TestFormat, whole_text};
use crate::index::test_double::{IndexCall, TestIndex};
use crate::ingest::{
    Ingest, IngestError, IngestItems, ItemContents, PutInstruction, UpdateInstruction,
    UpdateReport, UpdateStopped,
};
use crate::path::MakePathAbsoluteError;
use crate::test_support::log_receiver::capture_async;
use crate::types::test_double::{TestProgressSink, test_dir_lookup};
use crate::types::{BoxError, CollectionName, DirLookup, Key, Progress, TypeAttributes};

const NO_LIMIT: Limits = Limits {
    list: u64::MAX,
    content: u64::MAX,
};
const TIMEOUT: Duration = Duration::from_secs(30);

/// A file standing for the archive, in a directory that lasts as long as it.
fn archive_named(name: &str) -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(name);
    fs::write(&path, b"the archive").unwrap();
    (directory, path)
}

fn archive() -> (TempDir, PathBuf) {
    archive_named("docs.zip")
}

fn attributes_of(location: &str) -> TypeAttributes {
    TypeAttributes(json!({ "path": location }).as_object().unwrap().clone())
}

fn attributes(path: &Path) -> TypeAttributes {
    attributes_of(path.to_str().unwrap())
}

fn instruction_for(collection: &str, path: &Path) -> UpdateInstruction {
    UpdateInstruction {
        collection: CollectionName(collection.to_owned()),
        attributes: attributes(path),
        overwrite: false,
        parallelism: 1,
    }
}

fn instruction(path: &Path) -> UpdateInstruction {
    instruction_for("notes", path)
}

fn format() -> Arc<TestFormat> {
    Arc::new(TestFormat::new().with_extension("md", "markdown"))
}

struct Fixture {
    ingest: ArchiveIngest,
    readers: Arc<TestReaders>,
    index: Arc<TestIndex>,
}

fn fixture_with(readers: TestReaders, index: TestIndex, limits: Limits) -> Fixture {
    let readers = Arc::new(readers);
    let index = Arc::new(index);
    Fixture {
        ingest: ArchiveIngest::new(format(), index.clone(), readers.clone(), limits),
        readers,
        index,
    }
}

fn fixture(readers: TestReaders) -> Fixture {
    fixture_with(readers, TestIndex::new(), NO_LIMIT)
}

impl Fixture {
    async fn update(&self, path: &Path) -> Result<UpdateReport, UpdateStopped> {
        self.ingest
            .update(instruction(path), &TestProgressSink::new())
            .await
    }

    async fn prepared(&self, path: &Path) -> (IngestItems, ArchiveContents) {
        self.ingest.prepare(&instruction(path)).await.unwrap()
    }

    fn registered_keys(&self) -> Vec<String> {
        self.registered_keys_of("notes")
    }

    fn registered_keys_of(&self, collection: &str) -> Vec<String> {
        self.index
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                IndexCall::Register(document) if document.collection.0 == collection => {
                    Some(document.key.0)
                }
                _ => None,
            })
            .collect()
    }
}

fn keys_of(items: &IngestItems) -> Vec<&str> {
    items.items.iter().map(|item| item.key.0.as_str()).collect()
}

#[track_caller]
fn assert_source_unreadable(result: Result<UpdateReport, UpdateStopped>) {
    let stopped = result.unwrap_err();
    assert!(matches!(
        stopped.error,
        IngestError::SourceUnreadable { .. }
    ));
    assert_eq!(stopped.counts, UpdateReport::default());
}

/// Changes the file's length, keeping its modification time.
fn change_length(path: &Path) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_len(1).unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
}

/// Changes the file's modification time, keeping its length.
fn change_modified(path: &Path) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(99)))
        .unwrap();
}

async fn all_contents(contents: &mut ArchiveContents) -> Vec<Result<Vec<u8>, BoxError>> {
    let mut all = Vec::new();
    while let Some(content) = contents.next().await {
        all.push(content);
    }
    all
}

// What the component says it can do (the functions without a source).

#[test]
fn it_can_be_updated_but_does_not_take_document_content() {
    let fixture = fixture(TestReaders::new(Vec::new()));

    assert!(fixture.ingest.supports_update());
    assert!(!fixture.ingest.supports_put());
}

#[test]
fn create_makes_a_component_that_can_be_updated() {
    let ingest = super::create(format(), Arc::new(TestIndex::new()));

    assert!(ingest.supports_update());
}

#[tokio::test]
async fn taking_document_content_is_refused() {
    let fixture = fixture(TestReaders::new(Vec::new()));
    let instruction = PutInstruction {
        collection: CollectionName("notes".to_owned()),
        attributes: TypeAttributes::default(),
        key: Key("a.md".to_owned()),
        media_type: "text/markdown".to_owned(),
        content: b"# A".to_vec(),
        overwrite: false,
    };

    let result = fixture.ingest.put(instruction).await;

    assert!(matches!(result, Err(IngestError::PutNotSupported)));
    assert!(fixture.index.calls().is_empty());
}

// Checking the type-specific attributes.

fn no_directories() -> DirLookup {
    test_dir_lookup(Some("/home/user"), Ok("/work"))
}

fn checker() -> Fixture {
    fixture(TestReaders::new(Vec::new()))
}

async fn check(location: &str) -> Result<TypeAttributes, IngestError> {
    checker()
        .ingest
        .check_attributes(attributes_of(location), &no_directories())
        .await
}

#[tokio::test]
async fn the_attribute_names_are_checked_before_the_location() {
    let missing = "/no/such/file.zip";
    let extra = json!({ "path": missing, "recursive": true });
    let attributes = TypeAttributes(extra.as_object().unwrap().clone());

    let result = checker()
        .ingest
        .check_attributes(attributes, &no_directories())
        .await;

    assert!(matches!(result, Err(IngestError::AttributesMissingOrExtra)));
}

#[tokio::test]
async fn a_missing_location_is_not_found_with_its_cause() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("gone.zip");

    let result = check(missing.to_str().unwrap()).await;

    let Err(IngestError::LocationNotFound { source }) = result else {
        panic!("{result:?}");
    };
    assert!(source.downcast_ref::<std::io::Error>().is_some());
}

#[tokio::test]
async fn a_directory_does_not_fit_the_collection_type() {
    let directory = tempfile::tempdir().unwrap();
    let named_like_an_archive = directory.path().join("dir.zip");
    fs::create_dir(&named_like_an_archive).unwrap();

    let result = check(named_like_an_archive.to_str().unwrap()).await;

    assert!(matches!(result, Err(IngestError::LocationTypeMismatch)));
}

#[tokio::test]
async fn a_file_without_an_archive_extension_does_not_fit() {
    for name in ["a.gz", "a.zip.txt", "a"] {
        let (_directory, path) = archive_named(name);

        let result = check(path.to_str().unwrap()).await;

        assert!(
            matches!(result, Err(IngestError::LocationTypeMismatch)),
            "{name}"
        );
    }
}

#[tokio::test]
async fn every_archive_extension_is_accepted_in_any_case() {
    for name in [
        "a.zip", "a.tar", "a.tar.gz", "a.tgz", "A.ZIP", "a.TAR.GZ", "a.Tgz",
    ] {
        let (_directory, path) = archive_named(name);

        assert!(check(path.to_str().unwrap()).await.is_ok(), "{name}");
    }
}

#[cfg(unix)]
mod links {
    use std::os::unix::fs::symlink;

    use super::*;

    #[tokio::test]
    async fn a_link_to_an_archive_is_accepted() {
        let (directory, target) = archive();
        let link = directory.path().join("link.zip");
        symlink(&target, &link).unwrap();

        assert!(check(link.to_str().unwrap()).await.is_ok());
    }

    #[tokio::test]
    async fn a_link_to_a_directory_does_not_fit() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("link.zip");
        symlink(directory.path(), &link).unwrap();

        let result = check(link.to_str().unwrap()).await;

        assert!(matches!(result, Err(IngestError::LocationTypeMismatch)));
    }

    #[tokio::test]
    async fn a_link_to_nothing_is_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("link.zip");
        symlink(directory.path().join("gone.zip"), &link).unwrap();

        let result = check(link.to_str().unwrap()).await;

        assert!(matches!(result, Err(IngestError::LocationNotFound { .. })));
    }

    #[tokio::test]
    async fn a_link_is_judged_by_its_own_name() {
        let (directory, target) = archive_named("b.zip");
        let link = directory.path().join("a.txt");
        symlink(&target, &link).unwrap();

        let result = check(link.to_str().unwrap()).await;

        assert!(matches!(result, Err(IngestError::LocationTypeMismatch)));
    }
}

#[tokio::test]
async fn a_relative_location_comes_back_absolute() {
    let (directory, _path) = archive();
    let working_dir = directory.path().to_str().unwrap();
    let dirs = test_dir_lookup(Some("/home/user"), Ok(working_dir));

    let checked = checker()
        .ingest
        .check_attributes(attributes_of("docs.zip"), &dirs)
        .await
        .unwrap();

    assert_eq!(checked, attributes(&directory.path().join("docs.zip")));
}

#[cfg(unix)]
#[tokio::test]
async fn a_working_directory_that_is_not_utf8_is_not_found() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dirs = DirLookup {
        home_dir: Arc::new(|| None),
        current_dir: Arc::new(|| Ok(PathBuf::from(OsStr::from_bytes(b"/\xFF")))),
    };

    let result = checker()
        .ingest
        .check_attributes(attributes_of("docs.zip"), &dirs)
        .await;

    let Err(IngestError::LocationNotFound { source }) = result else {
        panic!("{result:?}");
    };
    assert!(source.downcast_ref::<super::LocationNotUtf8>().is_some());
}

#[tokio::test]
async fn an_unreadable_working_directory_is_not_found_with_the_path_error_as_its_cause() {
    let dirs = test_dir_lookup(Some("/home/user"), Err(ErrorKind::PermissionDenied));

    let result = checker()
        .ingest
        .check_attributes(attributes_of("docs.zip"), &dirs)
        .await;

    let Err(IngestError::LocationNotFound { source }) = result else {
        panic!("{result:?}");
    };
    assert!(matches!(
        source.downcast_ref::<MakePathAbsoluteError>(),
        Some(MakePathAbsoluteError::WorkingDirUnreadable(_))
    ));
}

// What becomes a document.

#[tokio::test]
async fn what_is_not_a_document_is_left_out_without_a_warning() {
    let (_directory, path) = archive();
    let items = vec![
        not_regular("link.md"),
        document("image.png"),
        raw_document(b"\xFF.png"),
        raw_document(b"image.\xFF"),
        document("no-extension"),
        document("dir.md/no-extension"),
        document("__MACOSX/a.md"),
        document("./__MACOSX/a.md"),
        document(".//__MACOSX/a.md"),
        document("__MACOSX\\a.md"),
        raw_document(b"__MACOSX/\xFF.md"),
        document("dir/._a.md"),
        document("dir\\._a.md"),
        raw_document(b"._\xFF.md"),
    ];
    let fixture = fixture(TestReaders::new(items));

    let (report, logs) = capture_async(fixture.update(&path)).await;

    assert_eq!(report.unwrap(), UpdateReport::default());
    assert!(fixture.registered_keys().is_empty());
    assert!(logs.is_empty());
}

#[tokio::test]
async fn a_name_that_is_not_utf8_with_a_document_extension_is_failed_with_a_warning() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![raw_document(b"\x95\\.md")]));

    let (report, logs) = capture_async(fixture.update(&path)).await;

    assert_eq!(report.unwrap().failed, 1);
    assert_eq!(logs.len(), 1);
    assert!(
        logs[0]
            .fields
            .contains(&("name".to_owned(), "\u{FFFD}\\.md".to_owned()))
    );
}

#[tokio::test]
async fn in_a_name_that_is_not_utf8_a_backslash_does_not_separate() {
    let (_directory, path) = archive();
    let hidden_looking = raw_document(b"\x95\\._x.md");
    let fixture = fixture(TestReaders::new(vec![hidden_looking]));

    let report = fixture.update(&path).await.unwrap();

    assert_eq!(report.failed, 1);
}

#[tokio::test]
async fn a_key_uses_slashes_and_drops_empty_and_dot_segments() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![
        document("docs\\a.md"),
        document("./docs//b.md"),
        document("../c.md"),
    ]));

    let (items, _) = fixture.prepared(&path).await;

    assert_eq!(keys_of(&items), ["docs/a.md", "docs/b.md", "../c.md"]);
}

#[tokio::test]
async fn of_entries_with_the_same_key_only_the_last_is_registered() {
    let (_directory, path) = archive();
    let readers = TestReaders::new(vec![document("a.md"), document("./a.md")]);
    let fixture = fixture(readers);

    let (report, logs) = capture_async(fixture.update(&path)).await;

    assert_eq!(report.unwrap().added, 1);
    assert!(logs.is_empty());
    assert_eq!(fixture.readers.positions_read(), [Position(1)]);
}

#[tokio::test]
async fn items_are_handed_over_in_position_order_whatever_the_names() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![
        document("z.md"),
        document("a.md"),
        document("m.md"),
    ]));

    let report = fixture.update(&path).await.unwrap();

    assert_eq!(report.added, 3);
    assert_eq!(
        fixture.readers.positions_read(),
        [Position(0), Position(1), Position(2)]
    );
}

#[tokio::test]
async fn the_time_is_the_archives_and_the_same_for_every_document() {
    let (_directory, path) = archive();
    let modified = UNIX_EPOCH + Duration::from_secs(1_000_000);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let fixture = fixture(TestReaders::new(vec![document("a.md"), document("b.md")]));

    let (items, _) = fixture.prepared(&path).await;

    assert!(items.items.iter().all(|item| item.time == modified));
}

#[tokio::test]
async fn documents_outside_the_list_are_deleted_and_unreadable_ones_kept() {
    let (_directory, path) = archive();
    let readers =
        TestReaders::new(vec![document("a.md"), document("b.md")]).on_read(|position, _| {
            (position == Position(0)).then(|| ReadResult::Entry {
                name: b"a.md".to_vec(),
                regular: true,
                content: Err("cannot be read".into()),
            })
        });
    let fixture = fixture(readers);

    let report = fixture.update(&path).await.unwrap();

    assert_eq!(report.failed, 1);
    let keep = fixture
        .index
        .calls()
        .into_iter()
        .find_map(|call| match call {
            IndexCall::DeleteOthers {
                keep,
                unreadable_dirs,
                ..
            } => Some((keep, unreadable_dirs)),
            _ => None,
        });
    let (keep, unreadable_dirs) = keep.unwrap();
    assert!(keep.contains(&Key("a.md".to_owned())));
    assert!(keep.contains(&Key("b.md".to_owned())));
    assert!(unreadable_dirs.is_empty());
}

#[tokio::test]
async fn progress_counts_each_document_including_those_without_a_key() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![
        document("a.md"),
        document("./a.md"),
        raw_document(b"\xFF.md"),
        document("image.png"),
    ]));
    let progress = TestProgressSink::new();

    fixture
        .ingest
        .update(instruction(&path), &progress)
        .await
        .unwrap();

    assert_eq!(
        progress.reports().last(),
        Some(&Progress {
            done: 2,
            total: Some(2)
        })
    );
}

// The size of the list.

fn with_list_limit(items: Vec<super::test_readers::TestItem>, list: u64) -> Fixture {
    fixture_with(
        TestReaders::new(items),
        TestIndex::new(),
        Limits {
            list,
            content: u64::MAX,
        },
    )
}

#[tokio::test]
async fn each_document_counts_its_name_and_128_bytes() {
    let (_directory, path) = archive();
    let items = || vec![document("a.md"), document("bb.md")];

    assert!(with_list_limit(items(), 265).update(&path).await.is_ok());
    assert_source_unreadable(with_list_limit(items(), 264).update(&path).await);
}

#[test]
fn both_limits_in_use_are_256_mib() {
    assert_eq!(super::LIMITS.list, 268_435_456);
    assert_eq!(super::LIMITS.content, 268_435_456);
}

#[tokio::test]
async fn the_limit_on_content_is_handed_to_the_second_pass() {
    let (_directory, path) = archive();
    let limits = Limits {
        list: u64::MAX,
        content: 1234,
    };
    let fixture = fixture_with(
        TestReaders::new(vec![document("a.md")]),
        TestIndex::new(),
        limits,
    );

    fixture.update(&path).await.unwrap();

    assert_eq!(fixture.readers.max_contents(), [1234]);
}

#[tokio::test]
async fn a_document_without_a_key_counts_like_any_other() {
    let (_directory, path) = archive();
    let items = || vec![raw_document(b"\xFF.md")];

    assert!(with_list_limit(items(), 132).update(&path).await.is_ok());
    assert_source_unreadable(with_list_limit(items(), 131).update(&path).await);
}

#[tokio::test]
async fn entries_that_are_not_documents_do_not_count() {
    let (_directory, path) = archive();
    let items = vec![document("image.png"), document("a.md")];

    assert!(with_list_limit(items, 132).update(&path).await.is_ok());
}

#[tokio::test]
async fn entries_with_the_same_key_count_one_by_one() {
    let (_directory, path) = archive();
    let items = || vec![document("a.md"), document("./a.md")];

    assert!(with_list_limit(items(), 266).update(&path).await.is_ok());
    assert_source_unreadable(with_list_limit(items(), 265).update(&path).await);
}

// That the content belongs to the entry.

#[tokio::test]
async fn a_change_while_the_list_is_read_makes_the_source_unreadable() {
    let changes: [fn(&Path); 2] = [change_length, change_modified];
    for change in changes {
        let (_directory, path) = archive();
        let changed = path.clone();
        let fixture =
            fixture(TestReaders::new(vec![document("a.md")]).on_list_end(move || change(&changed)));

        assert_source_unreadable(fixture.update(&path).await);
        assert!(fixture.index.calls().is_empty());
    }
}

#[tokio::test]
async fn a_change_before_the_second_pass_fails_every_document() {
    let changes: [fn(&Path); 2] = [change_length, change_modified];
    for change in changes {
        let (_directory, path) = archive();
        let fixture = fixture(TestReaders::new(vec![document("a.md"), document("b.md")]));
        let (_, mut contents) = fixture.prepared(&path).await;

        change(&path);

        let all = all_contents(&mut contents).await;
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(Result::is_err));
    }
}

#[tokio::test]
async fn a_change_after_the_first_document_fails_the_rest() {
    let changes: [fn(&Path); 2] = [change_length, change_modified];
    for change in changes {
        let (_directory, path) = archive();
        let fixture = fixture(TestReaders::new(vec![
            document("a.md"),
            document("b.md"),
            document("c.md"),
        ]));
        let (_, mut contents) = fixture.prepared(&path).await;

        assert!(contents.next().await.unwrap().is_ok());
        change(&path);

        let rest = all_contents(&mut contents).await;
        assert_eq!(rest.len(), 2);
        assert!(rest.iter().all(Result::is_err));
    }
}

#[tokio::test]
async fn an_archive_put_in_its_place_does_not_change_what_is_read() {
    let (directory, path) = archive();
    let readers = TestReaders::new(vec![document("a.md")]).on_read(|_, file| {
        let mut content = Vec::new();
        file.read_to_end(&mut content).unwrap();
        Some(ReadResult::Entry {
            name: b"a.md".to_vec(),
            regular: true,
            content: Ok(content),
        })
    });
    let fixture = fixture(readers);
    let (_, mut contents) = fixture.prepared(&path).await;

    let replacement = directory.path().join("new.zip");
    fs::write(&replacement, b"a different archive").unwrap();
    fs::rename(&replacement, &path).unwrap();

    assert_eq!(contents.next().await.unwrap().unwrap(), b"the archive");
}

#[tokio::test]
async fn an_entry_with_another_name_or_kind_fails_only_that_document() {
    let replies = [(b"other.md".to_vec(), true), (b"a.md".to_vec(), false)];
    for (name, regular) in replies {
        let (_directory, path) = archive();
        let readers = TestReaders::new(vec![document("a.md"), document("b.md")]).on_read(
            move |position, _| {
                (position == Position(0)).then(|| ReadResult::Entry {
                    name: name.clone(),
                    regular,
                    content: Ok(b"content".to_vec()),
                })
            },
        );
        let fixture = fixture(readers);
        let (_, mut contents) = fixture.prepared(&path).await;

        assert!(contents.next().await.unwrap().is_err());
        assert_eq!(contents.next().await.unwrap().unwrap(), b"b.md");
    }
}

#[tokio::test]
async fn a_document_that_cannot_be_read_fails_only_that_document() {
    let (_directory, path) = archive();
    let readers =
        TestReaders::new(vec![document("a.md"), document("b.md")]).on_read(|position, _| {
            (position == Position(0)).then(|| ReadResult::Entry {
                name: b"a.md".to_vec(),
                regular: true,
                content: Err("cannot be read".into()),
            })
        });
    let fixture = fixture(readers);
    let (_, mut contents) = fixture.prepared(&path).await;

    assert!(contents.next().await.unwrap().is_err());
    assert!(contents.next().await.unwrap().is_ok());
}

#[tokio::test]
async fn when_the_rest_cannot_be_read_every_remaining_document_fails() {
    let (_directory, path) = archive();
    let readers = TestReaders::new(vec![document("a.md"), document("b.md"), document("c.md")])
        .on_read(|position, _| {
            (position == Position(1)).then(|| ReadResult::RestUnreadable("cut short".into()))
        });
    let fixture = fixture(readers);
    let (_, mut contents) = fixture.prepared(&path).await;

    assert!(contents.next().await.unwrap().is_ok());

    let rest = all_contents(&mut contents).await;
    assert_eq!(rest.len(), 2);
    assert!(rest.iter().all(Result::is_err));
    assert_eq!(fixture.readers.positions_read().len(), 2);
}

#[derive(Debug, thiserror::Error)]
#[error("outer")]
struct ChainedError(#[source] std::io::Error);

#[tokio::test]
async fn the_cause_given_to_the_rest_keeps_its_sources() {
    let (_directory, path) = archive();
    let readers = TestReaders::new(vec![document("a.md"), document("b.md")]).on_read(|_, _| {
        let cause = ChainedError(std::io::Error::other("inner"));
        Some(ReadResult::RestUnreadable(Box::new(cause)))
    });
    let fixture = fixture(readers);
    let (_, mut contents) = fixture.prepared(&path).await;

    let all = all_contents(&mut contents).await;

    assert_eq!(all.len(), 2);
    for content in all {
        assert_eq!(super::causes(content.unwrap_err().as_ref()), "outer: inner");
    }
}

#[tokio::test]
async fn when_the_second_pass_cannot_start_every_document_fails() {
    let (_directory, path) = archive();
    let readers =
        TestReaders::new(vec![document("a.md"), document("b.md")]).failing_start("broken");
    let fixture = fixture(readers);
    let (_, mut contents) = fixture.prepared(&path).await;

    let all = all_contents(&mut contents).await;

    assert_eq!(all.len(), 2);
    assert!(all.iter().all(Result::is_err));
}

#[tokio::test]
async fn failing_to_duplicate_the_file_for_the_first_pass_makes_the_source_unreadable() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![document("a.md")]).with_clone_failures(&[true]));

    assert_source_unreadable(fixture.update(&path).await);
}

#[tokio::test]
async fn failing_to_duplicate_the_file_for_the_second_pass_fails_every_document() {
    let (_directory, path) = archive();
    let fixture = fixture(
        TestReaders::new(vec![document("a.md"), document("b.md")])
            .with_clone_failures(&[false, true]),
    );
    let (_, mut contents) = fixture.prepared(&path).await;

    let all = all_contents(&mut contents).await;

    assert_eq!(all.len(), 2);
    assert!(all.iter().all(Result::is_err));
}

// Holding nothing between calls.

#[tokio::test]
async fn two_collections_can_be_updated_at_the_same_time() {
    let (_first_directory, first) = archive();
    let (_second_directory, second) = archive();
    let fixture = fixture(TestReaders::new(vec![document("a.md"), document("b.md")]));
    let sink = TestProgressSink::new();

    let (one, other) = tokio::join!(
        fixture.ingest.update(instruction_for("one", &first), &sink),
        fixture
            .ingest
            .update(instruction_for("other", &second), &sink),
    );

    assert_eq!(one.unwrap().added, 2);
    assert_eq!(other.unwrap().added, 2);
    assert_eq!(fixture.registered_keys_of("one"), ["a.md", "b.md"]);
    assert_eq!(fixture.registered_keys_of("other"), ["a.md", "b.md"]);
}

#[tokio::test]
async fn a_second_update_reads_the_list_again() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![document("a.md")]));
    fixture.update(&path).await.unwrap();

    fixture.readers.set_items(vec![document("b.md")]);
    fixture.update(&path).await.unwrap();

    assert_eq!(fixture.registered_keys(), ["a.md", "b.md"]);
}

// The update steps and the tasks.

#[tokio::test]
async fn the_extension_chooses_the_reader_for_both_passes() {
    for (name, kind) in [
        ("a.zip", ArchiveKind::Zip),
        ("a.tar", ArchiveKind::Tar),
        ("a.tar.gz", ArchiveKind::GzippedTar),
        ("a.tgz", ArchiveKind::GzippedTar),
        ("A.ZIP", ArchiveKind::Zip),
    ] {
        let (_directory, path) = archive_named(name);
        let fixture = fixture(TestReaders::new(vec![document("a.md")]));

        fixture.update(&path).await.unwrap();

        assert_eq!(fixture.readers.kinds_listed(), [kind], "{name}");
        assert_eq!(fixture.readers.kinds_read(), [kind], "{name}");
    }
}

#[tokio::test]
async fn an_unreadable_source_warns_counts_nothing_and_leaves_the_index_alone() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![document("a.md")]).failing_list("cut short"));

    let (result, logs) = capture_async(fixture.update(&path)).await;

    assert_source_unreadable(result);
    assert!(fixture.index.calls().is_empty());
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, Level::WARN);
    assert!(
        logs[0]
            .fields
            .contains(&("collection".to_owned(), "notes".to_owned()))
    );
    assert!(
        logs[0]
            .fields
            .contains(&("cause".to_owned(), "cut short".to_owned()))
    );
}

#[tokio::test]
async fn a_list_over_the_limit_is_an_unreadable_source() {
    let (_directory, path) = archive();
    let fixture = with_list_limit(vec![document("a.md")], 1);

    assert_source_unreadable(fixture.update(&path).await);
    assert!(fixture.index.calls().is_empty());
}

#[tokio::test]
async fn an_archive_that_cannot_be_opened_is_an_unreadable_source() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = fixture(TestReaders::new(Vec::new()));

    assert_source_unreadable(fixture.update(&directory.path().join("gone.zip")).await);
}

#[tokio::test]
async fn a_location_without_an_archive_extension_is_an_unreadable_source() {
    let (_directory, path) = archive_named("a.txt");
    let fixture = fixture(TestReaders::new(Vec::new()));

    assert_source_unreadable(fixture.update(&path).await);
}

#[tokio::test]
async fn a_location_without_a_name_is_an_unreadable_source() {
    let fixture = fixture(TestReaders::new(Vec::new()));

    assert_source_unreadable(fixture.update(Path::new("/")).await);
}

#[tokio::test]
async fn attributes_that_are_missing_or_extra_are_refused_before_anything_is_read() {
    let (_directory, path) = archive();
    let with_extra = json!({ "path": path.to_str().unwrap(), "extra": true });
    for attributes in [
        TypeAttributes::default(),
        TypeAttributes(with_extra.as_object().unwrap().clone()),
    ] {
        let fixture = fixture(TestReaders::new(vec![document("a.md")]));
        let mut update = instruction(&path);
        update.attributes = attributes;

        let stopped = fixture
            .ingest
            .update(update, &TestProgressSink::new())
            .await
            .unwrap_err();

        assert!(matches!(
            stopped.error,
            IngestError::AttributesMissingOrExtra
        ));
        assert!(fixture.readers.kinds_listed().is_empty());
        assert!(fixture.index.calls().is_empty());
    }
}

#[tokio::test]
async fn nothing_is_read_before_it_is_asked_for() {
    let (_directory, path) = archive();
    let fixture = fixture(TestReaders::new(vec![document("a.md"), document("b.md")]));
    let (_, mut contents) = fixture.prepared(&path).await;

    assert!(fixture.readers.kinds_read().is_empty());

    contents.next().await.unwrap().unwrap();
    assert_eq!(fixture.readers.kinds_read().len(), 1);
    assert_eq!(fixture.readers.positions_read().len(), 1);

    contents.next().await.unwrap().unwrap();
    assert_eq!(fixture.readers.kinds_read().len(), 1);
    assert_eq!(fixture.readers.positions_read().len(), 2);

    assert!(contents.next().await.is_none());
}

async fn panic_message(future: impl Future) -> String {
    let panic = AssertUnwindSafe(future).catch_unwind().await.err().unwrap();
    panic.downcast_ref::<&str>().unwrap().to_string()
}

#[tokio::test]
async fn a_panic_in_the_first_pass_comes_through_the_update() {
    let (_directory, path) = archive();
    let fixture = fixture(
        TestReaders::new(vec![document("a.md")]).on_list_step(|| panic!("the listing panicked")),
    );

    assert_eq!(
        panic_message(fixture.update(&path)).await,
        "the listing panicked"
    );
}

#[tokio::test]
async fn a_panic_in_the_second_pass_comes_through_next() {
    let (_directory, path) = archive();
    let fixture = fixture(
        TestReaders::new(vec![document("a.md")]).on_read(|_, _| panic!("the reading panicked")),
    );
    let (_, mut contents) = fixture.prepared(&path).await;

    assert_eq!(panic_message(contents.next()).await, "the reading panicked");
    assert!(contents.next().await.is_none());
}

/// A signal that a reader waits for, with a limit on the wait: the wait blocks
/// its thread, so the runtime's own timers would not help.
#[derive(Clone, Default)]
struct Signal(Arc<(Mutex<bool>, Condvar)>);

impl Signal {
    fn send(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }

    fn wait(&self) {
        let (flag, _) = self
            .0
            .1
            .wait_timeout_while(self.0.0.lock().unwrap(), TIMEOUT, |sent| !*sent)
            .unwrap();
        assert!(
            *flag,
            "nothing sent the signal: the reader blocked the runtime"
        );
    }
}

#[tokio::test]
async fn the_first_pass_does_not_stop_other_tasks() {
    let (_directory, path) = archive();
    let signal = Signal::default();
    let waiting = signal.clone();
    let fixture = fixture(TestReaders::new(Vec::new()).on_list_step(move || waiting.wait()));

    tokio::spawn(async move { signal.send() });

    fixture.update(&path).await.unwrap();
}

#[tokio::test]
async fn the_second_pass_does_not_stop_other_tasks() {
    let (_directory, path) = archive();
    let signal = Signal::default();
    let waiting = signal.clone();
    let fixture = fixture(
        TestReaders::new(vec![document("a.md")]).on_read(move |_, _| {
            waiting.wait();
            None
        }),
    );
    let (_, mut contents) = fixture.prepared(&path).await;

    tokio::spawn(async move { signal.send() });

    contents.next().await.unwrap().unwrap();
}

/// Lets `count` callers through only together.
struct Rendezvous(Arc<(Mutex<usize>, Condvar)>, usize);

impl Rendezvous {
    fn new(count: usize) -> Self {
        Self(Arc::new((Mutex::new(0), Condvar::new())), count)
    }

    fn arrive(&self) {
        let mut arrived = self.0.0.lock().unwrap();
        *arrived += 1;
        self.0.1.notify_all();
        let (arrived, _) = self
            .0
            .1
            .wait_timeout_while(arrived, TIMEOUT, |arrived| *arrived < self.1)
            .unwrap();
        assert!(
            *arrived >= self.1,
            "the documents were not processed at once"
        );
    }
}

#[tokio::test]
async fn the_parallelism_reaches_the_shared_flow() {
    let (_directory, path) = archive();
    let rendezvous = Rendezvous::new(2);
    let format = TestFormat::new()
        .with_extension("md", "markdown")
        .on_extract(move |content, _| {
            rendezvous.arrive();
            Ok(whole_text(content))
        });
    let ingest = ArchiveIngest::new(
        Arc::new(format),
        Arc::new(TestIndex::new()),
        Arc::new(TestReaders::new(vec![document("a.md"), document("b.md")])),
        NO_LIMIT,
    );
    let mut update = instruction(&path);
    update.parallelism = 2;

    let report = ingest
        .update(update, &TestProgressSink::new())
        .await
        .unwrap();

    assert_eq!(report.added, 2);
}
