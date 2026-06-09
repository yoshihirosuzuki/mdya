//! File-format dispatch.
//!
//! Single source of truth for "which formats does mdya ingest?". The walker
//! (`ingest::walker`) filters files through [`FileFormat::from_path`]; the
//! writer (`ingest::writer::process_file`) dispatches extraction and chunking
//! through the same enum. Adding a new format = one variant + four trivial
//! `match` arms, all caught by `match` exhaustiveness at compile time.
//!
//! Why an enum instead of trait objects: chunking does not adopt a
//! preset framework, and registry / trait-factory patterns are
//! avoided here too. An enum gives the same dispatch with zero
//! runtime indirection and stronger exhaustiveness guarantees.

use std::path::Path;

use crate::chunking::{Chunk, ChunkingError, chunk_markdown, chunk_pdf};
use crate::extract::{ExtractError, extract_pdf};

/// One ingestable file format. Adding a variant forces every dispatcher
/// (`from_path`, `extract`, `chunk`) to handle it at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Markdown,
    Pdf,
}

impl FileFormat {
    /// Return `Some(format)` if `path`'s extension is one mdya ingests.
    /// `None` lets the walker drop the file silently. Matching is
    /// case-insensitive to handle `.MD` / `.PDF` on case-preserving file
    /// systems.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            return Some(Self::Markdown);
        }
        if ext.eq_ignore_ascii_case("pdf") {
            return Some(Self::Pdf);
        }
        None
    }

    /// Turn raw file bytes into UTF-8 plain text. Markdown decodes UTF-8
    /// strictly (matching the pre-existing `read_with_hash` contract); PDF
    /// delegates to `pdf-extract` via [`extract_pdf`].
    pub fn extract(&self, bytes: &[u8]) -> Result<String, ExtractError> {
        match self {
            Self::Markdown => Ok(String::from_utf8(bytes.to_vec())?),
            Self::Pdf => extract_pdf(bytes),
        }
    }

    /// Split extracted text into chunks. Markdown gets the heading-aware
    /// chunker; PDF gets the fixed-window slider, both sharing
    /// [`crate::chunking::WINDOW_CHARS`] / [`crate::chunking::OVERLAP_CHARS`].
    pub fn chunk(&self, text: &str) -> Result<Vec<Chunk>, ChunkingError> {
        match self {
            Self::Markdown => chunk_markdown(text),
            Self::Pdf => chunk_pdf(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_path_recognises_markdown_and_pdf_case_insensitively() {
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.md")),
            Some(FileFormat::Markdown)
        );
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.markdown")),
            Some(FileFormat::Markdown)
        );
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.MD")),
            Some(FileFormat::Markdown)
        );
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.MARKDOWN")),
            Some(FileFormat::Markdown)
        );
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.pdf")),
            Some(FileFormat::Pdf)
        );
        assert_eq!(
            FileFormat::from_path(&PathBuf::from("a.PDF")),
            Some(FileFormat::Pdf)
        );
    }

    #[test]
    fn from_path_rejects_other_extensions_and_no_extension() {
        assert_eq!(FileFormat::from_path(&PathBuf::from("a.txt")), None);
        assert_eq!(FileFormat::from_path(&PathBuf::from("a.rs")), None);
        assert_eq!(FileFormat::from_path(&PathBuf::from("a.epub")), None);
        assert_eq!(FileFormat::from_path(&PathBuf::from("README")), None);
    }

    #[test]
    fn extract_markdown_accepts_valid_utf8() {
        let text = FileFormat::Markdown
            .extract("# Hello".as_bytes())
            .expect("ok");
        assert_eq!(text, "# Hello");
    }

    #[test]
    fn extract_markdown_rejects_invalid_utf8() {
        let err = FileFormat::Markdown
            .extract(&[0xff, 0xfe, 0xfd])
            .expect_err("invalid utf-8 should error");
        assert!(matches!(err, ExtractError::InvalidUtf8(_)));
    }

    #[test]
    fn extract_pdf_on_non_pdf_bytes_returns_pdf_error() {
        let err = FileFormat::Pdf
            .extract(b"not a pdf")
            .expect_err("non-PDF bytes should fail");
        assert!(matches!(err, ExtractError::Pdf(_)));
    }

    #[test]
    fn chunk_markdown_dispatch_yields_heading_aware_chunks() {
        let chunks = FileFormat::Markdown
            .chunk("# Heading\n\nBody\n")
            .expect("ok");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].body.contains("Heading"));
        assert!(chunks[0].body.contains("Body"));
    }

    #[test]
    fn chunk_pdf_dispatch_yields_sliding_window_chunks() {
        let chunks = FileFormat::Pdf.chunk("hello world").expect("ok");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].body, "hello world");
    }

    #[test]
    fn chunk_pdf_dispatch_yields_placeholder_for_empty_text() {
        let chunks = FileFormat::Pdf.chunk("").expect("ok");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].body.is_empty());
    }
}
