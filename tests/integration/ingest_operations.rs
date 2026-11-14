//! The operations that ingest and remove documents on the command line, with
//! the real components: updating one collection or all of them, putting the
//! content of a document into a collection, and removing a document, with the
//! errors these operations report.
//!
//! In the test's own process, the function that sets the log receiver is
//! replaced with one that sets nothing (see the module `common`), so the
//! warnings that mdya logs go nowhere. The tests that check a warning run the
//! executable.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Output};

use crate::common::{Outcome, Workspace};

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// Writes `contents` to the file `name` in `dir`, creating the directories
/// on the way.
fn write(dir: &Path, name: &str, contents: impl AsRef<[u8]>) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A file named `name` in `workspace`, holding `contents`.
fn file(workspace: &Workspace, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
    let dir = workspace.dir("files");
    write(&dir, name, contents);
    dir.join(name)
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// Makes a directory collection named `name` on a directory of the same name
/// that holds `files`, and returns the directory.
fn directory_collection(workspace: &Workspace, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = workspace.dir(name);
    for (file, contents) in files {
        write(&dir, file, contents);
    }
    assert_success(&workspace.run(&["collection", "add", name, &text(&dir)]));
    dir
}

/// Makes an archive collection named `name` on a copy of a fixture, a tar
/// that holds `notes/short.md` and one more Markdown document, and returns
/// the copy.
fn archive_collection(workspace: &Workspace, name: &str) -> PathBuf {
    let archive = workspace.dir("archives").join(format!("{name}.tar.gz"));
    std::fs::copy(fixture("archives/python-ustar.tar.gz"), &archive).unwrap();
    assert_success(&workspace.run(&[
        "collection",
        "add",
        name,
        "--type",
        "archive",
        &text(&archive),
    ]));
    archive
}

fn raw_collection(workspace: &Workspace, name: &str) {
    assert_success(&workspace.run(&["collection", "add", name, "--type", "raw"]));
}

/// Puts the Markdown file `file` into `collection` under `key`, with
/// `options` added.
fn put(
    workspace: &Workspace,
    collection: &str,
    key: &str,
    file: &Path,
    options: &[&str],
) -> Outcome {
    let mut args = vec!["put", collection, key, "--file-type", "md"];
    args.extend_from_slice(options);
    let file = text(file);
    args.push(&file);
    workspace.run(&args)
}

/// The text of the document `key` in `collection`, or `None` when there is no
/// such document.
fn got(workspace: &Workspace, collection: &str, key: &str) -> Option<String> {
    let got = workspace.run(&["get", collection, key]);
    if got.code == ExitCode::SUCCESS {
        return Some(got.stdout);
    }
    assert_eq!(got.code, exit_code(1), "{got:?}");
    assert!(got.stderr.contains("does not exist"), "{got:?}");
    None
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

fn exit_code(code: u8) -> ExitCode {
    ExitCode::from(code)
}

/// What a run of the executable wrote to stderr.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The line of `output` that holds every one of `parts`.
fn line_with<'a>(output: &'a str, parts: &[&str]) -> Option<&'a str> {
    output
        .lines()
        .find(|line| parts.iter().all(|part| line.contains(part)))
}

/// The summary of an update of the collection `name` with the six counts
/// `counts`, in the order the summary gives them.
fn updated(name: &str, counts: [u32; 6]) -> String {
    format!(
        "Updated the collection \"{name}\": {} added, {} changed, {} unchanged, {} re-ingested, {} deleted, {} failed.",
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
    )
}

/// Takes permissions away from a directory while it lives, and gives them
/// back when dropped, so that the test's directory can be removed even when
/// the test fails.
#[cfg(unix)]
struct Locked(PathBuf);

#[cfg(unix)]
impl Locked {
    fn new(dir: &Path, mode: u32) -> Self {
        set_mode(dir, mode).unwrap();
        Self(dir.to_owned())
    }
}

#[cfg(unix)]
impl Drop for Locked {
    fn drop(&mut self) {
        let _ = set_mode(&self.0, 0o755);
    }
}

#[cfg(unix)]
fn set_mode(dir: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode))
}

