//! The `format` component: knows the file types mdya reads, and turns a
//! document's content into its body and chunks.

mod markdown;
mod names;
mod window;

use std::sync::Arc;

use crate::types::{BoxError, Chunk};

#[cfg(test)]
pub(crate) mod test_double;
#[cfg(test)]
mod tests;

/// A file type. What it holds is up to this component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType(Kind);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Markdown,
    Pdf,
    TestDouble(String),
}

impl FileType {
    /// A file type for a test double to return. `name` only tells values apart
    /// within a test.
    pub fn for_test_double(name: &str) -> Self {
        Self(Kind::TestDouble(name.to_owned()))
    }
}

/// Creates the component.
pub fn create() -> Arc<dyn Format> {
    Arc::new(Formats)
}

/// The component. It keeps nothing between calls.
struct Formats;

impl Format for Formats {
    fn file_type_from_extension(&self, extension: &str) -> Option<FileType> {
        names::from_short_name(extension).map(FileType)
    }

    fn file_type_from_mime_or_alias(&self, value: &str) -> Option<FileType> {
        names::from_short_name(value)
            .or_else(|| names::from_mime_type(value))
            .map(FileType)
    }

    fn extract(&self, content: &[u8], file_type: &FileType) -> Result<Extracted, FormatError> {
        match &file_type.0 {
            Kind::Markdown => extract_markdown(content),
            Kind::Pdf => todo!(),
            #[expect(clippy::panic)]
            Kind::TestDouble(name) => {
                panic!(
                    "the file type {name} of a test double reached the real component: only a test double returns one"
                )
            }
        }
    }
}

/// The body of Markdown is its content as UTF-8, unchanged.
fn extract_markdown(content: &[u8]) -> Result<Extracted, FormatError> {
    let body = std::str::from_utf8(content)
        .map_err(|source| FormatError::CannotExtract {
            source: source.into(),
        })?
        .to_owned();
    let chunks = markdown::chunks(&body);
    Ok(Extracted { body, chunks })
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
