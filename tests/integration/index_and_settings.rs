//! The state of the index, the settings, rebuilding the embeddings, and
//! maintaining the index on the command line, with the real components, and
//! the errors of these operations.
//!
//! In the test's own process, the warnings that mdya logs go nowhere (see the
//! module `common`). The test that checks a warning runs the executable.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{ExitCode, Output};

use serde_json::{Value, json};

#[cfg(unix)]
use crate::common::Locked;
use crate::common::{Outcome, Workspace};

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// The version of mdya under test.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The default embedding model, whose embeddings have 256 dimensions.
const DEFAULT_MODEL: &str = "cl-nagoya/ruri-v3-30m";

/// A model whose context length, 256 tokens, is shorter than the default
/// model's, and whose embeddings have 384 dimensions.
const SECOND_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2";

/// Makes the directory collection `notes` of two documents, of three chunks
/// in all, and updates it with `update_options`.
fn with_notes(workspace: &Workspace, update_options: &[&str]) {
    let dir = workspace.dir("notes");
    std::fs::write(
        dir.join("a.md"),
        "# A\n\nFirst words.\n\n# A2\n\nSecond part.\n",
    )
    .unwrap();
    std::fs::write(dir.join("b.md"), "# B\n\nOther words.\n").unwrap();
    assert_success(&workspace.run(&["collection", "add", "notes", &text(&dir)]));
    let mut update = vec!["update", "notes"];
    update.extend_from_slice(update_options);
    assert_success(&workspace.run(&update));
}

/// Makes the raw collection `name`, and puts `documents`, Markdown, into it
/// under their keys.
fn raw_collection(workspace: &Workspace, name: &str, documents: &[(&str, &str)]) {
    assert_success(&workspace.run(&["collection", "add", name, "--type", "raw"]));
    for &(key, contents) in documents {
        assert_success(&workspace.run_with_stdin(
            &["put", name, key, "--file-type", "md", "-"],
            contents.as_bytes(),
        ));
    }
}

/// The result of `mdya <args> --json`, which must succeed.
fn json_of(workspace: &Workspace, args: &[&str]) -> Value {
    let mut with_json = args.to_vec();
    with_json.push("--json");
    let outcome = workspace.run(&with_json);
    assert_success(&outcome);
    serde_json::from_str(&outcome.stdout).unwrap()
}

fn status(workspace: &Workspace) -> Value {
    json_of(workspace, &["status"])
}

fn settings(workspace: &Workspace) -> Value {
    json_of(workspace, &["config", "list"])
}

/// The keys of the hits of a search for `words` in the search mode `mode`, in
/// their order.
fn found(workspace: &Workspace, mode: &str) -> Vec<String> {
    let result = json_of(workspace, &["search", "words", "--mode", mode]);
    result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["key"].as_str().unwrap().to_owned())
        .collect()
}

/// The names of the settings, in the order that the settings are listed.
const SETTING_NAMES: [&str; 7] = [
    "mcp.show_version",
    "embedding.model",
    "embedding.ollama_endpoint",
    "resources.memory_limit_mb",
    "resources.embedding_parallelism",
    "get.cli_limit_bytes",
    "get.mcp_limit_bytes",
];

/// The settings that no one has changed.
fn defaults() -> Value {
    json!({
        "mcp.show_version": true,
        "embedding.model": DEFAULT_MODEL,
        "embedding.ollama_endpoint": "http://127.0.0.1:11434",
        "resources.memory_limit_mb": 8192,
        "resources.embedding_parallelism": 8,
        "get.cli_limit_bytes": 1_048_576,
        "get.mcp_limit_bytes": 1_048_576,
    })
}

/// A setting's value as `mdya config set` takes it and as its messages write
/// it: a string without quotes.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// Asserts that `outcome` failed with exit code `code`, wrote nothing to
/// stdout, and wrote `message` to stderr.
fn assert_failed(outcome: &Outcome, code: u8, message: &str) {
    assert_eq!(outcome.code, ExitCode::from(code), "{outcome:?}");
    assert_eq!(outcome.stdout, "", "{outcome:?}");
    assert!(outcome.stderr.contains(message), "{outcome:?}");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The line of `output` that holds every one of `parts`.
fn line_with<'a>(output: &'a str, parts: &[&str]) -> Option<&'a str> {
    output
        .lines()
        .find(|line| parts.iter().all(|part| line.contains(part)))
}

