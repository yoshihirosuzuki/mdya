//! Integration tests of operations that run at the same time, on one data
//! directory: a write operation runs in a separate process while the tests
//! run other write and read operations, each in a separate process too.
//!
//! The write operation is `mdya put ... -` with its stdin left open: it takes
//! the write lock, and then waits for the document's contents on stdin, so
//! it holds the lock for as long as the test leaves stdin open. It prepares
//! the embedding model only once it has read the contents.

// `allow-unwrap-in-tests` and `allow-panic-in-tests` in clippy.toml cover
// test functions and `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ExitCode, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{Outcome, Workspace};

/// What a write operation writes on stderr when another one holds the lock.
const IN_PROGRESS: &str = "Error: another write operation is in progress\n\nHow to fix:\n    \
                           Wait until the other write operation ends, then run the operation \
                           again.\n    A write operation that stopped partway (when \
                           interrupted, for example) does not keep the next one from \
                           starting.\n";

/// How long a test waits for one run of mdya, or for the write operation to
/// take the lock, long enough for a run that loads the real embedding model.
const WAIT: Duration = Duration::from_secs(120);

const RELEASE: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";
const RELEASE_REVISED: &str = "# リリースの手順\n\n署名してから、タグを打ちます。\n";
const TAGS: &str = "# タグの付け方\n\nタグの名前は、版の番号にします。\n";

/// A `mdya put` in a separate process, holding the write lock while it
/// waits for the document's contents on its stdin.
struct WaitingPut {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl WaitingPut {
    /// Starts `mdya put <collection> <key> --file-type md -` with the test's
    /// directories, and returns once it holds the write lock.
    fn start(workspace: &Workspace, collection: &str, key: &str) -> Self {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "the put did not take the lock");
            let mut child = workspace
                .executable()
                .args(["put", collection, key, "--file-type", "md", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take();
            // Owned before anything can fail, so that it is stopped if the
            // test fails here.
            let mut put = Self { child, stdin };
            if put.holds_the_lock(workspace, deadline) {
                return put;
            }
        }
    }

