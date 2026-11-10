use std::error::Error;

use super::*;

fn markdown() -> Option<FileType> {
    Some(FileType(Kind::Markdown))
}

fn pdf() -> Option<FileType> {
    Some(FileType(Kind::Pdf))
}

#[test]
fn the_extensions_of_markdown_name_markdown_in_any_case() {
    for extension in ["md", "markdown", "MD", "Md", "MarkDown"] {
        assert_eq!(
            create().file_type_from_extension(extension),
            markdown(),
            "{extension}"
        );
    }
}

#[test]
fn the_extension_of_pdf_names_pdf_in_any_case() {
    for extension in ["pdf", "PDF", "Pdf"] {
        assert_eq!(
            create().file_type_from_extension(extension),
            pdf(),
            "{extension}"
        );
    }
}

#[test]
fn other_extensions_name_no_file_type() {
    for extension in ["", "txt", "mdx", "mkd", ".md", "md ", "pd", "text/markdown"] {
        assert_eq!(
            create().file_type_from_extension(extension),
            None,
            "{extension:?}"
        );
    }
}

#[test]
fn the_mime_type_and_aliases_of_markdown_name_markdown_in_any_case() {
    for value in [
        "text/markdown",
        "TEXT/Markdown",
        "markdown",
        "MARKDOWN",
        "md",
        "Md",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            markdown(),
            "{value}"
        );
    }
}

#[test]
fn the_mime_type_and_alias_of_pdf_name_pdf_in_any_case() {
    for value in ["application/pdf", "APPLICATION/PDF", "pdf", "PDF"] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            pdf(),
            "{value}"
        );
    }
}

#[test]
fn markdown_with_the_utf8_charset_or_other_parameters_names_markdown() {
    for value in [
        "text/markdown; charset=UTF-8",
        "text/markdown;charset=utf-8",
        "TEXT/MARKDOWN; CHARSET=Utf-8",
        "text/markdown ; charset=\"UTF-8\"",
        "text/markdown; variant=GFM",
        "text/markdown; variant=GFM; charset=UTF-8",
        "text/markdown; flag",
        "text/markdown; variant=\"a;charset=other\"; charset=UTF-8",
        "text/markdown; variant=\"a\\\";charset=other\"; charset=UTF-8",
        "text/markdown; charset=\"UTF\\-8\"",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            markdown(),
            "{value}"
        );
    }
}

#[test]
fn markdown_with_another_charset_names_no_file_type() {
    for value in [
        "text/markdown; charset=ISO-8859-1",
        "text/markdown; charset=UTF-16",
        "text/markdown; charset=UTF8",
        "text/markdown; variant=GFM; charset=Shift_JIS",
        "text/markdown; charset=UTF-8; charset=Shift_JIS",
        "text/markdown; variant=\"a;b\"; charset=Shift_JIS",
        "text/markdown; variant=a\\;charset=Shift_JIS",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            None,
            "{value}"
        );
    }
}

#[test]
fn pdf_with_parameters_names_pdf_whatever_they_are() {
    for value in [
        "application/pdf; version=1.7",
        "application/pdf; charset=UTF-16",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            pdf(),
            "{value}"
        );
    }
}

#[test]
fn an_alias_takes_no_parameters() {
    for value in [
        "markdown; charset=UTF-8",
        "md; variant=GFM",
        "pdf; version=1.7",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            None,
            "{value}"
        );
    }
}

#[test]
fn other_values_name_no_file_type() {
    for value in [
        "",
        ";",
        "text/plain",
        "text/html; charset=UTF-8",
        "application/json",
        "text/markdown/extra",
        "text/markdown ",
        " text/markdown",
        "txt",
    ] {
        assert_eq!(
            create().file_type_from_mime_or_alias(value),
            None,
            "{value:?}"
        );
    }
}

#[test]
fn a_file_type_for_a_test_double_is_none_of_the_real_ones() {
    let double = Some(FileType::for_test_double("markdown"));

    assert_ne!(double, markdown());
    assert_ne!(double, pdf());
}

#[test]
#[should_panic(expected = "test double")]
fn extracting_with_a_file_type_for_a_test_double_is_a_bug() {
    let _ = create().extract(b"text", &FileType::for_test_double("markdown"));
}

#[test]
fn the_body_of_markdown_is_the_content_unchanged() {
    let content = "\u{feff}---\ntitle: t\r\n---\n\n# Title\r\n\r\n  *text*  \n\n";

    let extracted = create()
        .extract(content.as_bytes(), &markdown().unwrap())
        .unwrap();

    assert_eq!(extracted.body, content);
}

#[test]
fn markdown_that_is_not_utf8_cannot_be_extracted_and_tells_why() {
    let error = create()
        .extract(b"text \xFF\xFE", &markdown().unwrap())
        .unwrap_err();

    assert!(matches!(error, FormatError::CannotExtract { .. }));
    assert!(error.source().unwrap().to_string().contains("utf-8"));
}

#[test]
fn an_empty_markdown_document_has_an_empty_body_and_no_chunks() {
    let extracted = create().extract(b"", &markdown().unwrap()).unwrap();

    assert_eq!(
        extracted,
        Extracted {
            body: String::new(),
            chunks: Vec::new()
        }
    );
}

#[test]
fn content_that_is_not_a_pdf_cannot_be_extracted_as_a_pdf() {
    let error = create()
        .extract(b"not a pdf at all", &pdf().unwrap())
        .unwrap_err();

    assert!(matches!(error, FormatError::CannotExtract { .. }));
}

#[test]
fn a_markdown_document_gets_chunks_over_its_body() {
    let body = "# Title\n\nText.\n";

    let extracted = create()
        .extract(body.as_bytes(), &markdown().unwrap())
        .unwrap();

    assert_eq!(
        extracted.chunks,
        vec![Chunk {
            range: 0..body.len()
        }]
    );
}

#[test]
fn the_component_can_be_called_from_several_threads_at_once() {
    let format = create();

    let bodies: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let format = &format;
                scope.spawn(move || {
                    let content = format!("# Title {i}\n");
                    format
                        .extract(content.as_bytes(), &markdown().unwrap())
                        .unwrap()
                        .body
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for (i, body) in bodies.iter().enumerate() {
        assert_eq!(body, &format!("# Title {i}\n"));
    }
}
