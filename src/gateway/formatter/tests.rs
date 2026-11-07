use serde::Serialize;

use super::super::error_report::{Explanation, McpErrorCode};
use super::*;

#[derive(Serialize)]
struct Hit {
    key: String,
    score: f64,
}

fn hit(key: &str, score: f64) -> Hit {
    Hit {
        key: key.to_owned(),
        score,
    }
}

/// A value whose `Serialize` gives a safe integer the first time and an
/// integer outside the range every time after, so that a check that
/// serializes the value apart from writing it would let the second through.
struct ChangingNumber {
    calls: std::cell::Cell<u32>,
}

impl ChangingNumber {
    fn new() -> Self {
        Self {
            calls: std::cell::Cell::new(0),
        }
    }
}

impl Serialize for ChangingNumber {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let calls = self.calls.get();
        self.calls.set(calls + 1);
        if calls == 0 {
            serializer.serialize_u64(1)
        } else {
            serializer.serialize_u64(9_007_199_254_740_992)
        }
    }
}

fn explanation(message: &str, causes: &[&str], fix: &[&str]) -> Explanation {
    Explanation {
        message: message.to_owned(),
        causes: causes.iter().map(|&cause| cause.to_owned()).collect(),
        fix: fix.iter().map(|&line| line.to_owned()).collect(),
    }
}

// The command line's JSON.

#[test]
fn the_command_line_json_is_one_line_of_the_value() {
    assert_eq!(
        cli_json(&hit("a.md", 0.5)).unwrap(),
        r#"{"key":"a.md","score":0.5}"#
    );
}

