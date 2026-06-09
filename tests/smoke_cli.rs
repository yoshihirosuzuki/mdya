use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_subcommand_prints_package_version() {
    Command::cargo_bin("mdya")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .stdout(contains(format!("mdya v{}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn version_flag_prints_package_version() {
    Command::cargo_bin("mdya")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn update_all_subcommand_help_lists_global_flags() {
    // Light E2E: exercises `clap` derive + subcommand wire-up without
    // touching the embedder or the DB. Full ingest-path E2E lives in
    // `tests/e2e_update_all.rs` behind `#[ignore]` because it downloads
    // the real model.
    let assert = Command::cargo_bin("mdya")
        .unwrap()
        .args(["update-all", "--help"])
        .assert()
        .success();
    assert
        // Subcommand-specific description, fixes the assertion to a string
        // owned by `update-all` rather than the global help block.
        .stdout(contains("Walk every registered collection"))
        .stdout(contains("--config-dir"));
}
