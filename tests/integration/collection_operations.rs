//! The collection operations on the command line, with the real components:
//! making, listing, changing, and removing collections, including a
//! collection whose type this mdya does not know.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::common::Workspace;

/// `workspace`, and a directory in it that holds one Markdown document.
fn with_a_document(workspace: Workspace) -> (Workspace, String) {
    let documents = workspace.dir("documents");
    std::fs::write(documents.join("a.md"), "# A\n\nSome words.\n").unwrap();
    (workspace, text(&documents))
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// A file named `name` in `workspace`, with nothing in it.
fn empty_file(workspace: &Workspace, name: &str) -> String {
    let path = workspace.dir("files").join(name);
    std::fs::write(&path, "").unwrap();
    text(&path)
}

/// The collections, as `collection list --json` gives them.
fn listed(workspace: &Workspace) -> Vec<Value> {
    let listed = workspace.run(&["collection", "list", "--json"]);
    assert_eq!(listed.code, ExitCode::SUCCESS, "{listed:?}");
    let listed: Value = serde_json::from_str(&listed.stdout).unwrap();
    listed["collections"].as_array().unwrap().clone()
}

fn names(collections: &[Value]) -> Vec<&str> {
    collections
        .iter()
        .map(|collection| collection["name"].as_str().unwrap())
        .collect()
}

/// The one listed collection named `name`.
fn listed_one(workspace: &Workspace, name: &str) -> Value {
    let collections = listed(workspace);
    let found: Vec<&Value> = collections
        .iter()
        .filter(|collection| collection["name"] == name)
        .collect();
    assert_eq!(found.len(), 1, "{collections:?}");
    found[0].clone()
}

fn exit_code(code: u8) -> ExitCode {
    ExitCode::from(code)
}

#[test]
fn collections_are_made_changed_listed_and_removed() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let archive = empty_file(&workspace, "a.zip");

    let made = [
        workspace.run(&[
            "collection",
            "add",
            "notes",
            &documents,
            "--description",
            "memo",
        ]),
        workspace.run(&["collection", "add", "arch", "--type", "archive", &archive]),
        workspace.run(&["collection", "add", "memo", "--type", "raw"]),
        workspace.run(&["collection", "set", "notes", "--no-description"]),
        workspace.run(&["collection", "remove", "arch", "--yes"]),
    ];
    let collections = listed(&workspace);

    for outcome in &made {
        assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
    }
    let expected = json!([
        { "name": "memo", "type": "raw", "description": null, "document_count": 0,
          "attributes": {} },
        { "name": "notes", "type": "directory", "description": null, "document_count": 0,
          "attributes": { "path": documents } },
    ]);
    assert_eq!(Value::Array(collections), expected);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn making_a_collection_tells_what_was_made_on_stderr_only() {
    let (workspace, documents) = with_a_document(Workspace::new());

    let made = workspace.run(&[
        "collection",
        "add",
        "notes",
        &documents,
        "--description",
        "memo",
    ]);

    assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
    assert_eq!(made.stdout, "", "{made:?}");
    for told in ["notes", "directory", &documents, "memo"] {
        assert!(made.stderr.contains(told), "{told}: {made:?}");
    }
    assert_eq!(listed_one(&workspace, "notes")["document_count"], 0);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_collection_made_without_a_type_is_a_directory_collection() {
    let (workspace, documents) = with_a_document(Workspace::new());

    let made = workspace.run(&["collection", "add", "notes", &documents]);

    assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
    assert_eq!(listed_one(&workspace, "notes")["type"], "directory");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn an_archive_collection_takes_a_zip_a_tar_or_a_gzipped_tar() {
    let workspace = Workspace::new();
    let archives = ["a.zip", "b.tar", "c.tar.gz", "d.tgz", "E.ZIP"];

    for (number, archive) in archives.iter().enumerate() {
        let name = format!("arch{number}");
        let archive = empty_file(&workspace, archive);
        let made = workspace.run(&["collection", "add", &name, "--type", "archive", &archive]);
        assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
        assert_eq!(listed_one(&workspace, &name)["type"], "archive");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_collection_name_follows_the_naming_rules() {
    let workspace = Workspace::new();
    let longest = format!("{}-_", "a".repeat(62));
    let too_long = "a".repeat(65);

    let accepted = workspace.run(&["collection", "add", &longest, "--type", "raw"]);
    let refused: Vec<_> = ["", &too_long, "bad!", "a.b", "a b", "メモ"]
        .into_iter()
        .map(|name| workspace.run(&["collection", "add", name, "--type", "raw"]))
        .collect();

    assert_eq!(accepted.code, ExitCode::SUCCESS, "{accepted:?}");
    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(2), "{outcome:?}");
    }
    assert_eq!(names(&listed(&workspace)), [longest.as_str()]);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn collection_names_that_differ_only_in_case_are_different_collections() {
    let workspace = Workspace::new();

    let lower = workspace.run(&["collection", "add", "notes", "--type", "raw"]);
    let upper = workspace.run(&["collection", "add", "Notes", "--type", "raw"]);

    assert_eq!(lower.code, ExitCode::SUCCESS, "{lower:?}");
    assert_eq!(upper.code, ExitCode::SUCCESS, "{upper:?}");
    assert_eq!(names(&listed(&workspace)), ["Notes", "notes"]);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn making_a_collection_with_a_taken_name_fails_with_exit_code_1() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let other = text(&workspace.dir("other"));
    workspace.run(&["collection", "add", "notes", &documents]);

    let made = workspace.run(&["collection", "add", "notes", &other]);

    assert_eq!(made.code, exit_code(1), "{made:?}");
    assert_eq!(made.stdout, "", "{made:?}");
    assert_eq!(
        listed_one(&workspace, "notes")["attributes"]["path"],
        documents.as_str()
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn missing_or_unexpected_type_specific_attributes_fail_with_exit_code_2() {
    let (workspace, documents) = with_a_document(Workspace::new());

    let refused = [
        workspace.run(&["collection", "add", "notes"]),
        workspace.run(&["collection", "add", "arch", "--type", "archive"]),
        workspace.run(&["collection", "add", "memo", "--type", "raw", &documents]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(2), "{outcome:?}");
    }
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn making_a_collection_on_a_location_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    let missing = text(&workspace.dir("here").join("missing"));

    let refused = [
        workspace.run(&["collection", "add", "notes", &missing]),
        workspace.run(&[
            "collection",
            "add",
            "arch",
            "--type",
            "archive",
            &format!("{missing}.zip"),
        ]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
    }
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn making_a_collection_on_a_location_that_does_not_fit_the_type_fails_with_exit_code_1() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let not_an_archive = empty_file(&workspace, "a.txt");
    let archive = empty_file(&workspace, "a.zip");

    let refused = [
        workspace.run(&["collection", "add", "notes", &archive]),
        workspace.run(&["collection", "add", "arch", "--type", "archive", &documents]),
        workspace.run(&[
            "collection",
            "add",
            "arch",
            "--type",
            "archive",
            &not_an_archive,
        ]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
    }
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_relative_location_is_kept_as_an_absolute_path_from_the_working_directory() {
    let workspace = Workspace::new();
    let here = workspace.dir("here");
    std::fs::create_dir(here.join("docs")).unwrap();

    let made =
        workspace.run_executable_in(&here, &["collection", "add", "notes", "./docs/../docs"]);

    assert!(made.status.success(), "{made:?}");
    // The working directory the executable sees has no symbolic links in it.
    let expected = std::fs::canonicalize(&here)
        .unwrap()
        .join("docs")
        .join("..")
        .join("docs");
    assert_eq!(
        listed_one(&workspace, "notes")["attributes"]["path"],
        text(&expected)
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_location_of_a_tilde_is_kept_as_the_home_directory() {
    let workspace = Workspace::new();

    let made = workspace.run(&["collection", "add", "home", "~"]);

    assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
    let home = std::env::home_dir().unwrap();
    assert_eq!(
        listed_one(&workspace, "home")["attributes"]["path"],
        text(&home)
    );
}

#[cfg(unix)]
#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_location_through_a_symbolic_link_is_kept_as_given() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let link = workspace.dir("links").join("documents");
    std::os::unix::fs::symlink(&documents, &link).unwrap();

    let made = workspace.run(&["collection", "add", "notes", &text(&link)]);

    assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
    assert_eq!(
        listed_one(&workspace, "notes")["attributes"]["path"],
        text(&link)
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_without_a_terminal_or_yes_fails_with_exit_code_2() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let removed = workspace.run(&["collection", "remove", "memo"]);

    assert_eq!(removed.code, exit_code(2), "{removed:?}");
    assert!(removed.stderr.contains("--yes"), "{removed:?}");
    assert_eq!(names(&listed(&workspace)), ["memo"]);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_asks_and_keeps_it_when_the_user_declines() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let removed = workspace.run_answering(&["collection", "remove", "memo"], "n\n");

    assert_eq!(removed.code, exit_code(1), "{removed:?}");
    assert!(removed.stderr.contains("[y/N]"), "{removed:?}");
    assert_eq!(removed.stdout, "", "{removed:?}");
    assert_eq!(names(&listed(&workspace)), ["memo"]);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_asks_and_removes_it_when_the_user_agrees() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let removed = workspace.run_answering(&["collection", "remove", "memo"], "y\n");

    assert_eq!(removed.code, ExitCode::SUCCESS, "{removed:?}");
    assert!(removed.stderr.contains("[y/N]"), "{removed:?}");
    assert!(removed.stderr.contains("memo"), "{removed:?}");
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_without_the_final_maintenance_removes_it() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let removed = workspace.run(&["collection", "remove", "memo", "--yes", "--no-optimize"]);

    assert_eq!(removed.code, ExitCode::SUCCESS, "{removed:?}");
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_leaves_its_location_alone() {
    let (workspace, documents) = with_a_document(Workspace::new());
    workspace.run(&["collection", "add", "notes", &documents]);

    let removed = workspace.run(&["collection", "remove", "notes", "--yes"]);

    assert_eq!(removed.code, ExitCode::SUCCESS, "{removed:?}");
    assert!(Path::new(&documents).join("a.md").is_file());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_collection_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();

    let removed = workspace.run(&["collection", "remove", "ghost", "--yes"]);

    assert_eq!(removed.code, exit_code(1), "{removed:?}");
    assert!(removed.stderr.contains("ghost"), "{removed:?}");
}

/// An error that the arguments alone show is found before one that needs
/// the collections to be read.
#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_missing_yes_is_found_before_a_missing_collection() {
    let workspace = Workspace::new();

    let removed = workspace.run(&["collection", "remove", "ghost"]);

    assert_eq!(removed.code, exit_code(2), "{removed:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn collections_are_listed_in_the_order_of_their_names_code_points() {
    let workspace = Workspace::new();
    for name in ["b", "B", "_c", "-d", "a1", "A"] {
        // `--` keeps `-d` from being read as an option.
        let made = workspace.run(&["collection", "add", "--type", "raw", "--", name]);
        assert_eq!(made.code, ExitCode::SUCCESS, "{made:?}");
    }

    let collections = listed(&workspace);
    let human = workspace.run(&["collection", "list"]);

    let expected = ["-d", "A", "B", "_c", "a1", "b"];
    assert_eq!(names(&collections), expected);
    let positions: Vec<usize> = expected
        .iter()
        .map(|name| human.stdout.find(&format!("{name} (")).unwrap())
        .collect();
    assert!(positions.is_sorted(), "{human:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_list_in_the_human_format_shows_each_collection() {
    let (workspace, documents) = with_a_document(Workspace::new());
    workspace.run(&[
        "collection",
        "add",
        "notes",
        &documents,
        "--description",
        "memo",
    ]);
    workspace.run(&["collection", "add", "pasted", "--type", "raw"]);

    let human = workspace.run(&["collection", "list"]);

    assert_eq!(human.code, ExitCode::SUCCESS, "{human:?}");
    for shown in ["notes", "directory", &documents, "memo", "pasted", "raw"] {
        assert!(human.stdout.contains(shown), "{shown}: {human:?}");
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_description_is_changed_and_removed() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let described = workspace.run(&["collection", "set", "memo", "--description", "pasted"]);
    let description = listed_one(&workspace, "memo")["description"].clone();
    let undescribed = workspace.run(&["collection", "set", "memo", "--no-description"]);

    assert_eq!(described.code, ExitCode::SUCCESS, "{described:?}");
    assert!(described.stderr.contains("pasted"), "{described:?}");
    assert_eq!(description, "pasted");
    assert_eq!(undescribed.code, ExitCode::SUCCESS, "{undescribed:?}");
    assert_eq!(listed_one(&workspace, "memo")["description"], Value::Null);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_location_is_changed() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let other = text(&workspace.dir("other"));
    workspace.run(&["collection", "add", "notes", &documents]);

    let changed = workspace.run(&["collection", "set", "notes", &other]);

    assert_eq!(changed.code, ExitCode::SUCCESS, "{changed:?}");
    assert!(changed.stderr.contains(&other), "{changed:?}");
    assert_eq!(
        listed_one(&workspace, "notes")["attributes"]["path"],
        other.as_str()
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn changing_a_collection_that_does_not_exist_fails_with_exit_code_1() {
    let workspace = Workspace::new();

    let changed = workspace.run(&["collection", "set", "ghost", "--description", "x"]);

    assert_eq!(changed.code, exit_code(1), "{changed:?}");
    assert!(changed.stderr.contains("ghost"), "{changed:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn changing_nothing_fails_with_exit_code_2() {
    let workspace = Workspace::new();
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let changed = workspace.run(&["collection", "set", "memo"]);

    assert_eq!(changed.code, exit_code(2), "{changed:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_location_for_a_raw_collection_fails_with_exit_code_1() {
    let (workspace, documents) = with_a_document(Workspace::new());
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let changed = workspace.run(&["collection", "set", "memo", &documents]);

    assert_eq!(changed.code, exit_code(1), "{changed:?}");
    assert_eq!(listed_one(&workspace, "memo")["attributes"], json!({}));
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_new_location_that_does_not_exist_or_does_not_fit_fails_with_exit_code_1() {
    let (workspace, documents) = with_a_document(Workspace::new());
    let missing = text(&workspace.dir("here").join("missing"));
    let file = empty_file(&workspace, "a.txt");
    workspace.run(&["collection", "add", "notes", &documents]);

    let refused = [
        workspace.run(&["collection", "set", "notes", &missing]),
        workspace.run(&["collection", "set", "notes", &file]),
    ];

    for outcome in &refused {
        assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
    }
    assert_eq!(
        listed_one(&workspace, "notes")["attributes"]["path"],
        documents.as_str()
    );
}

/// A workspace with the collection `later`, of the type `future` that this
/// mdya does not know.
fn with_a_collection_of_an_unknown_type() -> Workspace {
    let workspace = Workspace::new();
    workspace.add_collection_of_unknown_type("later", "future", json!({ "source": "somewhere" }));
    workspace
}

/// Checks that `outcome` is the error for the collection `later` of the type
/// `future`.
fn assert_unknown_type_error(outcome: &crate::common::Outcome) {
    assert_eq!(outcome.code, exit_code(1), "{outcome:?}");
    assert!(outcome.stderr.contains("later"), "{outcome:?}");
    assert!(outcome.stderr.contains("future"), "{outcome:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_collection_of_an_unknown_type_is_listed_described_and_removed() {
    let workspace = with_a_collection_of_an_unknown_type();

    let collection = listed_one(&workspace, "later");
    let described = workspace.run(&["collection", "set", "later", "--description", "kept"]);
    let description = listed_one(&workspace, "later")["description"].clone();
    let removed = workspace.run(&["collection", "remove", "later", "--yes"]);

    assert_eq!(collection["type"], "future");
    assert_eq!(collection["attributes"], json!({ "source": "somewhere" }));
    assert_eq!(described.code, ExitCode::SUCCESS, "{described:?}");
    assert_eq!(description, "kept");
    assert_eq!(removed.code, ExitCode::SUCCESS, "{removed:?}");
    assert_eq!(listed(&workspace), Vec::<Value>::new());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_new_location_for_a_collection_of_an_unknown_type_fails_with_exit_code_1() {
    let workspace = with_a_collection_of_an_unknown_type();
    let other = text(&workspace.dir("other"));

    let changed = workspace.run(&["collection", "set", "later", &other]);

    assert_unknown_type_error(&changed);
    assert_eq!(
        listed_one(&workspace, "later")["attributes"],
        json!({ "source": "somewhere" })
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn updating_a_collection_of_an_unknown_type_fails_with_exit_code_1() {
    let workspace = with_a_collection_of_an_unknown_type();

    let updated = workspace.run(&["update", "later"]);

    assert_unknown_type_error(&updated);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn updating_all_collections_fails_on_one_of_an_unknown_type_and_updates_none() {
    let workspace = with_a_collection_of_an_unknown_type();
    let (workspace, documents) = with_a_document(workspace);
    // `early` comes before `later` in the order of the names, so an update
    // that went through the collections in that order would reach it first.
    workspace.run(&["collection", "add", "early", &documents]);

    let updated = workspace.run(&["update", "--all"]);

    assert_unknown_type_error(&updated);
    assert_eq!(listed_one(&workspace, "early")["document_count"], 0);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn putting_a_document_into_a_collection_of_an_unknown_type_fails_with_exit_code_1() {
    let workspace = with_a_collection_of_an_unknown_type();
    let document = workspace.dir("files").join("a.md");
    std::fs::write(&document, "# A\n").unwrap();

    let put = workspace.run(&[
        "put",
        "later",
        "a.md",
        "--file-type",
        "md",
        &text(&document),
    ]);

    assert_unknown_type_error(&put);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn removing_a_document_of_a_collection_of_an_unknown_type_fails_without_asking() {
    let workspace = with_a_collection_of_an_unknown_type();

    let removed = workspace.run_answering(&["remove", "later", "a.md"], "y\n");

    assert_unknown_type_error(&removed);
    assert!(!removed.stderr.contains("[y/N]"), "{removed:?}");
}

/// Searching and getting a document do not look at the collection type.
#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn searching_and_getting_in_a_collection_of_an_unknown_type_do_not_fail_on_its_type() {
    let workspace = with_a_collection_of_an_unknown_type();

    let searched = workspace.run(&["search", "words", "--mode", "fts", "-c", "later"]);
    let got = workspace.run(&["get", "later", "a.md"]);

    assert_eq!(searched.code, ExitCode::SUCCESS, "{searched:?}");
    assert_eq!(got.code, exit_code(1), "{got:?}");
    assert!(got.stderr.contains("a.md"), "{got:?}");
    assert!(!got.stderr.contains("future"), "{got:?}");
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn removing_a_collection_removes_its_documents_from_the_index() {
    let (workspace, documents) = with_a_document(Workspace::with_real_model());
    workspace.run(&["collection", "add", "notes", &documents]);
    let updated = workspace.run(&["update", "notes"]);
    let before = status(&workspace);

    let removed = workspace.run(&["collection", "remove", "notes", "--yes"]);
    let after = status(&workspace);

    assert_eq!(updated.code, ExitCode::SUCCESS, "{updated:?}");
    assert_eq!(before["document_count"], 1, "{before}");
    assert!(before["chunk_count"].as_u64().unwrap() > 0, "{before}");
    assert_eq!(removed.code, ExitCode::SUCCESS, "{removed:?}");
    assert_eq!(after["document_count"], 0, "{after}");
    assert_eq!(after["chunk_count"], 0, "{after}");
    assert!(Path::new(&documents).join("a.md").is_file());
}

/// The state of the index, as `status --json` gives it.
fn status(workspace: &Workspace) -> Value {
    let status = workspace.run(&["status", "--json"]);
    assert_eq!(status.code, ExitCode::SUCCESS, "{status:?}");
    serde_json::from_str(&status.stdout).unwrap()
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn the_list_counts_the_documents_of_each_collection() {
    let (workspace, documents) = with_a_document(Workspace::with_real_model());
    std::fs::write(Path::new(&documents).join("b.md"), "# B\n").unwrap();
    workspace.run(&["collection", "add", "notes", &documents]);
    workspace.run(&["collection", "add", "memo", "--type", "raw"]);

    let updated = workspace.run(&["update", "notes"]);

    assert_eq!(updated.code, ExitCode::SUCCESS, "{updated:?}");
    assert_eq!(listed_one(&workspace, "notes")["document_count"], 2);
    assert_eq!(listed_one(&workspace, "memo")["document_count"], 0);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_changed_location_is_read_at_the_next_update() {
    let (workspace, documents) = with_a_document(Workspace::with_real_model());
    let other = text(&workspace.dir("other"));
    workspace.run(&["collection", "add", "notes", &documents]);
    workspace.run(&["update", "notes"]);

    let changed = workspace.run(&["collection", "set", "notes", &other]);
    let count_after_change = listed_one(&workspace, "notes")["document_count"].clone();
    let updated = workspace.run(&["update", "notes"]);

    assert_eq!(changed.code, ExitCode::SUCCESS, "{changed:?}");
    assert_eq!(count_after_change, 1);
    assert_eq!(updated.code, ExitCode::SUCCESS, "{updated:?}");
    assert_eq!(listed_one(&workspace, "notes")["document_count"], 0);
}
