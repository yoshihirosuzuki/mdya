//! A test double for [`Format`], for the unit tests of the components that use
//! it.

// Parts of the double are for tests that the components using it have not
// written yet.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use super::{Extracted, FileType, Format, FormatError};
use crate::types::Chunk;

type ExtractFn = dyn Fn(&[u8], &FileType) -> Result<Extracted, FormatError> + Send + Sync;

/// Knows only the file types it is given. By default, `extract` reads the
/// content as UTF-8 (replacing invalid bytes) into a body with one chunk that
/// covers all of it. Records every call to `extract`.
pub(crate) struct TestFormat {
    extensions: HashMap<String, FileType>,
    media_types: HashMap<String, FileType>,
    extract: Box<ExtractFn>,
    extracted: Mutex<Vec<(Vec<u8>, FileType)>>,
}

impl TestFormat {
    pub(crate) fn new() -> Self {
        Self {
            extensions: HashMap::new(),
            media_types: HashMap::new(),
            extract: Box::new(|content, _| Ok(whole_text(content))),
            extracted: Mutex::new(Vec::new()),
        }
    }

    /// Maps a file name extension to the file type made by
    /// `FileType::for_test_double(type_name)`.
    pub(crate) fn with_extension(mut self, extension: &str, type_name: &str) -> Self {
        self.extensions
            .insert(extension.to_owned(), FileType::for_test_double(type_name));
        self
    }

    /// Maps a MIME type or alias to the file type made by
    /// `FileType::for_test_double(type_name)`.
    pub(crate) fn with_media_type(mut self, value: &str, type_name: &str) -> Self {
        self.media_types
            .insert(value.to_owned(), FileType::for_test_double(type_name));
        self
    }

    /// Replaces what `extract` does.
    pub(crate) fn on_extract(
        mut self,
        extract: impl Fn(&[u8], &FileType) -> Result<Extracted, FormatError> + Send + Sync + 'static,
    ) -> Self {
        self.extract = Box::new(extract);
        self
    }

    /// The content and file type of every `extract` call, in order.
    pub(crate) fn extracted(&self) -> Vec<(Vec<u8>, FileType)> {
        self.extracted.lock().unwrap().clone()
    }
}

/// The default `extract`: the content as the body, in one chunk.
pub(crate) fn whole_text(content: &[u8]) -> Extracted {
    let body = String::from_utf8_lossy(content).into_owned();
    let chunks = vec![Chunk {
        range: 0..body.len(),
    }];
    Extracted { body, chunks }
}

impl Format for TestFormat {
    fn file_type_from_extension(&self, extension: &str) -> Option<FileType> {
        self.extensions.get(extension).cloned()
    }

    fn file_type_from_mime_or_alias(&self, value: &str) -> Option<FileType> {
        self.media_types.get(value).cloned()
    }

    fn extract(&self, content: &[u8], file_type: &FileType) -> Result<Extracted, FormatError> {
        self.extracted
            .lock()
            .unwrap()
            .push((content.to_vec(), file_type.clone()));
        (self.extract)(content, file_type)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn knows_only_the_file_types_it_is_given() {
        let format: Arc<dyn Format> = Arc::new(
            TestFormat::new()
                .with_extension("md", "markdown")
                .with_media_type("text/markdown", "markdown"),
        );

        assert_eq!(
            format.file_type_from_extension("md"),
            Some(FileType::for_test_double("markdown"))
        );
        assert_eq!(
            format.file_type_from_mime_or_alias("text/markdown"),
            Some(FileType::for_test_double("markdown"))
        );
        assert_eq!(format.file_type_from_extension("pdf"), None);
    }

    #[test]
    fn extracts_the_content_as_one_chunk_and_records_the_call() {
        let double = Arc::new(TestFormat::new());
        let format: Arc<dyn Format> = double.clone();
        let file_type = FileType::for_test_double("markdown");

        let extracted = format.extract("本文".as_bytes(), &file_type).unwrap();

        assert_eq!(extracted.body, "本文");
        assert_eq!(
            extracted.chunks,
            vec![Chunk {
                range: 0.."本文".len()
            }]
        );
        assert_eq!(
            double.extracted(),
            vec![("本文".as_bytes().to_vec(), file_type)]
        );
    }

    #[test]
    fn extract_can_be_replaced() {
        let format: Arc<dyn Format> = Arc::new(TestFormat::new().on_extract(|_, _| {
            Err(FormatError::CannotExtract {
                source: "broken".into(),
            })
        }));

        let result = format.extract(b"x", &FileType::for_test_double("pdf"));

        assert!(matches!(result, Err(FormatError::CannotExtract { .. })));
    }
}
