//! Integration tests for the runtime memory guard.
//!
//! Each test spawns the real `mdya` binary inside an isolated `TempDir`
//! `--config-dir` so the user's `~/.mdya/` is untouched. The hidden
//! `mdya stress allocate <mb>` subcommand provides a deterministic way to
//! force RSS over the cap; in production code the same allocation would
//! come from `update-all` / `search hybrid` / the MCP stdio server.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const GUARD_EXIT_CODE: i32 = 137;

fn mdya() -> Command {
    Command::cargo_bin("mdya").expect("binary built")
}

fn write_config(base: &Path, memory_limit_mb: u64) {
    // The chunking section was dropped from the YAML schema, so the
    // fixture mirrors the current minimal 3-section shape.
    let yaml = format!(
        "\
collections: {{}}
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: {memory_limit_mb}
"
    );
    fs::write(base.join("config.yml"), yaml).expect("write config.yml");
}

#[test]
fn stress_allocate_above_cap_exits_with_137_and_logs_one_line() {
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    write_config(base, 64);

    let assert = mdya()
        .args(["--config-dir"])
        .arg(base)
        .args(["stress", "allocate", "512"])
        .timeout(std::time::Duration::from_secs(15))
        .assert();

    assert
        .code(GUARD_EXIT_CODE)
        .stderr(predicate::str::contains("memory limit exceeded"))
        .stderr(predicate::str::contains("self-terminating"));
}

#[test]
fn memory_limit_mb_zero_disables_the_guard() {
    // With the watchdog disabled, an allocation that would otherwise trip the
    // 64 MB cap must run unhindered. We bound execution at 3 seconds so the
    // stress subcommand's infinite sleep does not hang the test runner — a
    // timeout here proves the guard never fired (it kills in < 250 ms).
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    write_config(base, 0);

    let outcome = mdya()
        .args(["--config-dir"])
        .arg(base)
        .args(["stress", "allocate", "128"])
        .timeout(std::time::Duration::from_secs(3))
        .assert();

    // On Unix, `assert_cmd` delivers SIGKILL on timeout, so the child reports
    // termination by signal — `ExitStatus::code()` then returns `None`, not
    // 137. The guard, had it fired, would have produced `Some(137)` well
    // within the 3-second window (poll interval is 250 ms).
    let exit = outcome.get_output().status.code();
    assert_ne!(
        exit,
        Some(GUARD_EXIT_CODE),
        "guard fired despite memory_limit_mb=0; exit code was {exit:?}"
    );
}

#[test]
fn missing_runtime_section_applies_8192_default_and_lets_small_allocation_pass() {
    // A config.yml without a `runtime:` key falls back to the struct-level
    // default (8192 MB), so a 32 MB allocation must NOT trip the guard —
    // proving the silent migration path keeps existing users working out
    // of the box. We additionally include a legacy `chunking:` section so
    // the test covers the "unknown keys are ignored" contract for older
    // config.yml files.
    let tmp = TempDir::new().expect("tempdir");
    let base = tmp.path();
    let yaml = "\
collections: {}
embedding:
  model: cl-nagoya/ruri-v3-30m
chunking:
  strategy: fixed-window-by-bytes
  params:
    window_size: 512
    overlap: 64
";
    fs::write(base.join("config.yml"), yaml).expect("write config.yml");

    let outcome = mdya()
        .args(["--config-dir"])
        .arg(base)
        .args(["stress", "allocate", "32"])
        .timeout(std::time::Duration::from_secs(3))
        .assert();

    // See the comment in `memory_limit_mb_zero_disables_the_guard` for why
    // SIGKILL-on-timeout produces `None` rather than 137. The negative
    // assertion remains "guard did not produce its specific exit code".
    let exit = outcome.get_output().status.code();
    assert_ne!(
        exit,
        Some(GUARD_EXIT_CODE),
        "guard fired on 32 MB allocation with default 8192 cap; exit was {exit:?}"
    );
}
