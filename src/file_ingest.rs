//! The `file_ingest` component: ingests a `directory` collection.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::format::Format;
use crate::index::Index;
use crate::ingest::{
    Ingest, IngestError, IngestItems, PutInstruction, UpdateInstruction, UpdateReport,
    UpdateStopped, ingest_items,
};
use crate::path::make_path_absolute;
use crate::types::{BoxError, DirLookup, DocumentOutcome, ProgressSink, TypeAttributes};

use blocking::run_blocking;
use contents::FileContents;
use walk::walk;

mod blocking;
mod contents;
mod names;
mod walk;

/// Creates the component.
pub fn create(format: Arc<dyn Format>, index: Arc<dyn Index>) -> Arc<dyn Ingest> {
    Arc::new(FileIngest { format, index })
}

/// Holds nothing that changes between calls: all that an update reads lives in
/// that call, so that updates at the same time do not affect each other.
struct FileIngest {
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
}

#[derive(Debug, thiserror::Error)]
#[error("the absolute location is not valid UTF-8")]
struct LocationNotUtf8;

impl FileIngest {
    /// Walks the directory, and gets ready to hand over the content.
    async fn prepare(
        &self,
        instruction: &UpdateInstruction,
    ) -> Result<(IngestItems, FileContents), UpdateStopped> {
        let root = PathBuf::from(location_of(&instruction.attributes).map_err(stopped)?);
        let collection = instruction.collection.0.clone();
        let format = Arc::clone(&self.format);
        let walked = run_blocking(move || walk(&root, &collection, format.as_ref())).await;
        match walked {
            Ok(walked) => Ok((walked.items, FileContents::new(walked.files))),
            Err(cause) => {
                warn_source_unreadable(&instruction.collection.0, &cause);
                Err(stopped(IngestError::SourceUnreadable {
                    source: cause.into(),
                }))
            }
        }
    }
}

#[async_trait]
impl Ingest for FileIngest {
    fn supports_update(&self) -> bool {
        true
    }

    fn supports_put(&self) -> bool {
        false
    }

    async fn check_attributes(
        &self,
        attributes: TypeAttributes,
        dirs: &DirLookup,
    ) -> Result<TypeAttributes, IngestError> {
        let location = location_of(&attributes)?;
        let absolute = absolute_location(location, dirs)?;
        let metadata = tokio::fs::metadata(&absolute)
            .await
            .map_err(location_not_found)?;
        if !metadata.is_dir() {
            return Err(IngestError::LocationTypeMismatch);
        }
        let mut checked = serde_json::Map::new();
        checked.insert(PATH_ATTRIBUTE.to_owned(), Value::String(absolute));
        Ok(TypeAttributes(checked))
    }

    async fn update(
        &self,
        instruction: UpdateInstruction,
        progress: &dyn ProgressSink,
    ) -> Result<UpdateReport, UpdateStopped> {
        let (items, mut contents) = self.prepare(&instruction).await?;
        ingest_items(
            &instruction,
            items,
            &mut contents,
            Arc::clone(&self.format),
            Arc::clone(&self.index),
            progress,
        )
        .await
    }

    async fn put(&self, _instruction: PutInstruction) -> Result<DocumentOutcome, IngestError> {
        Err(IngestError::PutNotSupported)
    }
}

/// The location in the attributes, once their names have been checked.
fn location_of(attributes: &TypeAttributes) -> Result<&str, IngestError> {
    check_attribute_names(attributes)?;
    attributes
        .0
        .get(PATH_ATTRIBUTE)
        .and_then(Value::as_str)
        .ok_or(IngestError::AttributesMissingOrExtra)
}

fn absolute_location(location: &str, dirs: &DirLookup) -> Result<String, IngestError> {
    make_path_absolute(Path::new(location), dirs)
        .map_err(location_not_found)?
        .into_os_string()
        .into_string()
        .map_err(|_| location_not_found(LocationNotUtf8))
}

fn location_not_found(source: impl Into<BoxError>) -> IngestError {
    IngestError::LocationNotFound {
        source: source.into(),
    }
}