/// The start of the warning about chunks longer than the embedding model's
/// context length.
const TRUNCATED_CHUNKS: &str = "some chunks are longer than the embedding model's context length";

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn the_status_counts_the_index_and_compares_its_embedding_model_with_the_configured_one() {
    let workspace = Workspace::with_real_model();
    let empty = status(&workspace);
    with_notes(&workspace, &[]);
    raw_collection(&workspace, "memo", &[("memo.md", "# Memo\n\nWords.\n")]);

    let full = status(&workspace);
    let for_people = workspace.run(&["status"]);

    assert_eq!(
        empty,
        json!({
            "version": VERSION,
            "configured_embedding_model": DEFAULT_MODEL,
            "configured_embedding_dimension": 256,
            "index_embedding_models": [],
            "chunks_without_embedding_count": 0,
            "collection_count": 0,
            "document_count": 0,
            "chunk_count": 0,
            "warnings": [],
        })
    );
    assert_eq!(
        full,
        json!({
            "version": VERSION,
            "configured_embedding_model": DEFAULT_MODEL,
            "configured_embedding_dimension": 256,
            "index_embedding_models": [{
                "model": DEFAULT_MODEL,
                "dimension": 256,
                "configured": true,
                "chunk_count": 4,
                "truncated_chunk_count": 0,
            }],
            "chunks_without_embedding_count": 0,
            "collection_count": 2,
            "document_count": 3,
            "chunk_count": 4,
            "warnings": [],
        })
    );
    assert_success(&for_people);
    assert_eq!(
        for_people.stdout,
        format!(
            "version: mdya {VERSION}\n\
             configured embedding model: {DEFAULT_MODEL} (256 dimensions)\n\
             index embedding model: {DEFAULT_MODEL} (256 dimensions, the configured one)\n  \
             chunks with embeddings: 4 (0 truncated)\n\
             chunks without embeddings: 0\n\
             collections: 2\n\
             documents: 3\n\
             chunks: 4\n"
        )
    );
}

