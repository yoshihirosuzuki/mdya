//! The `ingest` role: what every collection type's ingest implementation
//! (`file_ingest`, `archive_ingest`, `raw_ingest`) offers, and the ingest flow
//! they share.

use async_trait::async_trait;

use crate::format::{FileType, FormatError};
use crate::index::IndexError;
use crate::types::{
    BoxError, CollectionName, DirLookup, DocumentOutcome, Key, ProgressSink, Time, TypeAttributes,
};

mod flow;

pub use flow::ingest_items;

/// An ingest implementation. Each one is created by its own module's `create`
/// and also offers `check_attribute_names`, callable without creating it:
///
/// ```ignore
/// pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError>;
/// ```
#[async_trait]
pub trait Ingest: Send + Sync {
    /// Whether the collection type can be updated from its source.
    fn supports_update(&self) -> bool;
    /// Whether documents can be ingested by passing their content.
    fn supports_put(&self) -> bool;
    /// Checks the type-specific attributes and returns them with locations
    /// made absolute by [`crate::path::make_path_absolute`], which takes
    /// `dirs`.
    async fn check_attributes(
        &self,
        attributes: TypeAttributes,
        dirs: &DirLookup,
    ) -> Result<TypeAttributes, IngestError>;
    /// Updates the collection from its source.
    async fn update(
        &self,
        instruction: UpdateInstruction,
        progress: &dyn ProgressSink,
    ) -> Result<UpdateReport, UpdateStopped>;
    /// Ingests one document from the content passed in.
    async fn put(&self, instruction: PutInstruction) -> Result<DocumentOutcome, IngestError>;
}

/// An instruction to update a collection.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInstruction {
    pub collection: CollectionName,
    pub attributes: TypeAttributes,
    pub overwrite: bool,
    /// How many documents to process at once (1 to 1024).
    pub parallelism: usize,
}

/// An instruction to ingest one document from its content.
#[derive(Clone, Debug, PartialEq)]
pub struct PutInstruction {
    pub collection: CollectionName,
    /// Empty for a `raw` collection.
    pub attributes: TypeAttributes,
    pub key: Key,
    /// The MIME type or alias the user gave.
    pub media_type: String,
    pub content: Vec<u8>,
    pub overwrite: bool,
}

/// The number of documents for each outcome of an update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub added: u64,
    pub changed: u64,
    pub unchanged: u64,
    pub reingested: u64,
    pub deleted: u64,
    pub failed: u64,
    /// The places that could not be read while walking the source.
    pub unreadable_places: u64,
}

/// The error of `update`, with the counts up to where it stopped.
#[derive(Debug, thiserror::Error)]
#[error("the update stopped")]
pub struct UpdateStopped {
    pub counts: UpdateReport,
    #[source]
    pub error: IngestError,
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("the type-specific attributes are missing or unexpected")]
    AttributesMissingOrExtra,
    /// The location was not found, or its kind could not be read. `source`
    /// tells why, including a [`crate::path::MakePathAbsoluteError`] when the
    /// location could not be made absolute.
    #[error("the location was not found")]
    LocationNotFound { source: BoxError },
    #[error("the location does not fit the collection type")]
    LocationTypeMismatch,
    #[error("this collection type cannot be updated")]
    UpdateNotSupported,
    #[error("this collection type does not take document content")]
    PutNotSupported,
    /// Only `put` returns it.
    #[error("the file type is not supported")]
    UnsupportedFileType,
    /// Only `put` returns it.
    #[error("the document's body cannot be extracted")]
    CannotExtract(#[source] FormatError),
    /// Only `update` returns it.
    #[error("the source cannot be read")]
    SourceUnreadable { source: BoxError },
    /// `index` failed while registering or deleting documents.
    #[error("the index failed")]
    Index(#[source] IndexError),
}

/// The items an implementation hands to the shared ingest flow.
#[derive(Debug)]
pub struct IngestItems {
    /// The items with a key. Their contents follow in this order.
    pub items: Vec<IngestItem>,
    /// The items without a key: the name to show in the warning, and why.
    pub unreadable_without_key: Vec<(String, BoxError)>,
    /// `None` when no documents are to be deleted. Otherwise, the key prefixes
    /// of the directories that could not be read; documents under them are
    /// kept. A key is under a directory when it begins with the directory and
    /// `/`, so a directory in this list has no trailing `/`.
    pub delete_except_unreadable_dirs: Option<Vec<String>>,
    /// The places that could not be read while walking the source.
    pub unreadable_places: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestItem {
    pub key: Key,
    pub time: Time,
    pub file_type: FileType,
}

/// Hands the contents of the items with a key to the shared ingest flow, one
/// at a time and in order. Unlike the component traits it need not be `Sync`,
/// because the flow calls it from one place at a time.
#[async_trait]
pub trait ItemContents: Send {
    /// The next item's content, or why it could not be read or confirmed to
    /// belong to the item. `None` after the last item.
    async fn next(&mut self) -> Option<Result<Vec<u8>, BoxError>>;
}