#[test]
fn updating_a_collection_whose_directory_is_gone_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    let documents = directory_collection(&workspace, "notes", &[("a.md", "# A\n")]);
    std::fs::remove_dir_all(&documents).unwrap();

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_eq!(updated_notes.code, exit_code(1), "{updated_notes:?}");
    assert_eq!(updated_notes.stdout, "", "{updated_notes:?}");
    assert!(
        updated_notes.stderr.contains(&updated("notes", [0; 6])),
        "{updated_notes:?}"
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_update_needs_either_one_collection_name_or_all() {
    let workspace = Workspace::new();
    directory_collection(&workspace, "notes", &[]);

    let refused = [
        workspace.run(&["update"]),
        workspace.run(&["update", "notes", "notes"]),
        workspace.run(&["update", "--all", "notes"]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(2), "{outcome:?}");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn updating_putting_or_removing_in_a_collection_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    let document = file(&workspace, "a.md", "# A\n");

    let refused = [
        workspace.run(&["update", "nothing"]),
        put(&workspace, "nothing", "a.md", &document, &[]),
        workspace.run(&["remove", "nothing", "a.md", "--yes"]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
        assert!(outcome.stderr.contains("\"nothing\""), "{outcome:?}");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn updating_a_raw_collection_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo");

    let refused = workspace.run(&["update", "memo"]);

    assert_eq!(refused.code, exit_code(1), "{refused:?}");
    assert!(refused.stderr.contains("mdya put"), "{refused:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn putting_into_a_directory_or_archive_collection_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    directory_collection(&workspace, "notes", &[]);
    archive_collection(&workspace, "arch");
    let document = file(&workspace, "a.md", "# A\n");

    let refused = [
        put(&workspace, "notes", "a.md", &document, &[]),
        put(&workspace, "arch", "a.md", &document, &[]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
        assert!(outcome.stderr.contains("mdya update"), "{outcome:?}");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_file_type_that_is_not_supported_fails_with_exit_code_2() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n");

    let refused: Vec<Outcome> = ["text/plain", "docx", "text/markdown; charset=ISO-8859-1"]
        .iter()
        .map(|file_type| {
            workspace.run(&[
                "put",
                "memo",
                "a.md",
                "--file-type",
                file_type,
                &text(&document),
            ])
        })
        .collect();

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(2), "{outcome:?}");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_document_without_a_terminal_or_yes_fails_with_exit_code_2() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo");

    // The document does not exist either, which is found later.
    let refused = workspace.run(&["remove", "memo", "a.md"]);

    assert_eq!(refused.code, exit_code(2), "{refused:?}");
    assert!(refused.stderr.contains("--yes"), "{refused:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_document_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo");

    let refused = workspace.run(&["remove", "memo", "a.md", "--yes"]);

    assert_eq!(refused.code, exit_code(1), "{refused:?}");
    assert!(refused.stderr.contains("\"a.md\""), "{refused:?}");
}

#[test]
#[cfg(unix)]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_update_or_a_put_that_cannot_write_the_index_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    directory_collection(&workspace, "notes", &[("a.md", "# A\n")]);
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n");

    let locked = Locked::new(workspace.data_dir(), 0o555);
    let refused = [
        workspace.run(&["update", "notes"]),
        put(&workspace, "memo", "a.md", &document, &[]),
    ];
    drop(locked);

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
        assert!(
            outcome.stderr.contains("the index cannot be written"),
            "{outcome:?}"
        );
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_makes_the_collection_match_its_directory() {
    let workspace = Workspace::with_real_model();
    let documents = directory_collection(
        &workspace,
        "notes",
        &[
            ("a.md", "# A\n\nFirst words.\n"),
            ("b.md", "# B\n\nOther words.\n"),
        ],
    );

    let first = workspace.run(&["update", "notes"]);
    write(&documents, "a.md", "# A\n\nNew words.\n");
    std::fs::remove_file(documents.join("b.md")).unwrap();
    write(&documents, "c.md", "# C\n\nMore words.\n");
    let second = workspace.run(&["update", "notes"]);

    assert_success(&first);
    assert!(
        first.stderr.contains(&updated("notes", [2, 0, 0, 0, 0, 0])),
        "{first:?}"
    );
    assert_success(&second);
    assert!(
        second
            .stderr
            .contains(&updated("notes", [1, 1, 0, 0, 1, 0])),
        "{second:?}"
    );
    assert_eq!(
        got(&workspace, "notes", "a.md").as_deref(),
        Some("# A\n\nNew words.\n")
    );
    assert_eq!(got(&workspace, "notes", "b.md"), None);
    assert_eq!(
        got(&workspace, "notes", "c.md").as_deref(),
        Some("# C\n\nMore words.\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_skips_unchanged_documents_and_ingests_them_again_when_forced() {
    let workspace = Workspace::with_real_model();
    directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);
    assert_success(&workspace.run(&["update", "notes"]));

    let again = workspace.run(&["update", "notes"]);
    let forced = workspace.run(&["update", "notes", "--force"]);

    assert_success(&again);
    assert!(
        again.stderr.contains(&updated("notes", [0, 0, 1, 0, 0, 0])),
        "{again:?}"
    );
    assert_success(&forced);
    assert!(
        forced
            .stderr
            .contains(&updated("notes", [0, 0, 0, 1, 0, 0])),
        "{forced:?}"
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_tells_its_progress_on_stderr_and_writes_nothing_to_stdout() {
    let workspace = Workspace::with_real_model();
    directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_success(&updated_notes);
    assert_eq!(updated_notes.stdout, "", "{updated_notes:?}");
    assert!(
        updated_notes.stderr.starts_with("Updating"),
        "{updated_notes:?}"
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_without_the_final_maintenance_updates_the_collection() {
    let workspace = Workspace::with_real_model();
    directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);

    let updated_notes = workspace.run(&["update", "notes", "--no-optimize"]);

    assert_success(&updated_notes);
    assert!(
        updated_notes
            .stderr
            .contains(&updated("notes", [1, 0, 0, 0, 0, 0])),
        "{updated_notes:?}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn updating_all_collections_updates_the_directory_and_archive_collections_only() {
    let workspace = Workspace::with_real_model();
    directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);
    archive_collection(&workspace, "arch");
    raw_collection(&workspace, "memo");
    let memo = file(&workspace, "memo.md", "# Memo\n\nWords.\n");
    assert_success(&put(&workspace, "memo", "memo.md", &memo, &[]));

    let updated_all = workspace.run(&["update", "--all"]);

    assert_success(&updated_all);
    assert!(
        updated_all.stderr.contains(
            "Updated 2 collections: 3 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed."
        ),
        "{updated_all:?}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
    assert!(got(&workspace, "arch", "notes/short.md").is_some());
    assert_eq!(
        got(&workspace, "memo", "memo.md").as_deref(),
        Some("# Memo\n\nWords.\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn putting_a_document_adds_skips_ingests_again_and_changes_it() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let first = file(&workspace, "first.md", "# Memo\n\nWords.\n");
    // The same text in a file written later: only the text tells the
    // versions of a raw collection's document apart.
    let same = file(&workspace, "same.md", "# Memo\n\nWords.\n");
    let other = file(&workspace, "other.md", "# Memo\n\nOther words.\n");

    let added = put(&workspace, "memo", "memo.md", &first, &[]);
    let skipped = put(&workspace, "memo", "memo.md", &same, &[]);
    let forced = put(&workspace, "memo", "memo.md", &same, &["--force"]);
    let changed = put(&workspace, "memo", "memo.md", &other, &[]);

    let told = [
        (&added, "Added \"memo.md\" to the collection \"memo\"."),
        (
            &skipped,
            "Skipped \"memo.md\" in the collection \"memo\": it has not changed.",
        ),
        (
            &forced,
            "Ingested \"memo.md\" in the collection \"memo\" again, though it had not changed.",
        ),
        (&changed, "Changed \"memo.md\" in the collection \"memo\"."),
    ];
    for (outcome, message) in told {
        assert_success(outcome);
        assert_eq!(outcome.stdout, "", "{outcome:?}");
        assert!(outcome.stderr.contains(message), "{message}: {outcome:?}");
    }
    assert_eq!(
        got(&workspace, "memo", "memo.md").as_deref(),
        Some("# Memo\n\nOther words.\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn putting_a_document_without_the_final_maintenance_puts_it() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n\nWords.\n");

    let put_one = put(&workspace, "memo", "a.md", &document, &["--no-optimize"]);

    assert_success(&put_one);
    assert!(
        put_one
            .stderr
            .contains("Added \"a.md\" to the collection \"memo\"."),
        "{put_one:?}"
    );
    assert!(got(&workspace, "memo", "a.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_key_to_put_can_be_any_string_even_an_empty_one() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n\nWords.\n");
    let keys = ["", "日本語/a b/../c.md", " "];

    let outcomes: Vec<Outcome> = keys
        .iter()
        .map(|key| put(&workspace, "memo", key, &document, &[]))
        .collect();

    for outcome in &outcomes {
        assert_success(outcome);
    }
    for key in keys {
        assert_eq!(
            got(&workspace, "memo", key).as_deref(),
            Some("# A\n\nWords.\n"),
            "{key:?}"
        );
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_file_type_to_put_is_a_mime_type_or_an_alias_in_any_letter_case() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let markdown = text(&file(&workspace, "a.md", "# A\n\nWords.\n"));
    let pdf = text(&fixture("pdfs/english.pdf"));
    let given = [
        ("TEXT/MARKDOWN", &markdown),
        ("Markdown", &markdown),
        ("MD", &markdown),
        ("text/markdown; charset=UTF-8", &markdown),
        ("text/markdown; variant=GFM", &markdown),
        ("Application/PDF", &pdf),
        ("Pdf", &pdf),
    ];

    for (index, (file_type, location)) in given.iter().enumerate() {
        let key = index.to_string();
        let put_one = workspace.run(&["put", "memo", &key, "--file-type", file_type, location]);

        assert_success(&put_one);
        assert!(got(&workspace, "memo", &key).is_some(), "{file_type}");
    }
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_to_put_is_read_from_stdin_when_its_location_is_a_dash() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");

    let put_one = workspace.run_with_stdin(
        &["put", "memo", "a.md", "--file-type", "md", "-"],
        b"# From stdin\n\nWords.\n",
    );

    assert_success(&put_one);
    assert_eq!(
        got(&workspace, "memo", "a.md").as_deref(),
        Some("# From stdin\n\nWords.\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn putting_a_document_that_cannot_be_read_fails_with_exit_code_1_and_changes_nothing() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let readable = file(&workspace, "a.md", "# A\n\nWords.\n");
    assert_success(&put(&workspace, "memo", "a.md", &readable, &[]));
    let not_utf8 = text(&file(&workspace, "not-utf8.md", b"\xff\xfe"));
    let not_pdf = text(&file(&workspace, "not.pdf", "not a PDF"));
    let missing = text(&workspace.dir("files").join("missing.md"));

    let refused = [
        workspace.run(&["put", "memo", "a.md", "--file-type", "md", &not_utf8]),
        workspace.run(&["put", "memo", "a.md", "--file-type", "pdf", &not_pdf]),
        workspace.run(&["put", "memo", "a.md", "--file-type", "md", &missing]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
        assert!(outcome.stderr.contains("Caused by:"), "{outcome:?}");
    }
    assert_eq!(
        got(&workspace, "memo", "a.md").as_deref(),
        Some("# A\n\nWords.\n")
    );
}

/// A workspace whose directory collection `notes` holds `a.md` and has been
/// updated, and the collection's directory.
fn with_an_updated_document() -> (Workspace, PathBuf) {
    let workspace = Workspace::with_real_model();
    let documents = directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);
    assert_success(&workspace.run(&["update", "notes"]));
    (workspace, documents)
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn removing_a_document_asks_and_keeps_it_when_the_user_declines() {
    let (workspace, _) = with_an_updated_document();

    let declined = workspace.run_answering(&["remove", "notes", "a.md"], "n\n");

    assert_eq!(declined.code, exit_code(1), "{declined:?}");
    assert!(
        declined
            .stderr
            .contains("Remove \"a.md\" from the collection \"notes\"? [y/N]"),
        "{declined:?}"
    );
    assert!(
        declined
            .stderr
            .contains("Did not remove \"a.md\" from the collection \"notes\"."),
        "{declined:?}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn removing_a_document_removes_it_when_the_user_agrees_and_leaves_its_source_alone() {
    let (workspace, documents) = with_an_updated_document();

    let removed = workspace.run_answering(&["remove", "notes", "a.md"], "y\n");

    assert_success(&removed);
    assert_eq!(removed.stdout, "", "{removed:?}");
    assert!(
        removed.stderr.contains(
            "Removed \"a.md\" from the collection \"notes\".\nIf it is still in the collection's source, it comes back at the next update.\n"
        ),
        "{removed:?}"
    );
    assert_eq!(got(&workspace, "notes", "a.md"), None);
    assert!(documents.join("a.md").is_file());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn removing_a_put_document_with_yes_neither_asks_nor_says_it_comes_back() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n\nWords.\n");
    assert_success(&put(&workspace, "memo", "a.md", &document, &[]));

    let removed = workspace.run(&["remove", "memo", "a.md", "--yes"]);

    assert_success(&removed);
    assert!(
        removed
            .stderr
            .contains("Removed \"a.md\" from the collection \"memo\".\n"),
        "{removed:?}"
    );
    assert!(!removed.stderr.contains("[y/N]"), "{removed:?}");
    assert!(!removed.stderr.contains("comes back"), "{removed:?}");
    assert_eq!(got(&workspace, "memo", "a.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn removing_a_document_without_the_final_maintenance_removes_it() {
    let (workspace, _) = with_an_updated_document();

    let removed = workspace.run(&["remove", "notes", "a.md", "--yes", "--no-optimize"]);

    assert_success(&removed);
    assert_eq!(got(&workspace, "notes", "a.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_skips_documents_that_cannot_be_read_warns_and_fails_with_exit_code_1() {
    let workspace = Workspace::with_real_model();
    let documents = directory_collection(&workspace, "notes", &[("good.md", "# Good\n\nWords.\n")]);
    write(&documents, "not-utf8.md", b"\xff\xfe");
    write(&documents, "not.pdf", "not a PDF");

    let output = workspace.run_executable(&["--log-level", "warn", "update", "notes"]);
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&updated("notes", [1, 0, 0, 0, 0, 2])),
        "{stderr}"
    );
    for key in ["key=\"not-utf8.md\"", "key=\"not.pdf\""] {
        assert!(
            line_with(
                &stderr,
                &["a document cannot be read and was skipped", key, "cause="]
            )
            .is_some(),
            "{key}: {stderr}"
        );
    }
    assert!(got(&workspace, "notes", "good.md").is_some());
    assert_eq!(got(&workspace, "notes", "not-utf8.md"), None);
    assert_eq!(got(&workspace, "notes", "not.pdf"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_that_cannot_be_read_keeps_its_previous_version() {
    let (workspace, documents) = with_an_updated_document();
    write(&documents, "a.md", b"\xff\xfe");

    let updated_notes = workspace.run(&["update", "notes"]);

    assert_eq!(updated_notes.code, exit_code(1), "{updated_notes:?}");
    assert_eq!(
        got(&workspace, "notes", "a.md").as_deref(),
        Some("# A\n\nWords.\n")
    );
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn updating_all_collections_goes_on_past_a_directory_that_is_gone_and_keeps_its_documents() {
    let workspace = Workspace::with_real_model();
    let notes = directory_collection(&workspace, "notes", &[("a.md", "# A\n\nWords.\n")]);
    let other = directory_collection(&workspace, "other", &[("b.md", "# B\n\nWords.\n")]);
    assert_success(&workspace.run(&["update", "--all"]));
    std::fs::rename(&notes, workspace.dir("moved").join("notes")).unwrap();
    write(&other, "c.md", "# C\n\nWords.\n");

    let output = workspace.run_executable(&["--log-level", "warn", "update", "--all"]);
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        line_with(
            &stderr,
            &[
                "the source cannot be read and the collection was not updated",
                "collection=\"notes\"",
                "cause="
            ]
        )
        .is_some(),
        "{stderr}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
    assert!(got(&workspace, "other", "c.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_of_an_archive_that_is_broken_or_gone_keeps_its_documents() {
    let workspace = Workspace::with_real_model();
    let archive = archive_collection(&workspace, "arch");
    assert_success(&workspace.run(&["update", "arch"]));

    std::fs::write(&archive, "not an archive").unwrap();
    let broken = workspace.run(&["update", "arch"]);
    let kept_after_broken = got(&workspace, "arch", "notes/short.md");
    std::fs::remove_file(&archive).unwrap();
    let gone = workspace.run(&["update", "arch"]);

    assert_eq!(broken.code, exit_code(1), "{broken:?}");
    assert!(kept_after_broken.is_some());
    assert_eq!(gone.code, exit_code(1), "{gone:?}");
    assert!(got(&workspace, "arch", "notes/short.md").is_some());
}

#[test]
#[cfg(unix)]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_reads_the_readable_part_of_a_directory_and_keeps_the_documents_in_the_rest() {
    let workspace = Workspace::with_real_model();
    let documents = directory_collection(
        &workspace,
        "notes",
        &[
            ("top.md", "# Top\n\nWords.\n"),
            ("sub/inner.md", "# Inner\n\nWords.\n"),
        ],
    );
    assert_success(&workspace.run(&["update", "notes"]));
    write(&documents, "new.md", "# New\n\nWords.\n");

    let locked = Locked::new(&documents.join("sub"), 0o000);
    let output = workspace.run_executable(&["--log-level", "warn", "update", "notes"]);
    drop(locked);
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        line_with(
            &stderr,
            &[
                "a part of the directory cannot be read and was skipped",
                "place=\"sub\"",
                "cause="
            ]
        )
        .is_some(),
        "{stderr}"
    );
    assert!(got(&workspace, "notes", "new.md").is_some());
    assert!(got(&workspace, "notes", "sub/inner.md").is_some());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_that_cannot_use_the_embedding_model_stops_with_exit_code_1() {
    let (workspace, documents) = with_an_updated_document();
    write(&documents, "b.md", "# B\n\nWords.\n");

    let output = workspace
        .executable_without_the_model()
        .args(["update", "notes"])
        .output()
        .unwrap();
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("the embedding model cannot be used"),
        "{stderr}"
    );
    assert!(stderr.contains("Caused by:"), "{stderr}");
    // `a.md` is counted as unchanged if it is reached before `b.md` fails,
    // which this test does not fix.
    assert!(
        [0, 1].iter().any(|unchanged| stderr.contains(&format!(
            "Before it stopped: 0 added, 0 changed, {unchanged} unchanged, 0 re-ingested, 0 deleted, 0 failed."
        ))),
        "{stderr}"
    );
    assert!(got(&workspace, "notes", "a.md").is_some());
    assert_eq!(got(&workspace, "notes", "b.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn putting_a_document_when_the_embedding_model_cannot_be_used_fails_with_exit_code_1() {
    let workspace = Workspace::with_real_model();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "a.md", "# A\n\nWords.\n");
    assert_success(&put(&workspace, "memo", "a.md", &document, &[]));

    let output = workspace
        .executable_without_the_model()
        .args(["put", "memo", "b.md", "--file-type", "md", &text(&document)])
        .output()
        .unwrap();
    let stderr = stderr(&output);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("the embedding model cannot be used"),
        "{stderr}"
    );
    assert!(stderr.contains("Caused by:"), "{stderr}");
    assert_eq!(got(&workspace, "memo", "b.md"), None);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_or_a_put_with_nothing_to_embed_goes_on_when_the_model_cannot_be_checked() {
    let (workspace, _) = with_an_updated_document();
    raw_collection(&workspace, "memo");
    let document = file(&workspace, "memo.md", "# Memo\n\nWords.\n");
    assert_success(&put(&workspace, "memo", "memo.md", &document, &[]));

    let mut update = workspace.executable_without_the_model();
    update.args(["--log-level", "warn", "update", "notes"]);
    let mut put_again = workspace.executable_without_the_model();
    put_again
        .args([
            "--log-level",
            "warn",
            "put",
            "memo",
            "memo.md",
            "--file-type",
            "md",
        ])
        .arg(&document);
    let outputs = [update.output().unwrap(), put_again.output().unwrap()];

    for output in &outputs {
        let stderr = stderr(output);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(
            line_with(
                &stderr,
                &[
                    "the dimension of the configured embedding model could not be checked",
                    "cause="
                ]
            )
            .is_some(),
            "{stderr}"
        );
    }
    assert!(stderr(&outputs[0]).contains(&updated("notes", [0, 0, 1, 0, 0, 0])));
    assert!(stderr(&outputs[1]).contains("Skipped \"memo.md\""));
}

#[test]
#[ignore = "runs a real embedding model, not the default one; run it with `just integration`"]
fn an_update_and_a_put_warn_of_chunks_longer_than_the_model_context_length() {
    let workspace = Workspace::with_real_model();
    // A model whose context length, 256 tokens, is shorter than a chunk can
    // be; no chunk is longer than the default model's.
    assert_success(&workspace.run(&[
        "config",
        "set",
        "embedding.model",
        "sentence-transformers/all-MiniLM-L6-v2",
    ]));
    // For this model, each `a` and each `,` is one token: the paragraph, of
    // 600 characters, is 400 tokens.
    let long = format!("# Long\n\n{}\n", "a, ".repeat(200));
    let documents = directory_collection(
        &workspace,
        "notes",
        &[
            ("long.md", long.as_str()),
            ("short.md", "# Short\n\nWords.\n"),
        ],
    );
    raw_collection(&workspace, "memo");
    let document = text(&file(&workspace, "long.md", &long));
    let update = ["--log-level", "warn", "update", "notes"];
    let put_long = [
        "--log-level",
        "warn",
        "put",
        "memo",
        "long.md",
        "--file-type",
        "md",
        &document,
    ];

    // The second update and the second put skip every document.
    let outputs = [
        workspace.run_executable(&update),
        workspace.run_executable(&put_long),
        workspace.run_executable(&update),
        workspace.run_executable(&put_long),
    ];
    std::fs::rename(&documents, workspace.dir("moved").join("notes")).unwrap();
    let source_unreadable = workspace.run_executable(&update);

    for (output, collection) in outputs.iter().zip(["notes", "memo", "notes", "memo"]) {
        let stderr = stderr(output);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(
            line_with(
                &stderr,
                &[
                    TRUNCATED_CHUNKS,
                    &format!("collection={collection}"),
                    "chunks=1",
                    "mdya embedding rebuild"
                ]
            )
            .is_some(),
            "{stderr}"
        );
    }
    let updated_again = stderr(&outputs[2]);
    assert!(
        updated_again.contains(&updated("notes", [0, 0, 2, 0, 0, 0])),
        "{updated_again}"
    );
    let put_again = stderr(&outputs[3]);
    assert!(
        put_again.contains("Skipped \"long.md\" in the collection \"memo\": it has not changed."),
        "{put_again}"
    );
    let stderr = stderr(&source_unreadable);
    assert_eq!(source_unreadable.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains(TRUNCATED_CHUNKS), "{stderr}");
}

/// The start of the warning about chunks longer than the embedding model's
/// context length.
const TRUNCATED_CHUNKS: &str = "some chunks are longer than the embedding model's context length";

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn an_update_ingests_every_document_with_any_parallelism() {
    let workspace = Workspace::with_real_model();
    directory_collection(
        &workspace,
        "notes",
        &[
            ("a.md", "# A\n\nWords.\n"),
            ("b.md", "# B\n\nWords.\n"),
            ("c.md", "# C\n\nWords.\n"),
        ],
    );

    let one_by_one = [
        workspace.run(&["config", "set", "resources.embedding_parallelism", "0"]),
        workspace.run(&["update", "notes"]),
    ];
    let above_the_cap = [
        workspace.run(&["config", "set", "resources.embedding_parallelism", "2000"]),
        workspace.run(&["update", "notes", "--force"]),
    ];

    for outcome in one_by_one.iter().chain(&above_the_cap) {
        assert_success(outcome);
    }
    assert!(
        one_by_one[1]
            .stderr
            .contains(&updated("notes", [3, 0, 0, 0, 0, 0])),
        "{:?}",
        one_by_one[1]
    );
    assert!(
        above_the_cap[1]
            .stderr
            .contains(&updated("notes", [0, 0, 0, 3, 0, 0])),
        "{:?}",
        above_the_cap[1]
    );
}
