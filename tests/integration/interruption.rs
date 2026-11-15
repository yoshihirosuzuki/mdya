//! Integration tests of operations interrupted part way, as with Ctrl+C:
//! the executable runs in a separate process and gets SIGINT soon after it
//! begins, and the tests check what it leaves and that doing the same
//! operation again finishes it.
//!
//! Where the operation is when SIGINT comes is not fixed. The update test
//! checks what holds wherever it is. The rebuild test relies on SIGINT coming
//! before the rebuild switches to the new embedding vectors, which it does
//! only after making all of them.

// `allow-unwrap-in-tests` and `allow-panic-in-tests` in clippy.toml cover
// test functions and `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used, clippy::panic)]
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{Outcome, Workspace};

/// How long a test waits for the operation to begin, or to end once
/// interrupted.
const WAIT: Duration = Duration::from_secs(120);

/// The signal that Ctrl+C sends.
const SIGINT: i32 = 2;

/// A process of the executable, stopped if it is still running when this is
/// dropped, as when the test fails while it runs.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Runs the mdya executable with `args` on the test's directories, sends it
/// SIGINT once it has written its first line on stderr (the first line of
/// progress, as it begins), and returns how it ended.
fn interrupted(workspace: &Workspace, args: &[&str]) -> ExitStatus {
    let mut child = Running(
        workspace
            .executable()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (began, beginning) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stderr).lines();
        let _ = began.send(lines.next());
        // Keeps reading, so that the process never waits to write on stderr.
        lines.for_each(drop);
    });
    match beginning.recv_timeout(WAIT) {
        Ok(Some(Ok(_))) => {}
        other => panic!("`mdya {}` did not begin: {other:?}", args.join(" ")),
    }
    let killed = std::process::Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "`mdya {}` did not end",
            args.join(" ")
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// The result of `mdya <args> --json`, which must succeed.
fn json_of(workspace: &Workspace, args: &[&str]) -> Value {
    let mut with_json = args.to_vec();
    with_json.push("--json");
    let outcome = workspace.run(&with_json);
    assert_success(&outcome);
    serde_json::from_str(&outcome.stdout).unwrap()
}

/// The text of the document `key` in the collection `notes`, if there is
/// one.
fn document(workspace: &Workspace, key: &str) -> Option<String> {
    let outcome = workspace.run(&["get", "notes", key]);
    if outcome.code == ExitCode::SUCCESS {
        return Some(outcome.stdout);
    }
    assert!(
        outcome.stderr.starts_with(&format!(
            "Error: the document \"{key}\" does not exist in the collection \"notes\""
        )),
        "{outcome:?}"
    );
    None
}

fn note(number: usize, words: &str) -> String {
    format!("# Note {number}\n\n{words} of note {number}.\n")
}

fn write_note(dir: &Path, number: usize, words: &str) {
    std::fs::write(dir.join(format!("note{number}.md")), note(number, words)).unwrap();
}

fn key(number: usize) -> String {
    format!("note{number}.md")
}

/// Note `number` before the update that the update test interrupts, if there
/// is one.
fn before_update(number: usize) -> Option<String> {
    match number {
        1..=20 => Some(note(number, "Words")),
        _ => None,
    }
}

/// Note `number` as that update writes it: notes 1 to 10 change, 21 to 30
/// are new, 18 to 20 are deleted, and 11 to 17 stay as they were.
fn as_updated(number: usize) -> Option<String> {
    match number {
        1..=10 => Some(note(number, "Changed words")),
        11..=17 | 21..=30 => Some(note(number, "Words")),
        _ => None,
    }
}

/// The last line of the update run again after the interrupted one left
/// each note as in `left`. A note left as written is skipped as unchanged,
/// a missing one is added, and one left as it was is changed or deleted.
fn summary_after(left: &[(usize, Option<String>)]) -> String {
    let (mut added, mut changed, mut unchanged, mut deleted) = (0, 0, 0, 0);
    for (number, text) in left {
        match (text, as_updated(*number)) {
            (None, None) => {}
            (None, Some(_)) => added += 1,
            (Some(_), None) => deleted += 1,
            (Some(text), Some(written)) if *text == written => unchanged += 1,
            (Some(_), Some(_)) => changed += 1,
        }
    }
    format!(
        "Updated the collection \"notes\": {added} added, {changed} changed, \
         {unchanged} unchanged, 0 re-ingested, {deleted} deleted, 0 failed."
    )
}

