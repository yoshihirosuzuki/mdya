//! What the command line does for every operation, with the real components:
//! the version and the help, where results, messages, and errors are written,
//! the characters replaced in what is written for people, colors, the lines
//! of progress, the directories used when none are given, and an address that
//! the MCP server cannot listen on.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::process::ExitCode;
#[cfg(unix)]
use std::process::Output;

use serde_json::Value;

#[cfg(unix)]
use crate::common::executable_at_home;
use crate::common::{Outcome, Workspace};

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// Asserts that `outcome` failed with exit code `code`, wrote nothing to
/// stdout, and wrote `message` to stderr.
fn assert_failed(outcome: &Outcome, code: u8, message: &str) {
    assert_eq!(outcome.code, ExitCode::from(code), "{outcome:?}");
    assert_eq!(outcome.stdout, "", "{outcome:?}");
    assert!(outcome.stderr.contains(message), "{outcome:?}");
}

#[cfg(unix)]
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The names in `dir`, sorted.
#[cfg(unix)]
fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_version_is_written_on_stdout_as_mdya_and_its_version() {
    let workspace = Workspace::new();

    for args in [&["--version"][..], &["-v"]] {
        let version = workspace.run(args);

        assert_success(&version);
        assert_eq!(
            version.stdout,
            format!("mdya {}\n", env!("CARGO_PKG_VERSION")),
            "{args:?}"
        );
        assert_eq!(version.stderr, "", "{args:?}");
    }
    assert!(!workspace.data_dir().try_exists().unwrap());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_help_of_a_command_is_written_on_stdout_whatever_the_other_arguments() {
    let workspace = Workspace::new();
    let helps: [(&[&str], &str); 7] = [
        (&["--help"], "Usage: mdya [OPTIONS] <COMMAND>"),
        (&["-h"], "Usage: mdya [OPTIONS] <COMMAND>"),
        (&["--json", "--help"], "Usage: mdya [OPTIONS] <COMMAND>"),
        (
            &["collection", "--help"],
            "Usage: mdya collection <COMMAND>",
        ),
        (
            &["collection", "add", "-h"],
            "Usage: mdya collection add [OPTIONS] <NAME> [LOCATION]",
        ),
        // Without the query.
        (
            &["search", "--help"],
            "Usage: mdya search [OPTIONS] <QUERY>",
        ),
        // With a limit of 0.
        (
            &["search", "--help", "--limit", "0"],
            "Usage: mdya search [OPTIONS] <QUERY>",
        ),
    ];

    for (args, usage) in helps {
        let helped = workspace.run(args);
        assert_success(&helped);
        assert!(helped.stdout.contains(usage), "{helped:?}");
        assert_eq!(helped.stderr, "", "{helped:?}");
    }
    let commands = workspace.run(&["--help"]).stdout;
    assert!(commands.contains("Commands:\n  collection "), "{commands}");
    let collection_commands = workspace.run(&["collection", "--help"]).stdout;
    assert!(
        collection_commands.contains("Commands:\n  add "),
        "{collection_commands}"
    );
    let arguments = workspace.run(&["collection", "add", "--help"]).stdout;
    assert!(arguments.contains("Arguments:\n  <NAME> "), "{arguments}");
    assert!(!workspace.data_dir().try_exists().unwrap());
}

#[test]
#[cfg(unix)]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn mdya_without_arguments_writes_the_help_of_mdya_on_stdout() {
    let workspace = Workspace::new();
    let home = workspace.dir("home");

    // Without the directories that `Workspace` gives, as a user types it.
    let bare = executable_at_home(&home).output().unwrap();

    assert_eq!(bare.status.code(), Some(0), "{}", stderr(&bare));
    assert_eq!(stdout(&bare), workspace.run(&["--help"]).stdout);
    assert_eq!(stderr(&bare), "");
    assert!(names_in(&home).is_empty(), "{:?}", names_in(&home));
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn options_without_a_command_or_a_command_that_does_not_exist_fail_with_exit_code_2_and_point_to_the_help()
 {
    let workspace = Workspace::new();
    // `run` gives the directories, so the first is those options alone.
    let refused: [&[&str]; 3] = [&[], &["collection"], &["nosuchcmd", "--help"]];

    for args in refused {
        let outcome = workspace.run(args);
        assert_failed(&outcome, 2, "--help");
        assert!(
            outcome
                .stderr
                .starts_with("Error: the arguments are not valid: "),
            "{outcome:?}"
        );
    }
}

/// A key with ESC, a line break, U+007F, and U+0085, which are control
/// characters, and U+202E, which changes the direction of text.
const KEY: &str = "a\u{1b}[31mb\u{202e}c\nd\u{7f}\u{85}e.md";
/// `KEY` as it is written for people.
const KEY_FOR_PEOPLE: &str = "a\\u{1b}[31mb\\u{202e}c\\nd\\u{7f}\\u{85}e.md";
/// `KEY` as it is written in JSON: the control characters U+0000 to U+001F
/// and the characters that change the direction of text are escaped, and
/// U+007F and U+0085 are not.
const KEY_IN_JSON: &str = "\"a\\u001b[31mb\\u202ec\\nd\u{7f}\u{85}e.md\"";
const DESCRIPTION: &str = "line\u{1b}[31m red\u{202e} mirrored\nnext";
const DESCRIPTION_FOR_PEOPLE: &str = "line\\u{1b}[31m red\\u{202e} mirrored\\nnext";
const DESCRIPTION_IN_JSON: &str = "\"line\\u001b[31m red\\u202e mirrored\\nnext\"";

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn strings_from_users_and_sources_are_replaced_for_people_and_escaped_in_json() {
    let workspace = Workspace::with_real_model();

    let added = workspace.run(&[
        "collection",
        "add",
        "memo",
        "--type",
        "raw",
        "--description",
        DESCRIPTION,
    ]);
    let put = workspace.run_with_stdin(
        &["put", "memo", KEY, "--file-type", "md", "-"],
        b"# T\n\nControl words.\n",
    );
    let listed = workspace.run(&["collection", "list"]);
    let listed_as_json = workspace.run(&["collection", "list", "--json"]);
    let searched = workspace.run(&["search", "Control", "--mode", "fts"]);
    let searched_as_json = workspace.run(&["search", "Control", "--mode", "fts", "--json"]);
    let missing = workspace.run(&["get", "memo", "nope\u{1b}[31m\u{202e}"]);

    for outcome in [
        &added,
        &put,
        &listed,
        &listed_as_json,
        &searched,
        &searched_as_json,
    ] {
        assert_success(outcome);
    }
    assert!(
        added
            .stderr
            .contains(&format!("  description: {DESCRIPTION_FOR_PEOPLE}\n")),
        "{added:?}"
    );
    assert!(
        put.stderr.contains(&format!(
            "Added \"{KEY_FOR_PEOPLE}\" to the collection \"memo\"."
        )),
        "{put:?}"
    );
    assert!(
        listed
            .stdout
            .contains(&format!("\n  {DESCRIPTION_FOR_PEOPLE}\n")),
        "{listed:?}"
    );
    assert!(
        searched
            .stdout
            .starts_with(&format!("1. memo  {KEY_FOR_PEOPLE}\n")),
        "{searched:?}"
    );
    assert_failed(
        &missing,
        1,
        "the document \"nope\\u{1b}[31m\\u{202e}\" does not exist",
    );
    for written in [
        &added.stderr,
        &put.stderr,
        &listed.stdout,
        &searched.stdout,
        &missing.stderr,
    ] {
        assert!(
            !written.contains(['\u{1b}', '\u{202e}', '\u{7f}', '\u{85}']),
            "{written}"
        );
    }
    assert!(
        listed_as_json
            .stdout
            .contains(&format!("\"description\":{DESCRIPTION_IN_JSON}")),
        "{listed_as_json:?}"
    );
    assert!(
        searched_as_json
            .stdout
            .contains(&format!("\"key\":{KEY_IN_JSON}")),
        "{searched_as_json:?}"
    );
    let result: Value = serde_json::from_str(&searched_as_json.stdout).unwrap();
    assert_eq!(result["hits"][0]["key"], KEY);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_error_is_written_for_people_on_stderr_and_nothing_on_stdout_even_with_json() {
    let workspace = Workspace::new();
    let failed = [
        (
            workspace.run(&[
                "search", "words", "--mode", "fts", "-c", "nothing", "--json",
            ]),
            1,
            "Error: the collection \"nothing\" does not exist",
        ),
        (
            workspace.run(&["search", " ", "--json"]),
            2,
            "Error: the query is empty",
        ),
    ];

    for (outcome, code, start) in &failed {
        assert_eq!(outcome.code, ExitCode::from(*code), "{outcome:?}");
        assert_eq!(outcome.stdout, "", "{outcome:?}");
        assert!(outcome.stderr.starts_with(start), "{outcome:?}");
        assert!(
            serde_json::from_str::<Value>(&outcome.stderr).is_err(),
            "{outcome:?}"
        );
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn errors_are_colored_on_a_terminal_unless_no_color_is_asked() {
    let workspace = Workspace::new();
    let remove = ["collection", "remove", "nothing", "--yes"];

    let colored = workspace.run_on_a_terminal(&remove, &[]);
    let with_the_option = workspace.run_on_a_terminal(
        &["--no-color", "collection", "remove", "nothing", "--yes"],
        &[],
    );
    let with_the_variable = workspace.run_on_a_terminal(&remove, &[("NO_COLOR", "1")]);
    let not_on_a_terminal = workspace.run(&remove);

    assert_failed(&colored, 1, "the collection \"nothing\" does not exist");
    assert!(colored.stderr.contains('\u{1b}'), "{colored:?}");
    for outcome in [&with_the_option, &with_the_variable, &not_on_a_terminal] {
        assert_failed(outcome, 1, "the collection \"nothing\" does not exist");
        assert!(!outcome.stderr.contains('\u{1b}'), "{outcome:?}");
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn without_a_terminal_an_update_a_rebuild_and_maintenance_each_start_with_a_progress_line() {
    let workspace = Workspace::with_real_model();
    let dir = workspace.dir("notes");
    std::fs::write(dir.join("a.md"), "# A\n\nWords.\n").unwrap();
    assert_success(&workspace.run(&["collection", "add", "notes", dir.to_str().unwrap()]));

    let ran = [
        (
            workspace.run(&["update", "notes"]),
            "Updating 0, 0 chunks embedded, 0s",
        ),
        (
            workspace.run(&["embedding", "rebuild", "--force"]),
            "Rebuilding 0, 0s",
        ),
        (workspace.run(&["optimize"]), "Optimizing, 0s"),
    ];

    for (outcome, line) in &ran {
        assert_success(outcome);
        assert_eq!(outcome.stdout, "", "{outcome:?}");
        assert_eq!(outcome.stderr.lines().next(), Some(*line), "{outcome:?}");
    }
}

#[test]
#[cfg(unix)]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_directories_are_under_the_home_directory_unless_given_and_relative_ones_start_at_the_working_directory()
 {
    let workspace = Workspace::new();
    let home = workspace.dir("home");
    let working = workspace.dir("working");
    let mdya = |args: &[&str]| {
        executable_at_home(&home)
            .current_dir(&working)
            .args(args)
            .output()
            .unwrap()
    };
    let add = ["collection", "add", "memo", "--type", "raw"];

    let by_default = mdya(&add);
    let listed_by_default = mdya(&["collection", "list", "--json"]);
    let with_a_tilde = mdya(&[&["--data-dir", "~/given"][..], &add].concat());
    let relative = mdya(&[&["--data-dir", "relative"][..], &add].concat());
    // The embedding model cannot be fetched, but its cache directory is made
    // before fetching.
    let searched = mdya(&["search", "--mode", "vector", "words"]);

    for output in [&by_default, &listed_by_default, &with_a_tilde, &relative] {
        assert_eq!(output.status.code(), Some(0), "{}", stderr(output));
    }
    let listed: Value = serde_json::from_str(&stdout(&listed_by_default)).unwrap();
    assert_eq!(listed["collections"][0]["name"], "memo");
    let searched_stderr = stderr(&searched);
    assert_eq!(searched.status.code(), Some(1), "{searched_stderr}");
    assert!(
        searched_stderr.contains("the embedding model cannot be used"),
        "{searched_stderr}"
    );
    assert_eq!(names_in(&home), [".mdya", ".mdya-models", "given"]);
    assert_eq!(names_in(&working), ["relative"]);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_mcp_server_address_in_use_fails_with_exit_code_1_and_one_not_in_the_form_with_exit_code_2() {
    let workspace = Workspace::new();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();

    let in_use = workspace.run(&["mcp", "--http", "--address", &address]);
    let not_in_the_form = workspace.run(&["mcp", "--http", "--address", "nope"]);
    let port_out_of_range = workspace.run(&["mcp", "--http", "--address", "127.0.0.1:65536"]);

    assert_failed(&in_use, 1, &format!("cannot listen on {address}"));
    for outcome in [&not_in_the_form, &port_out_of_range] {
        assert_failed(
            outcome,
            2,
            "is not in the form <host>:<port>, with an IPv6 address in brackets",
        );
    }
}
