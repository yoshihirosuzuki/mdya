use super::*;

fn clue<'a>(message: &'a str, backtrace: Option<&'a str>) -> PanicClue<'a> {
    PanicClue {
        version: "mdya 1.2.3",
        location: Some("src/index.rs:120:9"),
        message,
        backtrace,
    }
}

#[test]
fn a_panic_is_written_with_the_version_the_location_the_message_and_the_backtrace() {
    let backtrace = "   0: mdya::index::inner\n   1: mdya::index::outer\n";

    let written = human_panic(&clue("index out of bounds", Some(backtrace)));

    assert_eq!(
        written,
        "mdya 1.2.3 panicked at src/index.rs:120:9:\n\
         index out of bounds\n\
         Backtrace:\n   0: mdya::index::inner\n   1: mdya::index::outer\n"
    );
}

#[test]
fn a_backtrace_that_could_not_be_captured_is_said_to_be_not_available() {
    let written = human_panic(&clue("failed", None));

    assert!(
        written.ends_with("failed\nBacktrace: not available\n"),
        "{written:?}"
    );
}

#[test]
fn a_panic_without_a_location_says_only_that_it_panicked() {
    let mut panic = clue("failed", None);
    panic.location = None;

    let written = human_panic(&panic);

    assert!(
        written.starts_with("mdya 1.2.3 panicked:\nfailed\n"),
        "{written:?}"
    );
}

#[test]
fn the_line_breaks_in_the_message_are_kept_and_each_line_has_its_characters_replaced() {
    let message = format!("first{}\nsecond\u{1b}", '\u{202e}');

    let written = human_panic(&clue(&message, None));

    assert!(
        written.contains("\nfirst\\u{202e}\nsecond\\u{1b}\nBacktrace"),
        "{written:?}"
    );
}

#[test]
fn the_backtrace_has_its_characters_replaced_too() {
    let written = human_panic(&clue("failed", Some("   0: f\u{7}\n")));

    assert!(
        written.ends_with("Backtrace:\n   0: f\\u{7}\n"),
        "{written:?}"
    );
}

#[test]
fn the_place_of_a_failure_says_what_mdya_was_doing() {
    assert_eq!(
        human_failure_place(FailurePlace::ReadingArguments),
        "It happened while reading the arguments.\n"
    );
    assert_eq!(
        human_failure_place(FailurePlace::Preparing("mdya update")),
        "It happened while preparing to run mdya update.\n"
    );
    assert_eq!(
        human_failure_place(FailurePlace::Running("mdya collection add")),
        "It happened while running mdya collection add.\n"
    );
    assert_eq!(
        human_failure_place(FailurePlace::McpTool("search")),
        "It happened in the MCP tool search.\n"
    );
}
