//! Checks that the parts the integration tests share run mdya with the real
//! components, in the test's own process and as the executable.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helper below is not.
#![allow(clippy::unwrap_used)]

use std::process::ExitCode;

use crate::common::Workspace;

const DOCUMENT: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";

/// `workspace`, and a directory in it that holds the one document.
fn with_a_document(workspace: Workspace) -> (Workspace, String) {
    let documents = workspace.dir("documents");
    std::fs::write(documents.join("release.md"), DOCUMENT).unwrap();
    let documents = documents.to_str().unwrap().to_owned();
    (workspace, documents)
}

#[test]
fn an_added_collection_is_listed_in_the_tests_own_process() {
    let (workspace, documents) = with_a_document(Workspace::new());

    let added = workspace.run(&["collection", "add", "notes", &documents]);
    let listed = workspace.run(&["collection", "list"]);

    assert_eq!(added.code, ExitCode::SUCCESS, "{added:?}");
    assert_eq!(listed.code, ExitCode::SUCCESS, "{listed:?}");
    assert!(listed.stdout.contains("notes"), "{listed:?}");
}

#[test]
fn an_added_collection_is_listed_through_the_executable() {
    let (workspace, documents) = with_a_document(Workspace::new());

    let added = workspace.run_executable(&["collection", "add", "notes", &documents]);
    let listed = workspace.run_executable(&["collection", "list"]);

    assert!(added.status.success(), "{added:?}");
    assert!(listed.status.success(), "{listed:?}");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("notes"),
        "{listed:?}"
    );
}

/// `status` learns the embedding model's dimension, so it prepares the model,
/// and goes on with exit code 0 when the model cannot be fetched. The check
/// that a test did not prepare the model must see that preparation.
#[test]
fn a_preparation_of_the_model_that_fails_is_seen() {
    let workspace = Workspace::new();

    let status = workspace
        .executable_that_cannot_fetch()
        .arg("status")
        .output()
        .unwrap();

    assert!(status.status.success(), "{status:?}");
    assert!(workspace.model_was_prepared(), "{status:?}");
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_is_found_after_an_update_in_the_tests_own_process() {
    let (workspace, documents) = with_a_document(Workspace::with_real_model());

    let added = workspace.run(&["collection", "add", "notes", &documents]);
    let updated = workspace.run(&["update", "notes"]);
    let found = workspace.run(&["search", "リリース", "--json"]);

    assert_eq!(added.code, ExitCode::SUCCESS, "{added:?}");
    assert_eq!(updated.code, ExitCode::SUCCESS, "{updated:?}");
    assert_eq!(found.code, ExitCode::SUCCESS, "{found:?}");
    assert!(found.stdout.contains("release.md"), "{found:?}");
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_document_is_found_after_an_update_through_the_executable() {
    let (workspace, documents) = with_a_document(Workspace::with_real_model());

    let added = workspace.run_executable(&["collection", "add", "notes", &documents]);
    let updated = workspace.run_executable(&["update", "notes"]);
    let found = workspace.run_executable(&["search", "リリース", "--json"]);

    assert!(added.status.success(), "{added:?}");
    assert!(updated.status.success(), "{updated:?}");
    assert!(found.status.success(), "{found:?}");
    assert!(
        String::from_utf8_lossy(&found.stdout).contains("release.md"),
        "{found:?}"
    );
}
