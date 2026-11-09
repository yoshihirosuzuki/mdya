//! The `archive_ingest` component: ingests an `archive` collection.

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
use first_pass::{FirstPass, Listed};
use kind::ArchiveKind;
use readers::{Readers, RealReaders};
use second_pass::ArchiveContents;

mod blocking;
mod first_pass;
mod kind;
mod names;
mod readers;
mod second_pass;
mod stamp;
mod tar;
mod transfer;
mod zip;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod test_readers;

const MIB: u64 = 1_048_576;

/// The limits in use, which cannot be configured.
const LIMITS: Limits = Limits {
    list: 256 * MIB,
    content: 256 * MIB,
};

/// Creates the component.
pub fn create(format: Arc<dyn Format>, index: Arc<dyn Index>) -> Arc<dyn Ingest> {
    Arc::new(ArchiveIngest::new(
        format,
        index,
        Arc::new(RealReaders),
        LIMITS,
    ))
}

/// What keeps one archive from taking too much memory.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// The size of the list of entries, in bytes.
    list: u64,
    /// The size of one document once inflated, in bytes.
    content: u64,
}

/// Holds nothing that changes between calls: all that an update reads lives in
/// that call, so that updates at the same time do not affect each other.
struct ArchiveIngest {
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
    readers: Arc<dyn Readers>,
    limits: Limits,
}

#[derive(Debug, thiserror::Error)]
#[error("the location does not have the extension of an archive")]
struct NotAnArchiveName;

#[derive(Debug, thiserror::Error)]
#[error("the absolute location is not valid UTF-8")]
struct LocationNotUtf8;

impl ArchiveIngest {
    fn new(
        format: Arc<dyn Format>,
        index: Arc<dyn Index>,
        readers: Arc<dyn Readers>,
        limits: Limits,
    ) -> Self {
        Self {
            format,
            index,
            readers,
            limits,
        }
    }

    /// Reads the list of entries, and gets ready to hand over the content.
    async fn prepare(
        &self,
        instruction: &UpdateInstruction,
    ) -> Result<(IngestItems, ArchiveContents), UpdateStopped> {
        let location = location_of(&instruction.attributes).map_err(stopped)?;
        match self.read_list(PathBuf::from(location)).await {
            Ok(listed) => Ok((listed.items, ArchiveContents::new(listed.second_pass))),
            Err(cause) => {
                warn_source_unreadable(&instruction.collection.0, cause.as_ref());
                Err(stopped(IngestError::SourceUnreadable { source: cause }))
            }
        }
    }

    async fn read_list(&self, path: PathBuf) -> Result<Listed, BoxError> {
        let kind = ArchiveKind::of_path(&path).ok_or(NotAnArchiveName)?;
        let first_pass = FirstPass {
            path,
            kind,
            readers: Arc::clone(&self.readers),
            format: Arc::clone(&self.format),
            limits: self.limits,
        };
        run_blocking(move || first_pass.run()).await
    }
}

#[async_trait]
impl Ingest for ArchiveIngest {
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
        let absolute = make_path_absolute(Path::new(location), dirs)
            .map_err(location_not_found)?
            .into_os_string()
            .into_string()
            .map_err(|_| location_not_found(LocationNotUtf8))?;
        let metadata = tokio::fs::metadata(&absolute)
            .await
            .map_err(location_not_found)?;
        if !metadata.is_file() || ArchiveKind::of_path(Path::new(&absolute)).is_none() {
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

/// The name of the one type-specific attribute: where the archive is.
const PATH_ATTRIBUTE: &str = "path";

/// Checks the names of the type-specific attributes: there is `path` and
/// nothing else, and its value is a string. Whether the location exists is
/// not checked. `file_ingest` checks its attributes by the same rule in its
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
    use serde_json::{Value, json};

    use super::*;

    fn attributes(value: Value) -> TypeAttributes {
        TypeAttributes(value.as_object().unwrap().clone())
    }

    #[test]
    fn accepts_a_path_alone_whose_value_is_a_string() {
        for value in [
            json!({ "path": "/home/user/docs.zip" }),
            json!({ "path": "" }),
        ] {
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
            json!({ "location": "/home/user/docs.zip" }),
            json!({ "path": "/home/user/docs.zip", "recursive": true }),
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
            json!({ "path": ["/home/user/docs.zip"] }),
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
