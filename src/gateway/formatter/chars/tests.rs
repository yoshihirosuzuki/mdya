use super::*;

// Replacing with visible forms, for the human-readable output.

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

// Escaping in JSON, for the command line's JSON.

#[test]
fn json_without_the_direction_characters_is_kept() {
    let json = r#"{"key":"a.md","text":"日本\n\u007f\u0080"}"#.to_owned();

    assert_eq!(escape_direction_characters(json.clone()), json);
}

#[test]
fn the_nine_direction_characters_are_written_as_json_escapes() {
    let json =
        "\"a\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}b\"".to_owned();

    assert_eq!(
        escape_direction_characters(json),
        r#""a\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069b""#
    );
}

#[test]
fn the_escaped_json_reads_as_the_same_value() {
    let value = serde_json::json!({ "key": "a\u{202e}b", "nested": ["c\u{2066}d"] });
    let json = serde_json::to_string(&value).unwrap();

    let escaped = escape_direction_characters(json);

    assert!(!escaped.contains('\u{202e}'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&escaped).unwrap(),
        value
    );
}

#[test]
fn delete_and_the_c1_controls_are_not_escaped_in_json() {
    let json = "\"\u{7f}\u{80}\u{9f}\"".to_owned();

    assert_eq!(escape_direction_characters(json.clone()), json);
}
