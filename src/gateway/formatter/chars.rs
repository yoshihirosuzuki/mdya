//! Which characters each output format writes in another form: in the
//! human-readable output and in the logs not written as JSON, the control
//! characters and the characters that change the direction of text are
//! replaced with visible forms; in the command line's JSON and in the logs
//! written as JSON, the characters that change the direction of text are
//! escaped, in addition to the escapes JSON itself requires.

use std::borrow::Cow;

#[cfg(test)]
mod tests;

/// Replaces each control character (Unicode general category Cc: U+0000 to
/// U+001F and U+007F to U+009F) and each character that changes the
/// direction of text (U+202A to U+202E and U+2066 to U+2069) with a visible
/// form: `\n`, `\t`, and `\r` for line feed, tab, and carriage return, and a
/// backslash, `u`, and the code point in lowercase hexadecimal in braces for
/// the others. A backslash stays as it is, so that a Windows path reads
/// naturally; a string that already holds `\n` as two characters looks the
/// same as a replaced line feed.
pub(crate) fn replace_controls(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_replaced) {
        return Cow::Borrowed(text);
    }
    let mut replaced = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        push_visible(c, &mut replaced);
    }
    Cow::Owned(replaced)
}

/// Writes the characters that change the direction of text in `json`, a JSON
/// text, as JSON escapes: a backslash, `u`, and the four hexadecimal digits
/// of the code point. They can appear only inside strings, as everything else
/// in JSON is ASCII (RFC 8259, section 2), so the JSON reads as the same
/// value.
pub(crate) fn escape_direction_characters(json: &str) -> Cow<'_, str> {
    if !json.chars().any(changes_direction) {
        return Cow::Borrowed(json);
    }
    let mut escaped = String::with_capacity(json.len() + 8);
    for c in json.chars() {
        if changes_direction(c) {
            escaped.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            escaped.push(c);
        }
    }
    Cow::Owned(escaped)
}

/// Whether `c` is replaced. `char::is_control` is the Cc category.
fn is_replaced(c: char) -> bool {
    c.is_control() || changes_direction(c)
}

/// The nine characters that rustc's `text_direction_codepoint_in_literal`
/// lint, denied by default, rejects in literals: they make the text around
/// them show in another direction than the order it is stored in.
fn changes_direction(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn push_visible(c: char, out: &mut String) {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        c if is_replaced(c) => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
        c => out.push(c),
    }
}