    /// Whether the put took the lock: tries a write operation that changes
    /// nothing, until it fails because the put holds the lock. That write
    /// operation holds the lock for a moment each time, and a put that tries
    /// to take it then ends with the error: then this returns false, for the
    /// put to be started again.
    fn holds_the_lock(&mut self, workspace: &Workspace, deadline: Instant) -> bool {
        loop {
            let ran = run(workspace, &["config", "set", "mcp.show_version", "true"]);
            if ran.code == Some(1) && ran.stderr == IN_PROGRESS {
                return true;
            }
            assert_eq!(ran.code, Some(0), "{ran:?}");
            if let Some(status) = self.child.try_wait().unwrap() {
                let stderr = self.stderr();
                assert!(
                    stderr.ends_with(IN_PROGRESS),
                    "the put ended, {status}: {stderr}"
                );
                return false;
            }
            assert!(Instant::now() < deadline, "the put did not take the lock");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// What the put wrote on stderr, once it has ended.
    fn stderr(&mut self) -> String {
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }

    /// Gives the put the document's contents, closes its stdin, and waits for
    /// it to end. Returns how it ended and what it wrote on stderr.
    fn finish(&mut self, contents: &str) -> (ExitStatus, String) {
        let mut stdin = self.stdin.take().unwrap();
        stdin.write_all(contents.as_bytes()).unwrap();
        drop(stdin);
        let status = wait_for(&mut self.child, "the put");
        (status, self.stderr())
    }

    /// Stops the put part way, as when it is killed, while it holds the lock.
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for WaitingPut {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// A process of the executable, stopped if it is still running when this is
/// dropped, as when the test fails while it runs.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        stop(&mut self.0);
    }
}

/// What one run of the mdya executable ended with.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the mdya executable with `args` on the test's directories, with an
/// empty stdin.
fn run(workspace: &Workspace, args: &[&str]) -> Ran {
    run_with_stdin(workspace, args, b"")
}

/// Runs the mdya executable with `args` on the test's directories, with
/// `stdin` and then the end of it on its stdin, and waits for it no longer
/// than `WAIT`: a run that has not ended by then is stopped, and fails the
/// test. An operation that runs while another holds the write lock must not
/// wait for it.
fn run_with_stdin(workspace: &Workspace, args: &[&str], stdin: &[u8]) -> Ran {
    let mut child = Running(
        workspace
            .executable()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = read_on_a_thread(child.0.stdout.take().unwrap());
    let stderr = read_on_a_thread(child.0.stderr.take().unwrap());
    let mut input = child.0.stdin.take().unwrap();
    // An operation that does not read its stdin may end before the test
    // writes it.
    if let Err(error) = input.write_all(stdin) {
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe, "{error}");
    }
    drop(input);
    let status = wait_for(&mut child.0, &format!("`mdya {}`", args.join(" ")));
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

/// Waits for `child` to end, no longer than `WAIT`: one that has not ended
/// by then is stopped, and fails the test.
fn wait_for(child: &mut Child, what: &str) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            stop(child);
            panic!("{what} did not end");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop(child: &mut Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// The result of `mdya <args> --json`, which must succeed.
fn json_of(workspace: &Workspace, args: &[&str]) -> Value {
    let mut with_json = args.to_vec();
    with_json.push("--json");
    let ran = run(workspace, &with_json);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    serde_json::from_str(&ran.stdout).unwrap()
}

fn raw_collection(workspace: &Workspace, name: &str) {
    assert_success(&workspace.run(&["collection", "add", name, "--type", "raw"]));
}

fn put(workspace: &Workspace, collection: &str, key: &str, contents: &str) {
    assert_success(&workspace.run_with_stdin(
        &["put", collection, key, "--file-type", "md", "-"],
        contents.as_bytes(),
    ));
}

/// The keys of the hits of a full-text search for `query`.
fn found(workspace: &Workspace, query: &str) -> Vec<String> {
    json_of(workspace, &["search", query, "--mode", "fts"])["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn while_a_write_operation_runs_another_fails_with_exit_code_1_and_reads_do_not_wait() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "notes");
    let collections = json_of(&workspace, &["collection", "list"]);
    let settings = json_of(&workspace, &["config", "list"]);
    let another_data_dir = Workspace::new();
    let mut waiting = WaitingPut::start(&workspace, "notes", "a.md");

    // Each write operation with what it writes on stderr before the error:
    // the operations that show progress start it, as they write the first
    // line of progress when they begin.
    let writes: [(&[&str], &str); 7] = [
        (&["collection", "add", "other", "--type", "raw"], ""),
        (
            &["collection", "set", "notes", "--description", "changed"],
            "",
        ),
        (&["collection", "remove", "notes", "--yes"], ""),
        (&["config", "set", "get.cli_limit_bytes", "5"], ""),
        (&["update", "--all"], "Updating 0, 0 chunks embedded, 0s\n"),
        (&["optimize"], "Optimizing, 0s\n"),
        (&["remove", "notes", "a.md", "--yes"], ""),
    ];
    let refused: Vec<(Ran, &str)> = writes
        .iter()
        .map(|&(args, progress)| (run(&workspace, args), progress))
        .chain([(
            run_with_stdin(
                &workspace,
                &["put", "notes", "b.md", "--file-type", "md", "-"],
                b"# B\n",
            ),
            "",
        )])
        .collect();
    let reads: [&[&str]; 4] = [
        &["collection", "list", "--json"],
        &["config", "list", "--json"],
        &["search", "anything", "--mode", "fts", "--json"],
        &["--version"],
    ];
    let answered: Vec<Ran> = reads.iter().map(|args| run(&workspace, args)).collect();
    let elsewhere = run(
        &another_data_dir,
        &["collection", "add", "notes", "--type", "raw"],
    );
    waiting.kill();

    for (ran, progress) in &refused {
        assert_eq!(ran.code, Some(1), "{ran:?}");
        assert_eq!(ran.stdout, "", "{ran:?}");
        assert_eq!(ran.stderr, format!("{progress}{IN_PROGRESS}"), "{ran:?}");
    }
    for ran in &answered {
        assert_eq!(ran.code, Some(0), "{ran:?}");
    }
    assert_eq!(json_of(&workspace, &["collection", "list"]), collections);
    assert_eq!(json_of(&workspace, &["config", "list"]), settings);
    assert_eq!(elsewhere.code, Some(0), "{elsewhere:?}");
    assert!(!workspace.model_was_prepared());
    assert!(!another_data_dir.model_was_prepared());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_write_operation_that_stopped_partway_does_not_keep_the_next_from_starting() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "notes");
    let mut waiting = WaitingPut::start(&workspace, "notes", "a.md");

    waiting.kill();
    let added = run(&workspace, &["collection", "add", "other", "--type", "raw"]);

    assert_eq!(added.code, Some(0), "{added:?}");
    let collections = json_of(&workspace, &["collection", "list"]);
    let names: Vec<&str> = collections["collections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|collection| collection["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["notes", "other"]);
    assert_eq!(collections["collections"][0]["document_count"], 0);
    assert!(!workspace.model_was_prepared());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_read_sees_each_document_as_it_was_until_the_write_operation_has_written_it() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "notes");
    put(&workspace, "notes", "release.md", RELEASE);

    // Words in only the previous version and in only the new one.
    let mut revising = WaitingPut::start(&workspace, "notes", "release.md");
    let before_revising = run(&workspace, &["get", "notes", "release.md"]);
    let old_words_before_revising = found(&workspace, "変更履歴");
    let new_words_before_revising = found(&workspace, "署名");
    let (revised, revised_stderr) = revising.finish(RELEASE_REVISED);
    let after_revising = run(&workspace, &["get", "notes", "release.md"]);
    let old_words_after_revising = found(&workspace, "変更履歴");
    let new_words_after_revising = found(&workspace, "署名");

    let mut adding = WaitingPut::start(&workspace, "notes", "tags.md");
    let before_adding = run(&workspace, &["get", "notes", "tags.md"]);
    let count_before_adding = json_of(&workspace, &["collection", "list"]);
    let (added, added_stderr) = adding.finish(TAGS);
    let after_adding = run(&workspace, &["get", "notes", "tags.md"]);
    let count_after_adding = json_of(&workspace, &["collection", "list"]);

    assert_eq!(before_revising.stdout, RELEASE, "{before_revising:?}");
    assert_eq!(old_words_before_revising, ["release.md"]);
    assert_eq!(new_words_before_revising, Vec::<String>::new());
    assert!(revised.success(), "{revised_stderr}");
    assert_eq!(after_revising.stdout, RELEASE_REVISED, "{after_revising:?}");
    assert_eq!(old_words_after_revising, Vec::<String>::new());
    assert_eq!(new_words_after_revising, ["release.md"]);
    assert_eq!(before_adding.code, Some(1), "{before_adding:?}");
    assert!(
        before_adding.stderr.starts_with(
            "Error: the document \"tags.md\" does not exist in the collection \"notes\""
        ),
        "{before_adding:?}"
    );
    assert_eq!(count_before_adding["collections"][0]["document_count"], 1);
    assert!(added.success(), "{added_stderr}");
    assert_eq!(after_adding.stdout, TAGS, "{after_adding:?}");
    assert_eq!(count_after_adding["collections"][0]["document_count"], 2);
}
