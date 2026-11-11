//! Checks that the parts the integration tests share run mdya with the real
//! components, in the test's own process and as the executable.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helper below is not.
#![allow(clippy::unwrap_used)]

mod common;

use std::process::ExitCode;

use common::Workspace;

const DOCUMENT: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";

/// A workspace and a directory that holds the one document.
fn workspace_with_a_document() -> (Workspace, String) {
    let workspace = Workspace::new();
    let documents = workspace.dir("documents");
    std::fs::write(documents.join("release.md"), DOCUMENT).unwrap();
    let documents = documents.to_str().unwrap().to_owned();
    (workspace, documents)
}

#[test]
fn a_document_is_found_after_an_update_in_the_tests_own_process() {
    let (workspace, documents) = workspace_with_a_document();

    let added = workspace.run(&["collection", "add", "notes", &documents]);
    let updated = workspace.run(&["update", "notes"]);
    let found = workspace.run(&["search", "リリース", "--json"]);

    assert_eq!(added.code, ExitCode::SUCCESS, "{added:?}");
    assert_eq!(updated.code, ExitCode::SUCCESS, "{updated:?}");
    assert_eq!(found.code, ExitCode::SUCCESS, "{found:?}");
    assert!(found.stdout.contains("release.md"), "{found:?}");
}

#[test]
fn a_document_is_found_after_an_update_through_the_executable() {
    let (workspace, documents) = workspace_with_a_document();

    let added = workspace
        .executable()
        .args(["collection", "add", "notes", &documents])
        .output()
        .unwrap();
    let updated = workspace
        .executable()
        .args(["update", "notes"])
        .output()
        .unwrap();
    let found = workspace
        .executable()
        .args(["search", "リリース", "--json"])
        .output()
        .unwrap();

    assert!(added.status.success(), "{added:?}");
    assert!(updated.status.success(), "{updated:?}");
    assert!(found.status.success(), "{found:?}");
    assert!(
        String::from_utf8_lossy(&found.stdout).contains("release.md"),
        "{found:?}"
    );
}