/// The keys of the hits of a vector search for `query`.
fn found(workspace: &Workspace, query: &str) -> Vec<String> {
    json_of(workspace, &["search", query, "--mode", "vector", "-n", "3"])["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_interrupted_update_leaves_each_document_as_it_was_or_as_written_and_the_next_finishes() {
    let workspace = Workspace::with_real_model();
    let dir = workspace.dir("notes");
    for number in 1..=20 {
        write_note(&dir, number, "Words");
    }
    assert_success(&workspace.run(&["collection", "add", "notes", dir.to_str().unwrap()]));
    assert_success(&workspace.run(&["update", "notes"]));
    // Notes 1 to 10 change, 21 to 30 are new, and 18 to 20 are deleted;
    // notes 11 to 17 stay as they were.
    for number in 1..=10 {
        write_note(&dir, number, "Changed words");
    }
    for number in 21..=30 {
        write_note(&dir, number, "Words");
    }
    for number in 18..=20 {
        std::fs::remove_file(dir.join(key(number))).unwrap();
    }

    let status = interrupted(&workspace, &["update", "notes"]);
    let left: Vec<(usize, Option<String>)> = (1..=30)
        .map(|number| (number, document(&workspace, &key(number))))
        .collect();
    let updated = workspace.run(&["update", "notes"]);

    assert_eq!(status.signal(), Some(SIGINT), "{status}");
    for (number, text) in &left {
        // A document written again has no row in the table of documents from
        // when its old row is deleted until its new one is added, so an
        // update stopped in between leaves it missing until the next update
        // writes it.
        let missing_while_written_again = matches!(number, 1..=10) && text.is_none();
        assert!(
            *text == before_update(*number)
                || *text == as_updated(*number)
                || missing_while_written_again,
            "note {number} is not left as an interrupted update may leave it: {text:?}"
        );
    }
    assert_success(&updated);
    // The next update goes on from where the interrupted one stopped: notes
    // 11 to 17, and each note already written, are skipped.
    assert_eq!(
        updated.stderr.lines().last(),
        Some(summary_after(&left).as_str()),
        "{updated:?}"
    );
    for number in 1..=30 {
        assert_eq!(
            document(&workspace, &key(number)),
            as_updated(number),
            "note {number}"
        );
    }
    let collections = json_of(&workspace, &["collection", "list"]);
    assert_eq!(collections["collections"][0]["document_count"], 27);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_interrupted_rebuild_leaves_the_search_as_it_was_and_the_next_rebuild_finishes() {
    let workspace = Workspace::with_real_model();
    let dir = workspace.dir("notes");
    for number in 1..=20 {
        write_note(&dir, number, "Words");
    }
    write_note(&dir, 21, "Release steps and tags");
    assert_success(&workspace.run(&["collection", "add", "notes", dir.to_str().unwrap()]));
    assert_success(&workspace.run(&["update", "notes"]));
    let found_before = found(&workspace, "release tags");
    let status_before = json_of(&workspace, &["status"]);

    // SIGINT comes before the rebuild has made the new embedding vectors,
    // which takes seconds, so it does not stop the rebuild while it switches
    // to them, though nothing makes sure of that. Stopped while switching,
    // the rebuild may leave a table without the vector column the search
    // reads, and then the vector search has no hits and the status counts no
    // chunk as having a vector, failing the checks of the interrupted rebuild
    // below. Such a stop is left to the unit tests of the index.
    let status = interrupted(&workspace, &["embedding", "rebuild", "--force"]);
    let found_interrupted = found(&workspace, "release tags");
    let status_interrupted = json_of(&workspace, &["status"]);
    let rebuilt = workspace.run(&["embedding", "rebuild", "--force"]);

    assert_eq!(status.signal(), Some(SIGINT), "{status}");
    assert_eq!(found_before[0], "note21.md");
    assert_eq!(
        found_interrupted, found_before,
        "the rebuild may have been stopped while switching"
    );
    assert_eq!(
        status_interrupted, status_before,
        "the rebuild may have been stopped while switching"
    );
    assert_success(&rebuilt);
    assert!(
        rebuilt
            .stderr
            .ends_with("Rebuilt the embeddings of 21 chunks.\n"),
        "{rebuilt:?}"
    );
    assert_eq!(found(&workspace, "release tags"), found_before);
    assert_eq!(json_of(&workspace, &["status"]), status_before);
}