/// The status and changing the embedding model setting only check the
/// model's dimension, and go on without the model; a rebuild makes
/// embeddings, and fails.
#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn without_the_embedding_model_the_status_and_the_model_setting_go_on_and_a_rebuild_fails_with_exit_code_1()
 {
    let workspace = Workspace::with_real_model();
    with_notes(&workspace, &[]);
    let before = status(&workspace);
    let found_before = found(&workspace, "fts");
    let without_the_model = |args: &[&str]| {
        workspace
            .executable_without_the_model()
            .args(args)
            .output()
            .unwrap()
    };

    let status_as_json = without_the_model(&["status", "--json"]);
    let status_for_people = without_the_model(&["status"]);
    // To the model that made the index's embeddings.
    let model_set = without_the_model(&["config", "set", "embedding.model", DEFAULT_MODEL]);
    let rebuilt = without_the_model(&["embedding", "rebuild"]);
    let forced = without_the_model(&["embedding", "rebuild", "--force"]);

    assert_eq!(
        status_as_json.status.code(),
        Some(0),
        "{}",
        stderr(&status_as_json)
    );
    let unchecked: Value = serde_json::from_slice(&status_as_json.stdout).unwrap();
    assert_eq!(unchecked["configured_embedding_dimension"], Value::Null);
    assert_eq!(
        unchecked["index_embedding_models"],
        json!([{
            "model": DEFAULT_MODEL,
            "dimension": 256,
            "configured": null,
            "chunk_count": 3,
            "truncated_chunk_count": 0,
        }])
    );
    let warnings = unchecked["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let warning = warnings[0].as_str().unwrap();
    assert!(
        warning.starts_with(
            "the dimension of the configured embedding model could not be checked: \
             the embedding model cannot be used: "
        ),
        "{warning}"
    );
    // The reason: fetching the model from its URL failed.
    assert!(warning.contains("https://huggingface.co/"), "{warning}");
    let for_people = stdout(&status_for_people);
    assert_eq!(
        status_for_people.status.code(),
        Some(0),
        "{}",
        stderr(&status_for_people)
    );
    assert!(
        for_people.contains(&format!(
            "configured embedding model: {DEFAULT_MODEL} (dimensions unknown)\n"
        )),
        "{for_people}"
    );
    assert!(
        for_people.contains("(256 dimensions, unknown whether it is the configured one)\n"),
        "{for_people}"
    );
    // The warning of the result, with its reason, as the last line.
    assert!(
        for_people.ends_with(&format!("\nwarning: {warning}\n")),
        "{for_people}"
    );
    let message = stderr(&model_set);
    assert_eq!(model_set.status.code(), Some(0), "{message}");
    assert_eq!(stdout(&model_set), "");
    assert!(
        message.contains(&format!(
            "Changed embedding.model from {DEFAULT_MODEL} to {DEFAULT_MODEL}.\n"
        )),
        "{message}"
    );
    assert!(
        line_with(&message, &["its dimension could not be checked"]).is_some(),
        "{message}"
    );
    assert!(
        line_with(&message, &["cause: the embedding model cannot be used"]).is_some(),
        "{message}"
    );
    for output in [&rebuilt, &forced] {
        let stderr = stderr(output);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert_eq!(stdout(output), "");
        assert!(
            stderr.contains("the embedding model cannot be used"),
            "{stderr}"
        );
    }
    assert_eq!(status(&workspace), before);
    assert_eq!(found(&workspace, "fts"), found_before);
}

#[test]
#[ignore = "runs a real embedding model, not the default one; run it with `just integration`"]
fn a_rebuild_warns_of_chunks_longer_than_the_model_context_length_and_the_status_counts_them() {
    let workspace = Workspace::with_real_model();
    assert_success(&workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]));
    // For this model, each `a` and each `,` is one token: the paragraph, of
    // 600 characters, is 400 tokens.
    let long = format!("# Long\n\n{}\n", "a, ".repeat(200));
    raw_collection(
        &workspace,
        "memo",
        &[
            ("long.md", long.as_str()),
            ("short.md", "# Short\n\nWords.\n"),
        ],
    );

    let counted = status(&workspace);
    let forced =
        workspace.run_executable(&["--log-level", "warn", "embedding", "rebuild", "--force"]);
    let unforced = workspace.run_executable(&["--log-level", "warn", "embedding", "rebuild"]);

    assert_eq!(
        counted["index_embedding_models"],
        json!([{
            "model": SECOND_MODEL,
            "dimension": 384,
            "configured": true,
            "chunk_count": 2,
            "truncated_chunk_count": 1,
        }])
    );
    for (output, summary) in [
        (&forced, "Rebuilt the embeddings of 2 chunks."),
        (&unforced, "Nothing was rebuilt"),
    ] {
        let stderr = stderr(output);
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(summary), "{stderr}");
        assert!(
            line_with(
                &stderr,
                &[
                    TRUNCATED_CHUNKS,
                    "collection=memo",
                    "chunks=1",
                    "mdya embedding rebuild"
                ]
            )
            .is_some(),
            "{stderr}"
        );
    }
    assert_eq!(status(&workspace), counted);
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_settings_are_listed_with_their_default_values_as_json_and_for_people() {
    let workspace = Workspace::new();

    let as_json = workspace.run(&["config", "list", "--json"]);
    let for_people = workspace.run(&["config", "list"]);

    assert_success(&as_json);
    assert_eq!(
        serde_json::from_str::<Value>(&as_json.stdout).unwrap(),
        defaults()
    );
    let places: Vec<usize> = SETTING_NAMES
        .iter()
        .map(|name| as_json.stdout.find(&format!("\"{name}\"")).unwrap())
        .collect();
    assert!(places.is_sorted(), "{}", as_json.stdout);
    assert_success(&for_people);
    assert_eq!(
        for_people.stdout,
        "mcp.show_version = true\n\
         embedding.model = cl-nagoya/ruri-v3-30m\n\
         embedding.ollama_endpoint = http://127.0.0.1:11434\n\
         resources.memory_limit_mb = 8192\n\
         resources.embedding_parallelism = 8\n\
         get.cli_limit_bytes = 1048576\n\
         get.mcp_limit_bytes = 1048576\n"
    );
}

