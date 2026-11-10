//! Reads the text of a PDF document and cuts it into chunks at a fixed width.

use unpdf::{Document, ErrorMode, ParseOptions};

use super::{Extracted, FormatError, window};
use crate::types::{BoxError, Chunk};

/// The body of a PDF is the text that `unpdf` finds in it. A damaged PDF is
/// an unreadable document, not a shorter one: left to its default, `unpdf`
/// returns what it could read, which would replace the document's earlier
/// version with part of it.
pub(super) fn extract(content: &[u8]) -> Result<Extracted, FormatError> {
    let options = ParseOptions::new().with_error_mode(ErrorMode::Strict);
    let document = unpdf::parse_bytes_with_options(content, options).map_err(cannot_extract)?;
    if document.extraction_quality.pages_incomplete {
        return Err(cannot_extract("pages of the document are missing"));
    }
    let body = body_of(&document);
    let chunks = chunks(&body);
    Ok(Extracted { body, chunks })
}

fn cannot_extract(source: impl Into<BoxError>) -> FormatError {
    FormatError::CannotExtract {
        source: source.into(),
    }
}

/// `unpdf` puts blank lines between pages, so a PDF with no text would
/// otherwise have a body of only those.
fn body_of(document: &Document) -> String {
    let text = document.plain_text();
    if text.trim().is_empty() {
        return String::new();
    }
    text
}

