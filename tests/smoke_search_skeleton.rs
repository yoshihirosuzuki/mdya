//! Smoke tests for the `mdya search` CLI surface. FTS / vector /
//! hybrid behaviours live in their own smoke files; this file pins
//! the high-level `--help` surface that lists every mode.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn search_help_lists_three_subcommands() {
    Command::cargo_bin("mdya")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(contains("fts"))
        .stdout(contains("vector"))
        .stdout(contains("hybrid"));
}
