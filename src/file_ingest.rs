//! The `file_ingest` component: ingests a `directory` collection.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::format::Format;
use crate::index::Index;
use crate::ingest::{
    Ingest, IngestError, PutInstruction, UpdateInstruction, UpdateReport, UpdateStopped,
};
use crate::path::make_path_absolute;
use crate::types::{BoxError, DirLookup, DocumentOutcome, ProgressSink, TypeAttributes};

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
        let _ = (&self.format, &self.index, instruction, progress);
        todo!()
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
    use std::fs;
    use std::io;
    use std::path::Path;

    use serde_json::{Value, json};

    use super::*;
    use crate::format::test_double::TestFormat;
    use crate::index::test_double::TestIndex;
    use crate::ingest::PutInstruction;
    use crate::path::MakePathAbsoluteError;
    use crate::types::test_double::test_dir_lookup;
    use crate::types::{CollectionName, Key};

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