fn stopped(error: IngestError) -> UpdateStopped {
    UpdateStopped {
        counts: UpdateReport::default(),
        error,
    }
}

fn warn_source_unreadable(collection: &str, cause: &(dyn Error + 'static)) {
    tracing::warn!(
        collection,
        cause = causes(cause).as_str(),
        "the source cannot be read and the collection was not updated"
    );
}

/// The error and its sources, joined with `: `.
fn causes(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

/// The name of the one type-specific attribute: where the directory is.
const PATH_ATTRIBUTE: &str = "path";

/// Checks the names of the type-specific attributes: there is `path` and
/// nothing else, and its value is a string. Whether the location exists is
/// not checked. `archive_ingest` checks its attributes by the same rule in its
/// own code, so change the two together. Callable without creating the
/// component.
pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError> {
    let only_a_path_string = attributes.0.len() == 1
        && attributes
            .0
            .get(PATH_ATTRIBUTE)
            .is_some_and(serde_json::Value::is_string);
    if only_a_path_string {
        return Ok(());
    }
    Err(IngestError::AttributesMissingOrExtra)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::io;
    use std::panic::AssertUnwindSafe;
    use std::path::Path;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use futures::FutureExt;
    use serde_json::{Value, json};
    use tracing::Level;

    use super::*;
    use crate::format::test_double::{TestFormat, whole_text};
    use crate::format::{Extracted, FileType, FormatError};
    use crate::index::IndexError;
    use crate::index::test_double::{IndexCall, TestIndex};
    use crate::ingest::PutInstruction;
    use crate::path::MakePathAbsoluteError;
    use crate::test_support::log_receiver::capture_async;
    use crate::types::test_double::{TestProgressSink, test_dir_lookup};
    use crate::types::{CollectionName, Key, Progress};

    fn attributes(value: Value) -> TypeAttributes {
        TypeAttributes(value.as_object().unwrap().clone())
    }

    fn attributes_at(location: &str) -> TypeAttributes {
        attributes(json!({ "path": location }))
    }

    fn component() -> Arc<dyn Ingest> {
        create(Arc::new(TestFormat::new()), Arc::new(TestIndex::new()))
    }

    /// For a location that is already absolute, which needs neither directory.
    fn any_directories() -> DirLookup {
        test_dir_lookup(Some("/home/user"), Ok("/work"))
    }

    async fn check(location: &Path) -> Result<TypeAttributes, IngestError> {
        component()
            .check_attributes(
                attributes_at(location.to_str().unwrap()),
                &any_directories(),
            )
            .await
    }

    async fn check_with(location: &str, dirs: &DirLookup) -> Result<TypeAttributes, IngestError> {
        component()
            .check_attributes(attributes_at(location), dirs)
            .await
    }

    fn cause_of_not_found(result: Result<TypeAttributes, IngestError>) -> BoxError {
        match result {
            Err(IngestError::LocationNotFound { source }) => source,
            other => panic!("{other:?}"),
        }
    }

    // What the component says it can do, and the operation it does not offer.

    #[test]
    fn it_can_be_updated_but_does_not_take_document_content() {
        let ingest = component();

        assert!(ingest.supports_update());
        assert!(!ingest.supports_put());
    }

    #[tokio::test]
    async fn taking_document_content_is_refused_without_touching_the_index() {
        let index = Arc::new(TestIndex::new());
        let ingest = create(Arc::new(TestFormat::new()), index.clone());
        let instruction = PutInstruction {
            collection: CollectionName("notes".to_owned()),
            attributes: attributes_at("/home/user/docs"),
            key: Key("a.md".to_owned()),
            media_type: "text/markdown".to_owned(),
            content: b"# A".to_vec(),
            overwrite: false,
        };

        let result = ingest.put(instruction).await;

        assert!(matches!(result, Err(IngestError::PutNotSupported)));
        assert!(index.calls().is_empty());
    }

    // Updating from the directory.

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn write_file(root: &Path, relative: &str, content: &[u8]) {
        let file = root.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn markdown() -> TestFormat {
        TestFormat::new().with_extension("md", "markdown")
    }

    fn instruction(location: &Path) -> UpdateInstruction {
        UpdateInstruction {
            collection: CollectionName("notes".to_owned()),
            attributes: attributes_at(location.to_str().unwrap()),
            overwrite: false,
            parallelism: 1,
        }
    }

    fn updater(format: TestFormat, index: &Arc<TestIndex>) -> Arc<dyn Ingest> {
        create(Arc::new(format), index.clone())
    }

    async fn update(
        ingest: &Arc<dyn Ingest>,
        location: &Path,
    ) -> Result<UpdateReport, UpdateStopped> {
        ingest
            .update(instruction(location), &TestProgressSink::new())
            .await
    }

    fn registered_keys(index: &TestIndex) -> Vec<String> {
        index
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                IndexCall::Register(document) => Some(document.key.0),
                _ => None,
            })
            .collect()
    }

    fn deletion(index: &TestIndex) -> Option<(HashSet<Key>, Vec<String>)> {
        index.calls().into_iter().find_map(|call| match call {
            IndexCall::DeleteOthers {
                keep,
                unreadable_dirs,
                ..
            } => Some((keep, unreadable_dirs)),
            _ => None,
        })
    }

    fn set_of_keys(names: &[&str]) -> HashSet<Key> {
        names.iter().map(|name| Key((*name).to_owned())).collect()
    }

    #[tokio::test]
    async fn an_update_registers_the_documents_and_deletes_the_others() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        write_file(directory.path(), "sub/b.md", b"# B");
        write_file(directory.path(), "c.txt", b"not a document");
        let index = Arc::new(TestIndex::new().on_delete_others(|_, _, _| Ok(3)));
        let ingest = updater(markdown(), &index);

        let report = update(&ingest, directory.path()).await.unwrap();

        assert_eq!(report.added, 2);
        assert_eq!(report.deleted, 3);
        assert_eq!(report.unreadable_places, 0);
        assert_eq!(registered_keys(&index), ["a.md", "sub/b.md"]);
        let (keep, unreadable_dirs) = deletion(&index).unwrap();
        assert_eq!(keep, set_of_keys(&["a.md", "sub/b.md"]));
        assert!(unreadable_dirs.is_empty());
    }

    #[tokio::test]
    async fn an_update_hands_each_files_content_to_format_in_order() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"first");
        write_file(directory.path(), "b.md", b"second");
        let format = Arc::new(markdown());
        let ingest = create(format.clone(), Arc::new(TestIndex::new()));

        update(&ingest, directory.path()).await.unwrap();

        let contents: Vec<Vec<u8>> = format
            .extracted()
            .into_iter()
            .map(|(content, _)| content)
            .collect();
        assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
    }

    #[tokio::test]
    async fn an_update_of_an_empty_directory_deletes_everything() {
        let directory = tempfile::tempdir().unwrap();
        let index = Arc::new(TestIndex::new());
        let ingest = updater(markdown(), &index);

        update(&ingest, directory.path()).await.unwrap();

        assert_eq!(deletion(&index), Some((HashSet::new(), Vec::new())));
    }

    #[tokio::test]
    async fn progress_counts_the_documents_found() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        write_file(directory.path(), "b.md", b"# B");
        let ingest = updater(markdown(), &Arc::new(TestIndex::new()));
        let progress = TestProgressSink::new();

        ingest
            .update(instruction(directory.path()), &progress)
            .await
            .unwrap();

        let last = *progress.reports().last().unwrap();
        assert_eq!(
            last,
            Progress {
                done: 2,
                total: Some(2)
            }
        );
    }

    #[tokio::test]
    async fn a_directory_that_cannot_be_read_stops_the_update_and_touches_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let index = Arc::new(TestIndex::new());
        let ingest = updater(markdown(), &index);
        let gone = directory.path().join("gone");

        let (result, logs) = capture_async(update(&ingest, &gone)).await;

        let stopped = result.unwrap_err();
        let IngestError::SourceUnreadable { source } = stopped.error else {
            panic!("{:?}", stopped.error);
        };
        assert!(source.downcast_ref::<io::Error>().is_some());
        assert_eq!(stopped.counts, UpdateReport::default());
        assert!(index.calls().is_empty());
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, Level::WARN);
        assert!(
            logs[0]
                .fields
                .contains(&("collection".to_owned(), "notes".to_owned()))
        );
    }

    #[tokio::test]
    async fn attributes_without_a_path_stop_the_update_and_touch_nothing() {
        let index = Arc::new(TestIndex::new());
        let ingest = updater(markdown(), &index);
        let mut instruction = instruction(Path::new("/unused"));
        instruction.attributes = TypeAttributes::default();

        let stopped = ingest
            .update(instruction, &TestProgressSink::new())
            .await
            .unwrap_err();

        assert!(matches!(
            stopped.error,
            IngestError::AttributesMissingOrExtra
        ));
        assert_eq!(stopped.counts, UpdateReport::default());
        assert!(index.calls().is_empty());
    }

    #[tokio::test]
    async fn an_error_from_the_index_comes_back_with_the_counts_so_far() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        write_file(directory.path(), "b.md", b"# B");
        let index = Arc::new(TestIndex::new().on_register(|document, _| {
            if document.key.0 == "a.md" {
                return Ok(DocumentOutcome::Added);
            }
            Err(IndexError::CannotRead {
                source: "the index is gone".into(),
            })
        }));
        let ingest = updater(markdown(), &index);

        let stopped = update(&ingest, directory.path()).await.unwrap_err();

        assert!(matches!(stopped.error, IngestError::Index(_)));
        assert_eq!(stopped.counts.added, 1);
        assert_eq!(deletion(&index), None);
    }

    #[tokio::test]
    async fn a_document_that_cannot_be_extracted_is_counted_as_failed_and_stays_kept() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"bad");
        write_file(directory.path(), "b.md", b"good");
        let format = markdown().on_extract(|content, _| {
            if content == b"bad" {
                return Err(FormatError::CannotExtract {
                    source: "not text".into(),
                });
            }
            Ok(whole_text(content))
        });
        let index = Arc::new(TestIndex::new());
        let ingest = updater(format, &index);

        let report = update(&ingest, directory.path()).await.unwrap();

        assert_eq!(report.failed, 1);
        assert_eq!(report.added, 1);
        let (keep, _) = deletion(&index).unwrap();
        assert_eq!(keep, set_of_keys(&["a.md", "b.md"]));
    }

    /// Holds each caller until `count` callers have arrived, so that a test
    /// gets through only if that many run at once.
    #[derive(Clone)]
    struct Rendezvous {
        arrived: Arc<(Mutex<usize>, Condvar)>,
        count: usize,
    }

    impl Rendezvous {
        fn new(count: usize) -> Self {
            Self {
                arrived: Arc::new((Mutex::new(0), Condvar::new())),
                count,
            }
        }

        fn arrive(&self) {
            let (arrived, changed) = &*self.arrived;
            let mut arrived = arrived.lock().unwrap();
            *arrived += 1;
            changed.notify_all();
            let (arrived, _) = changed
                .wait_timeout_while(arrived, TIMEOUT, |arrived| *arrived < self.count)
                .unwrap();
            assert!(*arrived >= self.count, "the documents did not run at once");
        }
    }

    #[tokio::test]
    async fn the_parallelism_reaches_the_shared_flow() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        write_file(directory.path(), "b.md", b"# B");
        let rendezvous = Rendezvous::new(2);
        let format = markdown().on_extract(move |content, _| {
            rendezvous.arrive();
            Ok(whole_text(content))
        });
        let ingest = updater(format, &Arc::new(TestIndex::new()));
        let mut parallel = instruction(directory.path());
        parallel.parallelism = 2;

        let report = ingest
            .update(parallel, &TestProgressSink::new())
            .await
            .unwrap();

        assert_eq!(report.added, 2);
    }

    /// A `Format` that panics when asked about an extension, to reach the
    /// blocking thread the walk runs on.
    struct PanickingFormat;

    impl Format for PanickingFormat {
        fn file_type_from_extension(&self, _extension: &str) -> Option<FileType> {
            panic!("the walk panicked")
        }

        fn file_type_from_mime_or_alias(&self, _value: &str) -> Option<FileType> {
            None
        }

        fn extract(
            &self,
            _content: &[u8],
            _file_type: &FileType,
        ) -> Result<Extracted, FormatError> {
            unreachable!("the walk panics first")
        }
    }

    #[tokio::test]
    async fn a_panic_during_the_walk_reaches_the_caller_as_a_panic() {
        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        let ingest = create(Arc::new(PanickingFormat), Arc::new(TestIndex::new()));

        let panic = AssertUnwindSafe(update(&ingest, directory.path()))
            .catch_unwind()
            .await
            .err()
            .unwrap();

        assert_eq!(panic.downcast_ref::<&str>(), Some(&"the walk panicked"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_subdirectory_is_kept_out_of_the_deletion_and_counted() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        write_file(directory.path(), "a.md", b"# A");
        write_file(directory.path(), "locked/b.md", b"# B");
        let locked = directory.path().join("locked");
        fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let superuser_reads_anything = fs::read_dir(&locked).is_ok();
        let index = Arc::new(TestIndex::new());
        let ingest = updater(markdown(), &index);

        let result = update(&ingest, directory.path()).await;

        fs::set_permissions(&locked, Permissions::from_mode(0o700)).unwrap();
        if superuser_reads_anything {
            return;
        }
        let report = result.unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(report.unreadable_places, 1);
        assert_eq!(
            deletion(&index),
            Some((set_of_keys(&["a.md"]), vec!["locked".to_owned()]))
        );
    }

    // Checking the type-specific attributes against the file system.

    #[tokio::test]
    async fn the_attribute_names_are_checked_before_the_location() {
        let extra = attributes(json!({ "path": "/no/such/directory", "recursive": true }));

        let result = component()
            .check_attributes(extra, &any_directories())
            .await;

        assert!(matches!(result, Err(IngestError::AttributesMissingOrExtra)));
    }

    #[tokio::test]
    async fn a_directory_is_accepted_and_keeps_its_absolute_location() {
        let directory = tempfile::tempdir().unwrap();

        let checked = check(directory.path()).await.unwrap();

        assert_eq!(checked, attributes_at(directory.path().to_str().unwrap()));
    }

    #[tokio::test]
    async fn a_missing_location_is_not_found_with_its_cause() {
        let directory = tempfile::tempdir().unwrap();

        let cause = cause_of_not_found(check(&directory.path().join("gone")).await);

        let cause = cause.downcast_ref::<io::Error>().unwrap();
        assert_eq!(cause.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn a_file_does_not_fit_the_collection_type() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("a.md");
        fs::write(&file, b"# A").unwrap();

        let result = check(&file).await;

        assert!(matches!(result, Err(IngestError::LocationTypeMismatch)));
    }

    // Making the location absolute.

    #[tokio::test]
    async fn a_relative_location_comes_back_absolute() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("docs")).unwrap();
        let working_dir = directory.path().to_str().unwrap();
        let dirs = test_dir_lookup(Some("/home/user"), Ok(working_dir));

        let checked = check_with("docs", &dirs).await.unwrap();

        let expected = directory.path().join("docs");
        assert_eq!(checked, attributes_at(expected.to_str().unwrap()));
    }

    #[tokio::test]
    async fn a_leading_tilde_becomes_the_home_directory() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("docs")).unwrap();
        let home_dir = directory.path().to_str().unwrap();
        let dirs = test_dir_lookup(Some(home_dir), Ok("/work"));

        let checked = check_with("~/docs", &dirs).await.unwrap();

        let expected = directory.path().join("docs");
        assert_eq!(checked, attributes_at(expected.to_str().unwrap()));
    }

    #[tokio::test]
    async fn single_dot_components_are_removed_and_double_dot_components_kept() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("docs")).unwrap();
        let root = directory.path();
        let location = root.join(".").join("docs").join("..").join("docs");

        let checked = check(&location).await.unwrap();

        let expected = root.join("docs").join("..").join("docs");
        assert_eq!(checked, attributes_at(expected.to_str().unwrap()));
    }

    #[tokio::test]
    async fn an_unknown_home_directory_is_not_found_with_the_path_error_as_its_cause() {
        let dirs = test_dir_lookup(None, Ok("/work"));

        let cause = cause_of_not_found(check_with("~/docs", &dirs).await);

        assert!(matches!(
            cause.downcast_ref::<MakePathAbsoluteError>(),
            Some(MakePathAbsoluteError::HomeDirUnknown)
        ));
    }

    #[tokio::test]
    async fn an_unreadable_working_directory_is_not_found_with_the_path_error_as_its_cause() {
        let dirs = test_dir_lookup(Some("/home/user"), Err(io::ErrorKind::PermissionDenied));

        let cause = cause_of_not_found(check_with("docs", &dirs).await);

        assert!(matches!(
            cause.downcast_ref::<MakePathAbsoluteError>(),
            Some(MakePathAbsoluteError::WorkingDirUnreadable(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_working_directory_that_is_not_utf8_is_not_found() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::PathBuf;

        let dirs = DirLookup {
            home_dir: Arc::new(|| None),
            current_dir: Arc::new(|| Ok(PathBuf::from(OsStr::from_bytes(b"/\xFF")))),
        };

        let cause = cause_of_not_found(check_with("docs", &dirs).await);

        assert!(cause.downcast_ref::<LocationNotUtf8>().is_some());
    }

    #[cfg(unix)]
    mod links {
        use std::os::unix::fs::symlink;

        use super::*;

        #[tokio::test]
        async fn a_link_to_a_directory_is_accepted_and_stays_a_link() {
            let directory = tempfile::tempdir().unwrap();
            let link = directory.path().join("link");
            symlink(directory.path(), &link).unwrap();

            let checked = check(&link).await.unwrap();

            assert_eq!(checked, attributes_at(link.to_str().unwrap()));
        }

        #[tokio::test]
        async fn a_link_to_a_file_does_not_fit() {
            let directory = tempfile::tempdir().unwrap();
            let file = directory.path().join("a.md");
            fs::write(&file, b"# A").unwrap();
            let link = directory.path().join("link");
            symlink(&file, &link).unwrap();

            let result = check(&link).await;

            assert!(matches!(result, Err(IngestError::LocationTypeMismatch)));
        }

        #[tokio::test]
        async fn a_link_to_nothing_is_not_found() {
            let directory = tempfile::tempdir().unwrap();
            let link = directory.path().join("link");
            symlink(directory.path().join("gone"), &link).unwrap();

            let result = check(&link).await;

            assert!(matches!(result, Err(IngestError::LocationNotFound { .. })));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_location_whose_kind_cannot_be_read_is_not_found_with_its_cause() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let locked = directory.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let superuser_reads_anything = fs::read_dir(&locked).is_ok();

        let result = check(&locked.join("docs")).await;

        fs::set_permissions(&locked, Permissions::from_mode(0o700)).unwrap();
        if superuser_reads_anything {
            return;
        }
        let cause = cause_of_not_found(result);
        let cause = cause.downcast_ref::<io::Error>().unwrap();
        assert_eq!(cause.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn accepts_a_path_alone_whose_value_is_a_string() {
        for value in [json!({ "path": "/home/user/docs" }), json!({ "path": "" })] {
            assert!(
                check_attribute_names(&attributes(value.clone())).is_ok(),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_missing_or_extra_attributes() {
        for value in [
            json!({}),
            json!({ "location": "/home/user/docs" }),
            json!({ "path": "/home/user/docs", "recursive": true }),
        ] {
            assert!(
                matches!(
                    check_attribute_names(&attributes(value.clone())),
                    Err(IngestError::AttributesMissingOrExtra)
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_a_path_whose_value_is_not_a_string() {
        for value in [
            json!({ "path": null }),
            json!({ "path": 1 }),
            json!({ "path": ["/home/user/docs"] }),
        ] {
            assert!(
                matches!(
                    check_attribute_names(&attributes(value.clone())),
                    Err(IngestError::AttributesMissingOrExtra)
                ),
                "{value}"
            );
        }
    }
}
