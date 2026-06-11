//! Smoke tests for `mdya init` + `mdya collection add`.
//!
//! Exercises the real binary via `assert_cmd`, isolated in a `TempDir` so
//! the user's `~/.mdya/` is never touched. Covers the contract:
//! - idempotency: re-running `mdya init` is a no-op.
//! - path validation: missing / non-directory paths fail with non-zero exit.
//! - duplicate detection: re-adding the same collection name fails.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn mdya() -> Command {
    Command::cargo_bin("mdya").expect("binary built")
}

#[test]
fn init_materializes_config_index_and_lance_models() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .arg("init")
        .assert()
        .success();

    assert!(base.join("config.yml").is_file());
    assert!(base.join("index").is_dir());
    // The embedding-model cache lives under `--model-cache-dir`
    // (default `~/.mdya-models/`), not `<config_dir>/models/`. `mdya init`
    // therefore must NOT materialize `<config_dir>/models/` — that
    // directory is created lazily by `ModelCache::new` on first model
    // load.
    assert!(
        !base.join("models").exists(),
        "init must not create <config_dir>/models/ anymore",
    );
    assert!(base.join("index").join("chunks.lance").exists());
    // `mdya init` pre-warms the lindera/ipadic config.yml under
    // `<base>/lance-models/` so the first `update-all` skips the
    // bootstrap step. The redirect target Lance reads at FTS
    // `create_index` time lives here, not in the OS default cache dir.
    assert!(base.join("lance-models").is_dir());
    let ipadic_config = base
        .join("lance-models")
        .join("lindera")
        .join("ipadic")
        .join("config.yml");
    assert!(
        ipadic_config.is_file(),
        "lindera/ipadic config.yml not materialized at {}",
        ipadic_config.display()
    );
    let ipadic_yaml = fs::read_to_string(&ipadic_config).expect("read ipadic config");
    assert!(
        ipadic_yaml.contains("embedded://ipadic"),
        "lindera/ipadic config.yml must select the embedded dictionary, got:\n{ipadic_yaml}"
    );

    let yaml = fs::read_to_string(base.join("config.yml")).expect("read config");
    assert!(
        yaml.contains("cl-nagoya/ruri-v3-30m"),
        "config.yml should embed default model, got:\n{yaml}"
    );
}

#[test]
fn init_is_idempotent_and_does_not_overwrite_user_edits() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .arg("init")
        .assert()
        .success();

    // Simulate the user editing config.yml after the first init.
    let user_edit = "collections:\n  custom:\n    path: ~/custom\n";
    fs::write(base.join("config.yml"), user_edit).expect("write");

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .arg("init")
        .assert()
        .success();

    let after = fs::read_to_string(base.join("config.yml")).expect("read");
    assert_eq!(after, user_edit);
}

#[test]
fn collection_add_writes_entry_with_basename_default() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let collection_dir = base.join("notes-source");
    fs::create_dir(&collection_dir).expect("mkdir collection");

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
        .arg(&collection_dir)
        .assert()
        .success();

    let cfg = fs::read_to_string(base.join("config.yml")).expect("read");
    assert!(cfg.contains("notes-source"), "config.yml content:\n{cfg}");
}

#[test]
fn collection_add_with_missing_path_fails_with_non_zero_exit() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let missing = base.join("does-not-exist");

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
        .arg(&missing)
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn collection_add_with_name_override_uses_override() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let collection_dir = base.join("notes-source");
    fs::create_dir(&collection_dir).expect("mkdir collection");

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
        .arg(&collection_dir)
        .args(["--name", "work-notes"])
        .assert()
        .success();

    let cfg = fs::read_to_string(base.join("config.yml")).expect("read");
    assert!(cfg.contains("work-notes"));
    // basename should NOT have been used as a fallback alongside override.
    // Parse through the strong-typed `Config` schema rather than a
    // dynamic YAML `Value`: `serde_saphyr` is a typed-only deserializer
    // (it deliberately omits the intermediate DOM that `serde_yaml`'s
    // `Value` exposed), and the typed lookup is more precise anyway.
    let parsed: mdya::config::Config = serde_saphyr::from_str(&cfg).expect("parse");
    assert!(parsed.collections.contains_key("work-notes"));
    assert!(!parsed.collections.contains_key("notes-source"));
}

#[test]
fn collection_add_with_duplicate_name_fails() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let collection_dir = base.join("notes");
    fs::create_dir(&collection_dir).expect("mkdir collection");

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
        .arg(&collection_dir)
        .assert()
        .success();

    mdya()
        .args(["--config-dir"])
        .arg(base)
        .args(["collection", "add"])
        .arg(&collection_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}
