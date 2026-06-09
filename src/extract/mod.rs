//! Format-specific text extraction.
//!
//! Each submodule turns the raw bytes of one file format into UTF-8 plain text
//! suitable for the chunking pipeline. Errors are wrapped in [`ExtractError`]
//! so the ingest writer does not depend on extractor-internal error types
//! (`pdf_extract::OutputError`, `std::string::FromUtf8Error`).

use thiserror::Error;

pub mod pdf;

pub use pdf::extract_pdf;

/// Failures returned by any [`crate::format::FileFormat`] extractor.
///
/// `InvalidUtf8` covers the Markdown path (raw file bytes must be UTF-8);
/// `Pdf` covers any `pdf-extract` failure, stringified so the variant does
/// not leak the upstream `OutputError` type into mdya's public surface.
#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("non-UTF-8 file content: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("pdf extract failed: {0}")]
    Pdf(String),
}
