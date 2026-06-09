//! Full end-to-end smoke for `mdya update-all`.
//!
//! Marked `#[ignore]` because it downloads the real `cl-nagoya/ruri-v3-30m`
//! model from HuggingFace Hub on first run (~140 MB). Run manually via
//! `just update-all-e2e`; not part of `just check`.
//!
//! NOTE: the `--config-dir` points at a fresh `TempDir`, so
//! `~/.mdya/models/` lives inside it and the model is **re-downloaded
//! every test run**. The same pattern is used by `tests/e2e_embed_real.rs`
//! to keep the user's home cache untouched. If interactive iteration
//! becomes painful, point `--config-dir` at a stable path manually.
//!
//! The smoke covers: `init` → `collection add` → write 1 markdown file
//! → `update-all` → assert exit 0 and the documented stdout summary.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn mdya() -> Command {
    Command::cargo_bin("mdya").expect("binary built")
}

#[test]
#[ignore = "downloads ~140 MB model; run with `just update-all-e2e`"]
fn update_all_indexes_one_markdown_file_end_to_end() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let coll_dir = base.join("notes");
    fs::create_dir(&coll_dir).expect("mkdir notes");
    fs::write(
        coll_dir.join("hello.md"),
        "# Hello\n\nThis is a tiny end-to-end smoke for `mdya update-all`.\n",
    )
    .expect("write hello.md");

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .arg("init")
        .assert()
        .success();

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .args(["collection", "add"])
        .arg(&coll_dir)
        .assert()
        .success();

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .arg("update-all")
        .assert()
        .success()
        .stdout(predicate::str::contains("Indexed 1 documents"))
        .stdout(predicate::str::contains("new: 1"));
}
