//! The `format` component: knows the file types mdya reads, and turns a
//! document's content into its body and chunks.

use std::sync::Arc;

use crate::types::{BoxError, Chunk};

#[cfg(test)]
pub(crate) mod test_double;

/// A file type. What it holds is up to this component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType(String);

impl FileType {
    /// A file type for a test double to return. `name` only tells values apart
    /// within a test.
    pub fn for_test_double(name: &str) -> Self {
        Self(format!("test-double:{name}"))
    }
}

/// Creates the component.
pub fn create() -> Arc<dyn Format> {
    todo!()
}

/// A document's body and chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extracted {
    pub body: String,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    /// The body cannot be extracted from the content. `source` tells why.
    #[error("the document's body cannot be extracted")]
    CannotExtract { source: BoxError },
}

pub trait Format: Send + Sync {
    /// The file type a file name extension stands for, if any.
    fn file_type_from_extension(&self, extension: &str) -> Option<FileType>;
    /// The file type a MIME type or alias stands for, if any.
    fn file_type_from_mime_or_alias(&self, value: &str) -> Option<FileType>;
    fn extract(&self, content: &[u8], file_type: &FileType) -> Result<Extracted, FormatError>;
}
