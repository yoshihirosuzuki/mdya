//! Which file type a file name extension, a MIME type, or an alias stands for.

use super::Kind;

/// The whitespace allowed around the `;` before a MIME type's parameters.
const OPTIONAL_WHITESPACE: [char; 2] = [' ', '\t'];

/// The kind that a short name stands for, if any. File name extensions and
/// aliases are the same names, in any case.
pub(super) fn from_short_name(name: &str) -> Option<Kind> {
    if name.eq_ignore_ascii_case("md") || name.eq_ignore_ascii_case("markdown") {
        return Some(Kind::Markdown);
    }
    name.eq_ignore_ascii_case("pdf").then_some(Kind::Pdf)
}

/// The kind that a MIME type stands for, if any, in any case and with or
/// without parameters. `text/markdown` stands for Markdown only when each
/// `charset` it names is UTF-8, as a document in another charset is not one
/// mdya can read.
pub(super) fn from_mime_type(value: &str) -> Option<Kind> {
    let (essence, parameters) = split_parameters(value);
    if essence.eq_ignore_ascii_case("application/pdf") {
        return Some(Kind::Pdf);
    }
    let is_markdown = essence.eq_ignore_ascii_case("text/markdown") && charset_is_utf8(parameters);
    is_markdown.then_some(Kind::Markdown)
}

fn split_parameters(value: &str) -> (&str, &str) {
    match value.split_once(';') {
        Some((essence, parameters)) => (essence.trim_end_matches(OPTIONAL_WHITESPACE), parameters),
        None => (value, ""),
    }
}

fn charset_is_utf8(parameters: &str) -> bool {
    split_outside_quotes(parameters)
        .into_iter()
        .filter_map(charset_value)
        .all(|charset| charset.eq_ignore_ascii_case("UTF-8"))
}

/// Splits parameters at each `;` that is not inside a quoted string, where a
/// backslash makes the character after it an ordinary one.
fn split_outside_quotes(parameters: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut piece_start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in parameters.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                pieces.push(&parameters[piece_start..i]);
                piece_start = i + 1;
            }
            _ => {}
        }
    }
    pieces.push(&parameters[piece_start..]);
    pieces
}

/// The value of a parameter that is a `charset`, without the quotes and
/// backslashes of a quoted string.
fn charset_value(parameter: &str) -> Option<String> {
    let (name, value) = parameter.split_once('=')?;
    let is_charset = name
        .trim_matches(OPTIONAL_WHITESPACE)
        .eq_ignore_ascii_case("charset");
    is_charset.then(|| {
        value
            .trim_matches(OPTIONAL_WHITESPACE)
            .trim_matches('"')
            .replace('\\', "")
    })
}
