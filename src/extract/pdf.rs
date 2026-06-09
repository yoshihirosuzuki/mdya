//! PDF text extraction via `pdf-extract`.
//!
//! `pdf-extract` is pure Rust (internally lopdf + adobe-cmap-parser plus
//! RustCrypto for encrypted-PDF read paths); the dependency comment in
//! `Cargo.toml` records the `cargo tree` audit. This module hides the
//! upstream `pdf_extract::OutputError` behind [`super::ExtractError::Pdf`]
//! so callers depend only on mdya's own error type.

use super::ExtractError;

/// Extract plain text from in-memory PDF bytes.
///
/// **May** return `Ok("")` for PDFs whose pages contain no extractable
/// text (e.g. completely blank pages with no glyphs) — the caller's
/// chunker turns that into a placeholder chunk via
/// `chunking::placeholder_chunk`. The exact behaviour for scan-only
/// image PDFs depends on `pdf-extract` internals (it may return
/// `Ok("")`, partial text, or `Err` depending on whether the PDF
/// declares any glyph runs at all) and is not asserted by the current
/// test fixtures — image-only PDFs / OCR are out of scope.
/// Returns [`ExtractError::Pdf`] for malformed / unsupported PDFs; the
/// ingest writer logs and skips these files rather than aborting the
/// run.
pub fn extract_pdf(bytes: &[u8]) -> Result<String, ExtractError> {
    pdf_extract::extract_text_from_mem(bytes).map_err(|e| ExtractError::Pdf(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_return_pdf_error() {
        // Not a valid PDF; pdf-extract returns Err so we wrap into ExtractError::Pdf.
        let err = extract_pdf(b"").expect_err("empty bytes should fail");
        assert!(
            matches!(err, ExtractError::Pdf(_)),
            "expected ExtractError::Pdf, got {err:?}"
        );
    }

    #[test]
    fn non_pdf_bytes_return_pdf_error() {
        let err = extract_pdf(b"not a pdf at all").expect_err("non-PDF bytes should fail");
        assert!(matches!(err, ExtractError::Pdf(_)));
    }
}