#[test]
fn the_command_line_json_escapes_the_direction_characters_and_reads_as_the_same_value() {
    let json = cli_json(&hit("a\u{202e}b", 0.5)).unwrap();

    assert_eq!(
        json,
        concat!(r#"{"key":"a"#, "\\", r#"u202eb","score":0.5}"#)
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        serde_json::json!({ "key": "a\u{202e}b", "score": 0.5 })
    );
}

#[test]
fn the_command_line_json_escapes_controls_as_json_does() {
    let json = cli_json(&hit("a\nb\u{1b}c\u{7f}", 0.5)).unwrap();

    assert_eq!(json, "{\"key\":\"a\\nb\\u001bc\u{7f}\",\"score\":0.5}");
}

#[test]
fn the_command_line_json_of_an_unsafe_number_is_an_error() {
    assert_eq!(
        cli_json(&hit("a.md", f64::NAN)),
        Err(UnsafeNumber::NotFinite("NaN".to_owned()))
    );
}

#[test]
fn the_command_line_json_checks_the_numbers_as_it_writes_them() {
    let value = ChangingNumber::new();

    assert_eq!(cli_json(&value), Ok("1".to_owned()));
}

// What is passed to the MCP library.

#[test]
fn a_tool_result_checks_the_numbers_as_it_makes_the_value() {
    let value = ChangingNumber::new();

    assert_eq!(mcp_tool_result(&value), Ok(serde_json::json!(1)));
}

#[test]
fn a_tool_result_is_the_json_value_with_its_strings_unchanged() {
    let result = mcp_tool_result(&hit("a\u{202e}\u{1b}b", 0.5)).unwrap();

    assert_eq!(
        result,
        serde_json::json!({ "key": "a\u{202e}\u{1b}b", "score": 0.5 })
    );
}

#[test]
fn a_tool_result_with_an_unsafe_number_is_an_error() {
    assert_eq!(
        mcp_tool_result(&hit("a.md", f64::INFINITY)),
        Err(UnsafeNumber::NotFinite("inf".to_owned()))
    );
}

#[test]
fn a_tool_error_has_the_code_the_details_and_the_explanation_as_the_message() {
    let mut details = serde_json::Map::new();
    details.insert("collection".to_owned(), serde_json::json!("no\u{1b}tes"));
    let code = McpErrorCode {
        code: "collection_not_found",
        details: details.clone(),
    };

    let error = mcp_tool_error(
        code,
        &explanation(
            "the collection \"no\u{1b}tes\" does not exist",
            &["a\ncause"],
            &["Create it."],
        ),
    )
    .unwrap();

    assert_eq!(
        error,
        McpToolError {
            code: "collection_not_found",
            message: "Error: the collection \"no\u{1b}tes\" does not exist\n\
                      \n\
                      Caused by:\n    \
                      0: a\ncause\n\
                      \n\
                      How to fix:\n    \
                      Create it."
                .to_owned(),
            details,
        }
    );
}

#[test]
fn a_tool_error_with_an_unsafe_number_in_its_details_is_an_error() {
    let mut details = serde_json::Map::new();
    details.insert(
        "size_bytes".to_owned(),
        serde_json::json!(9_007_199_254_740_992_u64),
    );
    let code = McpErrorCode {
        code: "output_too_large",
        details,
    };

    assert!(mcp_tool_error(code, &explanation("too large", &[], &["Get less."])).is_err());
}

// The human-readable output of an explanation.

#[test]
fn an_explanation_without_causes_has_the_message_then_the_fix() {
    assert_eq!(
        human_explanation(
            &explanation(
                "another write operation is in progress",
                &[],
                &["Wait.", "Then run it again."]
            ),
            false
        ),
        "Error: another write operation is in progress\n\
         \n\
         How to fix:\n    \
         Wait.\n    \
         Then run it again."
    );
}

#[test]
fn an_explanation_with_causes_numbers_them_from_the_nearest() {
    assert_eq!(
        human_explanation(
            &explanation(
                "the index cannot be written",
                &["the nearest cause", "its cause"],
                &["Fix the cause."]
            ),
            false
        ),
        "Error: the index cannot be written\n\
         \n\
         Caused by:\n    \
         0: the nearest cause\n    \
         1: its cause\n\
         \n\
         How to fix:\n    \
         Fix the cause."
    );
}

#[test]
fn an_explanation_replaces_controls_and_direction_characters_in_all_of_its_text() {
    let laid_out = human_explanation(
        &explanation(
            "the collection \"no\ntes\" does not exist",
            &["disk\u{1b}full"],
            &["Check \u{202e}this."],
        ),
        false,
    );

    assert_eq!(
        laid_out,
        "Error: the collection \"no\\ntes\" does not exist\n\
         \n\
         Caused by:\n    \
         0: disk\\u{1b}full\n\
         \n\
         How to fix:\n    \
         Check \\u{202e}this."
    );
}

#[test]
fn colored_headings_are_red_yellow_and_green_and_the_rest_is_not_colored() {
    let laid_out = human_explanation(
        &explanation(
            "the index cannot be written",
            &["disk full"],
            &["Fix the cause."],
        ),
        true,
    );

    assert_eq!(
        laid_out,
        "\u{1b}[31mError:\u{1b}[0m the index cannot be written\n\
         \n\
         \u{1b}[33mCaused by:\u{1b}[0m\n    \
         0: disk full\n\
         \n\
         \u{1b}[32mHow to fix:\u{1b}[0m\n    \
         Fix the cause."
    );
}

#[test]
fn the_color_is_added_after_the_replacement_so_it_is_not_replaced() {
    let laid_out = human_explanation(&explanation("a\u{1b}b", &[], &["Fix."]), true);

    assert!(laid_out.starts_with("\u{1b}[31mError:\u{1b}[0m a\\u{1b}b\n"));
}

#[test]
fn the_help_keeps_its_line_breaks_and_replaces_the_other_controls() {
    assert_eq!(
        human_help("Usage: mdya\n\n  a\u{1b}b\u{202e}c\n"),
        format!("Usage: mdya\n\n  a{0}u{{1b}}b{0}u{{202e}}c\n", '\\')
    );
}

#[test]
fn the_version_is_one_line() {
    let version = VersionOutput {
        version: "mdya 1.2.3".to_owned(),
    };

    assert_eq!(human_version(&version), "mdya 1.2.3\n");
}

#[test]
fn the_text_of_a_document_is_kept_as_it_is() {
    let document = DocumentText {
        collection: "notes".to_owned(),
        key: "a.md".to_owned(),
        text: "a\u{1b}b\u{202e}c".to_owned(),
    };

    assert_eq!(human_document_text(&document), "a\u{1b}b\u{202e}c");
}
