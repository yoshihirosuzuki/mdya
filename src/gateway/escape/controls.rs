//! Replaces control characters with visible ones in human-readable output.

use std::borrow::Cow;

/// Replaces each control character (Unicode general category Cc: U+0000 to
/// U+001F and U+007F to U+009F) with a visible form: `\n`, `\t`, and `\r`
/// for line feed, tab, and carriage return, and `\u{…}` with the code point
/// in lowercase hexadecimal for the others. A backslash stays as it is, so
/// that a Windows path reads naturally; a string that already holds `\n` as
/// two characters looks the same as a replaced line feed.
pub(super) fn replace_controls(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut replaced = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        push_visible(c, &mut replaced);
    }
    Cow::Owned(replaced)
}

/// `char::is_control` is the Cc category.
fn push_visible(c: char, out: &mut String) {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", u32::from(c))),
        c => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_without_control_characters_is_borrowed_unchanged() {
        let text = "notes/リリースの手順.md";

        let replaced = replace_controls(text);

        assert!(matches!(replaced, Cow::Borrowed(_)));
        assert_eq!(replaced, text);
    }

    #[test]
    fn an_empty_string_stays_empty() {
        assert_eq!(replace_controls(""), "");
    }

    #[test]
    fn line_feed_tab_and_carriage_return_get_their_short_forms() {
        assert_eq!(replace_controls("a\nb\tc\rd"), "a\\nb\\tc\\rd");
    }

    #[test]
    fn other_c0_controls_and_delete_get_hexadecimal_forms() {
        assert_eq!(
            replace_controls("\u{0}\u{7}\u{1b}[31m\u{1f}\u{7f}"),
            "\\u{0}\\u{7}\\u{1b}[31m\\u{1f}\\u{7f}"
        );
    }

    #[test]
    fn c1_controls_get_hexadecimal_forms() {
        assert_eq!(
            replace_controls("\u{80}\u{9b}31m\u{9f}"),
            "\\u{80}\\u{9b}31m\\u{9f}"
        );
    }

    #[test]
    fn characters_next_to_the_control_ranges_stay() {
        // U+0020 (space), U+007E (~), and U+00A0 (no-break space) are not Cc.
        assert_eq!(replace_controls(" ~\u{a0}"), " ~\u{a0}");
    }

    #[test]
    fn multibyte_characters_around_a_control_character_stay() {
        assert_eq!(replace_controls("日本\n語\u{9b}é"), "日本\\n語\\u{9b}é");
    }

    #[test]
    fn a_backslash_stays() {
        assert_eq!(replace_controls("C:\\Users\\me"), "C:\\Users\\me");
    }
}
