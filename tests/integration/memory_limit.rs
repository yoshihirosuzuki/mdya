//! Integration tests of the memory limit: the executable runs in a separate
//! process under a limit that a settings operation stays under and loading
//! the embedding model goes over, and the tests check how it stops, what it
//! leaves, and that the same operation runs again once the limit is raised.
//!
//! Each run of mdya in the test process starts the memory watchdog on the
//! test process itself, with the limit set in the data directory. While the
//! limit is lowered, the tests run mdya only in separate processes.

// `allow-unwrap-in-tests` and `allow-panic-in-tests` in clippy.toml cover
// test functions and `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::common::{Outcome, Workspace};
use crate::mcp_server::{StdioServer, succeeded};

/// How long a test waits for one run of mdya.
const WAIT: Duration = Duration::from_secs(120);

/// The lowered memory limit, in MB.
const LOW_LIMIT: &str = "64";

/// What mdya writes last on stderr when it goes over the lowered limit.
const OVER_THE_LIMIT: &str = "Error: mdya's memory usage went over the limit of 64 MB\n\n\
                              How to fix:\n    Raise the memory limit (mdya config set \
                              resources.memory_limit_mb) or lower the parallelism (mdya config \
                              set resources.embedding_parallelism), then run the operation \
                              again.\n";

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

/// What one run of the mdya executable ended with.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the mdya executable with `args` on the test's directories, in a
/// separate process with an empty stdin, and waits for it no longer than
/// `WAIT`: a run that has not ended by then is stopped, and fails the test.
fn run(workspace: &Workspace, args: &[&str]) -> Ran {
    let mut child = Running(
        workspace
            .executable()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = read_on_a_thread(child.0.stdout.take().unwrap());
    let stderr = read_on_a_thread(child.0.stderr.take().unwrap());
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "`mdya {}` did not end",
            args.join(" ")
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    Ran {
        code: status.code(),
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// Reads `output` to its end on a thread of its own, so that the process
/// writing it never waits for the test to read.
fn read_on_a_thread(mut output: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        output.read_to_string(&mut text).unwrap();
        text
    })
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

fn note(number: usize, words: &str) -> String {
    format!("# Note {number}\n\n{words} of note {number}.\n")
}

fn write_note(dir: &Path, number: usize, words: &str) {
    std::fs::write(dir.join(format!("note{number}.md")), note(number, words)).unwrap();
}

/// The text of the document `note<number>.md` in the collection `notes`, if
/// there is one. Runs mdya in the test process.
fn document(workspace: &Workspace, number: usize) -> Option<String> {
    let key = format!("note{number}.md");
    let outcome = workspace.run(&["get", "notes", &key]);
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

/// A workspace with the collection `notes` of notes 1 to 20, updated, and
/// then the memory limit lowered. Runs mdya in the test process until the
/// limit is lowered.
fn notes_under_the_low_limit(workspace: &Workspace) -> PathBuf {
    let dir = workspace.dir("notes");
    for number in 1..=20 {
        write_note(&dir, number, "Words");
    }
    assert_success(&workspace.run(&["collection", "add", "notes", dir.to_str().unwrap()]));
    assert_success(&workspace.run(&["update", "notes"]));
    assert_success(&workspace.run(&["config", "set", "resources.memory_limit_mb", LOW_LIMIT]));
    dir
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_over_the_memory_limit_ends_with_exit_code_137_and_runs_again_under_a_raised_limit() {
    let workspace = Workspace::with_real_model();
    let dir = notes_under_the_low_limit(&workspace);
    for number in 1..=3 {
        write_note(&dir, number, "Changed words");
    }

    let over = run(&workspace, &["update", "notes"]);
    let raised = run(
        &workspace,
        &["config", "set", "resources.memory_limit_mb", "8192"],
    );
    // Checked before mdya runs in the test process again, which it may only
    // under the raised limit.
    assert_eq!(raised.code, Some(0), "{raised:?}");
    let left: Vec<(usize, Option<String>)> = (1..=20)
        .map(|number| (number, document(&workspace, number)))
        .collect();
    let updated = workspace.run(&["update", "notes"]);

    assert_eq!(over.code, Some(137), "{over:?}");
    assert_eq!(over.stdout, "", "{over:?}");
    assert!(over.stderr.ends_with(OVER_THE_LIMIT), "{over:?}");
    // The update leaves what an interrupted one leaves: a note it changes is
    // as it was, as written, or missing. A note written again has no row in
    // the table of documents from when its old row is deleted until its new
    // one is added, so an update stopped in between leaves it missing until
    // the next update writes it. The notes changed are written together, so
    // all three may be missing.
    for (number, text) in &left {
        let before = Some(note(*number, "Words"));
        let candidates = match number {
            1..=3 => vec![before, Some(note(*number, "Changed words")), None],
            _ => vec![before],
        };
        assert!(
            candidates.contains(text),
            "note {number} is not left as an update over the limit may leave it: {text:?}"
        );
    }
    assert_success(&updated);
    for number in 1..=20 {
        let words = if number <= 3 {
            "Changed words"
        } else {
            "Words"
        };
        assert_eq!(
            document(&workspace, number),
            Some(note(number, words)),
            "note {number}"
        );
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn the_mcp_server_over_the_memory_limit_ends_with_exit_code_137_and_its_exchange_ends() {
    let workspace = Workspace::with_real_model();
    notes_under_the_low_limit(&workspace);
    let mut server = StdioServer::start(workspace.executable());
    server.initialize();

    // A vector search loads the embedding model.
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "search", "arguments": { "query": "words", "mode": "vector" } }
    }));
    let ended = server.wait_for_end();
    let raised = run(
        &workspace,
        &["config", "set", "resources.memory_limit_mb", "8192"],
    );
    assert_eq!(raised.code, Some(0), "{raised:?}");
    // A server started under the raised limit, to search again.
    let mut restarted = StdioServer::start(workspace.executable());
    restarted.initialize();
    let found = restarted.call("search", json!({ "query": "words", "mode": "vector" }));
    let finished = restarted.finish();

    assert_eq!(ended.status.code(), Some(137), "{}", ended.stderr);
    // No answer to the search: the exchange ends with the server.
    assert!(ended.stdout.is_empty(), "{:?}", ended.stdout);
    assert!(ended.stderr.ends_with(OVER_THE_LIMIT), "{}", ended.stderr);
    let hits = succeeded(&found)["hits"].as_array().unwrap().len();
    assert!(hits > 0, "{found}");
    assert_eq!(finished.status.code(), Some(0), "{}", finished.stderr);
}
