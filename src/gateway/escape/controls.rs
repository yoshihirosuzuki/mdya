//! Replaces control characters and the characters that change the direction
//! of text with visible ones in human-readable output.

use std::borrow::Cow;

/// Replaces each control character (Unicode general category Cc: U+0000 to
/// U+001F and U+007F to U+009F) and each character that changes the
/// direction of text (U+202A to U+202E and U+2066 to U+2069) with a visible
/// form: `\n`, `\t`, and `\r` for line feed, tab, and carriage return, and
/// `\u{…}` with the code point in lowercase hexadecimal for the others. A
/// backslash stays as it is, so that a Windows path reads naturally; a string
/// that already holds `\n` as two characters looks the same as a replaced
/// line feed.
pub(super) fn replace_controls(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_replaced) {
        return Cow::Borrowed(text);
    }
    let mut replaced = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        push_visible(c, &mut replaced);
    }
    Cow::Owned(replaced)
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
    fn characters_that_change_the_direction_of_text_get_hexadecimal_forms() {
        assert_eq!(
            replace_controls(
                "a\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}b"
            ),
            "a\\u{202a}\\u{202b}\\u{202c}\\u{202d}\\u{202e}\\u{2066}\\u{2067}\\u{2068}\\u{2069}b"
        );
    }

    #[test]
    fn other_characters_near_the_direction_characters_stay() {
        // U+061C, U+200E, and U+200F also have the Unicode property
        // Bidi_Control; U+2029, U+202F, U+2065, and U+206A are next to the
        // two ranges.
        let text = "\u{61c}\u{200e}\u{200f}\u{2029}\u{202f}\u{2065}\u{206a}";

        let replaced = replace_controls(text);

        assert!(matches!(replaced, Cow::Borrowed(_)));
        assert_eq!(replaced, text);
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