fn chunks(body: &str) -> Vec<Chunk> {
    if body.is_empty() {
        return Vec::new();
    }
    window::cut(body)
        .into_iter()
        .map(|range| Chunk { range })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::Path;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pdfs")
                .join(name),
        )
        .unwrap()
    }

    /// The content stream of a page, and the filter its dictionary names.
    #[derive(Clone, Copy)]
    struct PageContent<'a> {
        filter: &'a str,
        data: &'a str,
    }

    const EMPTY_PAGE: PageContent<'static> = PageContent {
        filter: "",
        data: "",
    };

    fn pdf_of_one_page(content: &str) -> Vec<u8> {
        pdf_of_pages(&[PageContent {
            filter: "",
            data: content,
        }])
    }

    /// A PDF with a page for each of `pages`, set in Helvetica.
    fn pdf_of_pages(pages: &[PageContent<'_>]) -> Vec<u8> {
        let kids: Vec<String> = (0..pages.len())
            .map(|i| format!("{} 0 R", 4 + 2 * i))
            .collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        for (i, page) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents {} 0 R \
                 /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} {} >>\nstream\n{}\nendstream",
                page.data.len(),
                page.filter,
                page.data
            ));
        }
        let mut pdf = String::from("%PDF-1.5\n");
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref_at = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        pdf.push_str("0000000000 65535 f \n");
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }

    /// The content of a page with `lines` lines of `line`.
    fn lines_of_text(line: &str, lines: usize) -> String {
        let shown = format!("({line}) Tj 0 -13 Td ").repeat(lines);
        format!("BT /F1 10 Tf 40 810 Td {shown}ET")
    }

    fn assert_covers(body: &str, chunks: &[Chunk]) {
        assert_eq!(chunks.first().unwrap().range.start, 0, "{chunks:?}");
        assert_eq!(chunks.last().unwrap().range.end, body.len(), "{chunks:?}");
        for pair in chunks.windows(2) {
            assert!(pair[1].range.start <= pair[0].range.end, "{chunks:?}");
            assert!(pair[0].range.start < pair[1].range.start, "{chunks:?}");
        }
    }

    #[test]
    fn the_body_of_a_pdf_is_the_text_in_it() {
        let extracted = extract(&fixture("english.pdf")).unwrap();

        assert!(
            extracted.body.contains("mdya PDF ingest smoke fixture"),
            "{:?}",
            extracted.body
        );
    }

    #[test]
    fn japanese_text_is_read() {
        let extracted = extract(&fixture("japanese.pdf")).unwrap();

        assert!(
            extracted
                .body
                .contains("取り込み日本語スモーク用フィクスチャ"),
            "{:?}",
            extracted.body
        );
    }

    #[test]
    fn a_short_pdf_is_one_chunk_over_its_body() {
        let extracted = extract(&fixture("english.pdf")).unwrap();

        assert_eq!(
            extracted.chunks,
            vec![Chunk {
                range: 0..extracted.body.len()
            }]
        );
    }

    #[test]
    fn a_long_pdf_is_cut_at_the_width_with_ranges_that_cover_its_body() {
        let line = "All work and no play makes a dull document indeed";
        let pdf = pdf_of_one_page(&lines_of_text(line, 40));

        let extracted = extract(&pdf).unwrap();

        assert!(extracted.chunks.len() > 1, "{:?}", extracted.chunks);
        assert_covers(&extracted.body, &extracted.chunks);
        for chunk in &extracted.chunks {
            let chars = extracted.body[chunk.range.clone()].chars().count();
            assert!(chars <= 700, "{chars}");
        }
    }

    #[test]
    fn a_pdf_with_no_text_has_an_empty_body_and_no_chunks() {
        let extracted = extract(&pdf_of_one_page("")).unwrap();

        assert_eq!(extracted.body, "");
        assert_eq!(extracted.chunks, Vec::new());
    }

    #[test]
    fn a_pdf_of_only_spaces_has_an_empty_body_and_no_chunks() {
        let extracted = extract(&pdf_of_one_page("BT /F1 12 Tf (   ) Tj ET")).unwrap();

        assert_eq!(extracted.body, "");
        assert_eq!(extracted.chunks, Vec::new());
    }

    #[test]
    fn a_pdf_of_several_empty_pages_has_an_empty_body_and_no_chunks() {
        let extracted = extract(&pdf_of_pages(&[EMPTY_PAGE, EMPTY_PAGE, EMPTY_PAGE])).unwrap();

        assert_eq!(extracted.body, "");
        assert_eq!(extracted.chunks, Vec::new());
    }

    #[test]
    fn a_page_with_text_is_read_when_another_page_is_empty() {
        let text = PageContent {
            filter: "",
            data: "BT /F1 12 Tf 40 800 Td (Hello) Tj ET",
        };

        let extracted = extract(&pdf_of_pages(&[EMPTY_PAGE, text])).unwrap();

        assert!(extracted.body.contains("Hello"), "{:?}", extracted.body);
    }

    #[test]
    fn a_page_whose_content_cannot_be_decoded_makes_the_pdf_unreadable() {
        let damaged = PageContent {
            filter: "/Filter /FlateDecode",
            data: "this is not deflate data",
        };

        let error = extract(&pdf_of_pages(&[damaged])).unwrap_err();

        assert!(matches!(error, FormatError::CannotExtract { .. }));
    }

    #[test]
    fn a_pdf_with_pages_missing_cannot_be_extracted_and_tells_why() {
        let whole = String::from_utf8(pdf_of_one_page("")).unwrap();
        // "1" and "2" are one byte each, so the offsets in the file stay right.
        let declares_a_page_too_many = whole.replace("/Count 1", "/Count 2");

        let error = extract(declares_a_page_too_many.as_bytes()).unwrap_err();

        assert!(matches!(error, FormatError::CannotExtract { .. }));
        assert!(error.source().unwrap().to_string().contains("missing"));
    }

    #[test]
    fn content_that_is_not_a_pdf_cannot_be_extracted_and_tells_why() {
        for content in [b"not a pdf at all".as_slice(), b"".as_slice()] {
            let error = extract(content).unwrap_err();

            assert!(matches!(error, FormatError::CannotExtract { .. }));
            assert!(error.source().is_some());
        }
    }

    #[test]
    fn a_pdf_cut_short_cannot_be_extracted() {
        let whole = fixture("english.pdf");

        let error = extract(&whole[..whole.len() / 2]).unwrap_err();

        assert!(matches!(error, FormatError::CannotExtract { .. }));
    }
}