/// The index has no chunk, so changing the embedding model setting does not
/// check the model's dimension.
#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_setting_is_changed_and_reset_and_each_change_tells_both_values() {
    let workspace = Workspace::new();
    let changes = [
        ("mcp.show_version", "false"),
        ("embedding.model", "google/embeddinggemma-300m"),
        ("embedding.model", SECOND_MODEL),
        ("embedding.model", "ollama:nomic-embed-text"),
        ("embedding.ollama_endpoint", "https://example.invalid:11434"),
        ("embedding.ollama_endpoint", "http://localhost:11434"),
        ("resources.memory_limit_mb", "0"),
        ("resources.embedding_parallelism", "9007199254740991"),
        ("get.cli_limit_bytes", "0"),
        ("get.mcp_limit_bytes", "9007199254740991"),
    ];

    for (name, value) in changes {
        let before = shown(&settings(&workspace)[name]);
        let changed = workspace.run(&["config", "set", name, value]);
        assert_success(&changed);
        assert_eq!(changed.stdout, "", "{changed:?}");
        assert_eq!(
            changed.stderr,
            format!("Changed {name} from {before} to {value}.\n")
        );
        assert_eq!(shown(&settings(&workspace)[name]), value);
    }
    for name in SETTING_NAMES {
        let before = shown(&settings(&workspace)[name]);
        let reset = workspace.run(&["config", "reset", name]);
        assert_success(&reset);
        assert_eq!(reset.stdout, "", "{reset:?}");
        assert_eq!(
            reset.stderr,
            format!(
                "Changed {name} from {before} to {}.\n",
                shown(&defaults()[name])
            )
        );
    }
    assert_eq!(settings(&workspace), defaults());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_setting_that_does_not_exist_or_a_value_that_it_does_not_take_fails_with_exit_code_2() {
    let workspace = Workspace::new();
    let refused: [(&[&str], &str); 8] = [
        (
            &["config", "set", "nosuch", "1"],
            "there is no setting named \"nosuch\"",
        ),
        (
            &["config", "reset", "nosuch"],
            "there is no setting named \"nosuch\"",
        ),
        (
            &["config", "set", "mcp.show_version", "yes"],
            "the value is not valid for the setting mcp.show_version",
        ),
        (
            &["config", "set", "get.cli_limit_bytes", "9007199254740992"],
            "the value is not valid for the setting get.cli_limit_bytes",
        ),
        (
            &["config", "set", "get.cli_limit_bytes", "1.5"],
            "the value is not valid for the setting get.cli_limit_bytes",
        ),
        (
            &["config", "set", "embedding.model", "no/such-model"],
            "the value is not valid for the setting embedding.model",
        ),
        (
            &["config", "set", "embedding.model", "ollama:"],
            "the value is not valid for the setting embedding.model",
        ),
        (
            &[
                "config",
                "set",
                "embedding.ollama_endpoint",
                "ftp://example.invalid",
            ],
            "the value is not valid for the setting embedding.ollama_endpoint",
        ),
    ];

    for (args, message) in refused {
        assert_failed(&workspace.run(args), 2, message);
    }
    assert_eq!(settings(&workspace), defaults());
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn settings_or_an_index_that_cannot_be_read_fail_the_operations_with_exit_code_1() {
    let without_settings = Workspace::new();
    raw_collection(&without_settings, "memo", &[]);
    let settings_file = without_settings.data_dir().join("config.json");
    std::fs::write(&settings_file, "{ not json").unwrap();
    let without_index = Workspace::new();
    raw_collection(&without_index, "memo", &[]);
    // Each version of the table of the collections, and the file that points
    // to the latest one.
    let versions = without_index
        .data_dir()
        .join("lancedb/collection.lance/_versions");
    for entry in std::fs::read_dir(versions).unwrap() {
        std::fs::write(entry.unwrap().path(), "not a version").unwrap();
    }

    let refused_without_settings: [&[&str]; 6] = [
        &["config", "list"],
        &["config", "list", "--json"],
        &["config", "set", "get.cli_limit_bytes", "5"],
        &["collection", "list"],
        &["status"],
        &["optimize"],
    ];
    // Not the status, which checks the embedding model's dimension before it
    // reads the collections.
    let refused_without_index: [&[&str]; 2] = [
        &["collection", "list"],
        &["collection", "add", "other", "--type", "raw"],
    ];

    for args in refused_without_settings {
        let outcome = without_settings.run(args);
        assert_failed(&outcome, 1, "the settings cannot be read");
        assert!(outcome.stderr.contains("Caused by:"), "{outcome:?}");
        assert!(outcome.stderr.contains("How to fix:"), "{outcome:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&settings_file).unwrap(),
        "{ not json"
    );
    for args in refused_without_index {
        let outcome = without_index.run(args);
        assert_failed(&outcome, 1, "the collection information cannot be read");
        assert!(outcome.stderr.contains("Caused by:"), "{outcome:?}");
        assert!(outcome.stderr.contains("How to fix:"), "{outcome:?}");
    }
}

#[test]
#[cfg(unix)]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn changing_a_setting_that_cannot_be_written_fails_with_exit_code_1() {
    let workspace = Workspace::new();
    raw_collection(&workspace, "memo", &[]);

    let locked = Locked::new(workspace.data_dir(), 0o555);
    let refused = workspace.run(&["config", "set", "get.cli_limit_bytes", "5"]);
    drop(locked);

    assert_failed(&refused, 1, "the settings cannot be written");
    assert!(refused.stderr.contains("Caused by:"), "{refused:?}");
    assert_eq!(settings(&workspace), defaults());
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_rebuild_with_the_configured_model_does_nothing_unless_forced() {
    let workspace = Workspace::with_real_model();
    with_notes(&workspace, &[]);
    let before = status(&workspace);
    let found_before = found(&workspace, "vector");

    let unforced = workspace.run(&["embedding", "rebuild"]);
    let forced = workspace.run(&["embedding", "rebuild", "--force"]);
    let without_maintenance = workspace.run(&["embedding", "rebuild", "--force", "--no-optimize"]);

    assert_success(&unforced);
    assert_eq!(unforced.stdout, "", "{unforced:?}");
    assert!(
        unforced.stderr.ends_with(
            "Nothing was rebuilt: the embeddings were already made with the configured model.\n"
        ),
        "{unforced:?}"
    );
    for rebuilt in [&forced, &without_maintenance] {
        assert_success(rebuilt);
        assert_eq!(rebuilt.stdout, "", "{rebuilt:?}");
        assert!(
            rebuilt
                .stderr
                .ends_with("Rebuilt the embeddings of 3 chunks.\n"),
            "{rebuilt:?}"
        );
    }
    assert_eq!(status(&workspace), before);
    assert_eq!(found(&workspace, "vector"), found_before);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn maintaining_the_index_tells_it_and_keeps_every_document() {
    let workspace = Workspace::with_real_model();
    let on_an_empty_index = workspace.run(&["optimize"]);
    with_notes(&workspace, &["--no-optimize"]);
    let listed = json_of(&workspace, &["collection", "list"]);
    let before = status(&workspace);
    let found_before = found(&workspace, "fts");

    let maintained = workspace.run(&["optimize"]);

    for outcome in [&on_an_empty_index, &maintained] {
        assert_success(outcome);
        assert_eq!(outcome.stdout, "", "{outcome:?}");
        assert!(
            outcome.stderr.ends_with("Maintained the index.\n"),
            "{outcome:?}"
        );
    }
    assert_eq!(json_of(&workspace, &["collection", "list"]), listed);
    assert_eq!(status(&workspace), before);
    assert_eq!(found(&workspace, "fts"), found_before);
}

#[test]
#[cfg(unix)]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn maintaining_or_rebuilding_an_index_that_cannot_be_written_fails_with_exit_code_1() {
    let workspace = Workspace::with_real_model();
    // Without the final maintenance, so that maintaining has something to
    // write.
    with_notes(&workspace, &["--no-optimize"]);
    let before = status(&workspace);

    let locked = Locked::tree(&workspace.data_dir().join("lancedb"), 0o555);
    let maintained = workspace.run(&["optimize"]);
    let rebuilt = workspace.run(&["embedding", "rebuild", "--force"]);
    drop(locked);

    for outcome in [&maintained, &rebuilt] {
        assert_failed(outcome, 1, "the index cannot be written");
        assert!(outcome.stderr.contains("Caused by:"), "{outcome:?}");
    }
    assert_eq!(status(&workspace), before);
}
