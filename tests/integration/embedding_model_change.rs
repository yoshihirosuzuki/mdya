//! Changing the embedding model setting on the command line, with the real
//! components. The index's embeddings stay those of the model that made them
//! until a rebuild: meanwhile vector search finds nothing, and updates and
//! puts fail. A rebuild makes them again with the configured model. A running
//! MCP server keeps the model of the setting it started with.
//!
//! Each test runs the default model and a second one. In the test's own
//! process, the warnings that mdya logs go nowhere (see the module `common`):
//! the tests that check a logged warning run the executable.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{ExitCode, Output};

use serde_json::{Value, json};

use crate::common::{Outcome, Workspace};
use crate::mcp_server::{StdioServer, succeeded};

/// The default embedding model, whose embeddings have 256 dimensions.
const DEFAULT_MODEL: &str = "cl-nagoya/ruri-v3-30m";

/// Another embedding model, whose embeddings have 384 dimensions.
const SECOND_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2";

const RELEASE: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";
const TAGS: &str = "# タグの付け方\n\nタグの名前は、版の番号にします。\n";
const COOKING: &str = "# 料理\n\n玉ねぎを炒めます。\n";

/// The files of the collection `notes`.
const NOTES: [(&str, &str); 3] = [
    ("release.md", RELEASE),
    ("tags.md", TAGS),
    ("cooking.md", COOKING),
];

/// `TAGS` with a sentence added.
const CHANGED_TAGS: &str = "# タグの付け方\n\nタグの名前は、版の番号にします。署名も付けます。\n";
const CLEANING: &str = "# 掃除\n\n窓を拭きます。\n";
const MEMO: &str = "# メモ\n\n買い物の一覧。\n";
const POST: &str = "# 新しいメモ\n\n郵便を出す。\n";

/// The error of an update or a put after the setting changed from the
/// default model, which made the index's embeddings, to the second one.
const MODELS_DIFFER: &str = "the configured embedding model sentence-transformers/all-MiniLM-L6-v2 (384 dimensions) differs from the one the index's embeddings were made with, cl-nagoya/ruri-v3-30m (256 dimensions)";

/// How to fix that error.
const REBUILD_FIRST: &str =
    "Rebuild the embeddings with mdya embedding rebuild, then run the operation again.";

/// What the message after changing the embedding model setting adds, when
/// the index's embeddings were made with `model`, another one.
fn made_with_another_model(model: &str) -> String {
    format!(
        "The index's embeddings were made with another model ({model}).\n\
         Until you run mdya embedding rebuild, vector search finds nothing, and update and put fail.\n"
    )
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

/// A workspace with the directory collection `notes` of the three documents
/// above, updated with the default model. Each document is one chunk.
fn with_notes() -> Workspace {
    let workspace = Workspace::with_real_model();
    let dir = workspace.dir("notes");
    for (file, contents) in NOTES {
        std::fs::write(dir.join(file), contents).unwrap();
    }
    assert_success(&workspace.run(&["collection", "add", "notes", &text(&dir)]));
    assert_success(&workspace.run(&["update", "notes"]));
    workspace
}

/// Runs `mdya put memo <key> --file-type md -` with `contents` as its standard
/// input.
fn put(workspace: &Workspace, key: &str, contents: &str) -> Outcome {
    workspace.run_with_stdin(
        &["put", "memo", key, "--file-type", "md", "-"],
        contents.as_bytes(),
    )
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

/// The result of a search for `query` in the search mode `mode`.
fn search(workspace: &Workspace, query: &str, mode: &str) -> Value {
    json_of(workspace, &["search", query, "--mode", mode])
}

/// The keys of the hits of a full-text search for `query`, in their order.
fn found(workspace: &Workspace, query: &str) -> Vec<String> {
    let result = search(workspace, query, "fts");
    keys(&result).into_iter().map(str::to_owned).collect()
}

fn hits(result: &Value) -> &Vec<Value> {
    result["hits"].as_array().unwrap()
}

/// The keys of the hits of a search result, in their order.
fn keys(result: &Value) -> Vec<&str> {
    hits(result)
        .iter()
        .map(|hit| hit["key"].as_str().unwrap())
        .collect()
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items
}

fn scores(result: &Value) -> Vec<f64> {
    hits(result)
        .iter()
        .map(|hit| hit["score"].as_f64().unwrap())
        .collect()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{actual} is not {expected}"
    );
}

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
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

#[test]
#[ignore = "runs the real embedding model and a second one; run it with `just integration`"]
fn a_vector_or_hybrid_search_warns_of_embeddings_made_with_another_model() {
    let workspace = with_notes();
    assert_success(&workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]));
    let search_logging = |log_level: &str, options: &[&str]| {
        let mut args = vec!["--log-level", log_level, "search", "タグ"];
        args.extend_from_slice(options);
        workspace.run_executable(&args)
    };
    let warning = "The index's embeddings were made with cl-nagoya/ruri-v3-30m (256 dimensions), but the query's embedding was made with sentence-transformers/all-MiniLM-L6-v2 (384 dimensions).";

    let vector = search_logging("warn", &["--mode", "vector", "--json"]);
    let hybrid = search_logging("warn", &["--mode", "hybrid", "--json"]);
    let full_text = search_logging("warn", &["--mode", "fts", "--json"]);
    let for_people = search_logging("warn", &["--mode", "hybrid"]);
    let quiet = search_logging("error", &["--mode", "hybrid", "--json"]);

    let result_of = |output: &Output| -> Value {
        assert_eq!(output.status.code(), Some(0), "{}", stderr(output));
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let assert_warns = |result: &Value| {
        let warnings = result["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{result}");
        let text = warnings[0].as_str().unwrap();
        assert!(
            text.starts_with(warning) && text.contains("mdya embedding rebuild"),
            "{text}"
        );
    };
    // Vector search: no hits.
    let result = result_of(&vector);
    assert_eq!(result["mode"], "vector");
    assert_eq!(result["hits"], json!([]));
    assert_warns(&result);
    assert!(line_with(&stderr(&vector), &["WARN", warning]).is_some());
    // Hybrid search: the full-text hits alone, ranked by their full-text
    // ranks.
    let full_text = result_of(&full_text);
    assert_eq!(sorted(keys(&full_text)), ["release.md", "tags.md"]);
    assert_eq!(full_text["warnings"], json!([]));
    let result = result_of(&hybrid);
    assert_eq!(result["mode"], "hybrid");
    assert_eq!(keys(&result), keys(&full_text));
    let expected: Vec<f64> = (0..keys(&full_text).len())
        .map(|rank| 1.0 / (60.0 + rank as f64))
        .collect();
    for (actual, expected) in scores(&result).into_iter().zip(expected) {
        assert_close(actual, expected);
    }
    assert_warns(&result);
    assert!(line_with(&stderr(&hybrid), &["WARN", warning]).is_some());
    // Without JSON, the warning is on stderr only.
    assert_eq!(for_people.status.code(), Some(0), "{}", stderr(&for_people));
    assert!(!stdout(&for_people).contains(warning));
    assert!(line_with(&stderr(&for_people), &["WARN", warning]).is_some());
    // The result holds the warning whatever the log level.
    assert_warns(&result_of(&quiet));
    assert!(!stderr(&quiet).contains(warning));
}

#[test]
#[ignore = "runs the real embedding model and a second one; run it with `just integration`"]
fn a_changed_model_setting_fails_updates_and_puts_until_a_rebuild_makes_the_embeddings_with_it() {
    let workspace = with_notes();
    assert_success(&workspace.run(&["collection", "add", "memo", "--type", "raw"]));
    assert_success(&put(&workspace, "memo.md", MEMO));
    let before = status(&workspace);

    let changed = workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]);
    let mismatched = status(&workspace);
    // With nothing to write: the sources and the contents are as before.
    let refused_unchanged = [
        workspace.run(&["update", "notes"]),
        put(&workspace, "memo.md", MEMO),
    ];
    let notes = workspace.dir("notes");
    std::fs::write(notes.join("tags.md"), CHANGED_TAGS).unwrap();
    std::fs::write(notes.join("cleaning.md"), CLEANING).unwrap();
    let refused_changed = [
        workspace.run(&["update", "notes"]),
        workspace.run(&["update", "--all"]),
        put(&workspace, "post.md", POST),
    ];
    let found_left = [
        found(&workspace, "署名"),
        found(&workspace, "窓"),
        found(&workspace, "郵便"),
    ];
    let got = workspace.run(&["get", "notes", "tags.md"]);
    let not_added = [
        workspace.run(&["get", "notes", "cleaning.md"]),
        workspace.run(&["get", "memo", "post.md"]),
    ];
    let searched_meanwhile = search(&workspace, "タグ", "vector");
    let left = status(&workspace);

    let rebuilt = workspace.run(&["embedding", "rebuild"]);
    let rebuilt_status = status(&workspace);
    let searched_after = search(&workspace, "タグ", "vector");
    let updated = workspace.run(&["update", "notes"]);
    let put_after = put(&workspace, "post.md", POST);
    let found_after = [
        found(&workspace, "署名"),
        found(&workspace, "窓"),
        found(&workspace, "郵便"),
    ];
    let after = status(&workspace);
    let reset = workspace.run(&["config", "reset", "embedding.model"]);

    // Changing the setting tells that the index's embeddings were made with
    // another model, and leaves them as they were.
    assert_success(&changed);
    assert_eq!(changed.stdout, "", "{changed:?}");
    assert_eq!(
        changed.stderr,
        format!(
            "Changed embedding.model from {DEFAULT_MODEL} to {SECOND_MODEL}.\n{}",
            made_with_another_model(DEFAULT_MODEL)
        )
    );
    assert_eq!(mismatched["configured_embedding_model"], SECOND_MODEL);
    assert_eq!(mismatched["configured_embedding_dimension"], 384);
    assert_eq!(
        mismatched["index_embedding_models"],
        json!([{
            "model": DEFAULT_MODEL,
            "dimension": 256,
            "configured": false,
            "chunk_count": 4,
            "truncated_chunk_count": 0,
        }])
    );
    // Meanwhile, each update and put fails, telling both models and the fix,
    // and writes nothing; full-text search, getting a document, and vector
    // search go on.
    for outcome in refused_unchanged.iter().chain(&refused_changed) {
        assert_eq!(outcome.code, ExitCode::from(1), "{outcome:?}");
        assert_eq!(outcome.stdout, "", "{outcome:?}");
        assert!(outcome.stderr.contains(MODELS_DIFFER), "{outcome:?}");
        assert!(outcome.stderr.contains(REBUILD_FIRST), "{outcome:?}");
    }
    assert_eq!(left, mismatched);
    let nothing: [&str; 0] = [];
    assert_eq!(found_left, [nothing; 3]);
    assert_success(&got);
    assert_eq!(got.stdout, TAGS);
    for (outcome, message) in not_added.iter().zip([
        "the document \"cleaning.md\" does not exist in the collection \"notes\"",
        "the document \"post.md\" does not exist in the collection \"memo\"",
    ]) {
        assert_eq!(outcome.code, ExitCode::from(1), "{outcome:?}");
        assert!(outcome.stderr.contains(message), "{outcome:?}");
    }
    assert_eq!(searched_meanwhile["hits"], json!([]));
    assert_eq!(
        searched_meanwhile["warnings"].as_array().unwrap().len(),
        1,
        "{searched_meanwhile}"
    );
    // A rebuild, without being forced, makes the embeddings of every chunk
    // with the configured model, and leaves the documents and their chunks.
    assert_success(&rebuilt);
    assert_eq!(rebuilt.stdout, "", "{rebuilt:?}");
    assert!(
        rebuilt
            .stderr
            .ends_with("Rebuilt the embeddings of 4 chunks.\n"),
        "{rebuilt:?}"
    );
    assert_eq!(
        rebuilt_status["index_embedding_models"],
        json!([{
            "model": SECOND_MODEL,
            "dimension": 384,
            "configured": true,
            "chunk_count": 4,
            "truncated_chunk_count": 0,
        }])
    );
    for field in [
        "chunks_without_embedding_count",
        "collection_count",
        "document_count",
        "chunk_count",
    ] {
        assert_eq!(rebuilt_status[field], before[field], "{field}");
    }
    // Then vector search finds every document without a warning, and the
    // update and the put that failed go through.
    assert_eq!(
        sorted(keys(&searched_after)),
        ["cooking.md", "memo.md", "release.md", "tags.md"]
    );
    assert_eq!(searched_after["warnings"], json!([]));
    assert_success(&updated);
    assert!(
        updated.stderr.ends_with(
            "Updated the collection \"notes\": 1 added, 1 changed, 2 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
        ),
        "{updated:?}"
    );
    assert_success(&put_after);
    assert_eq!(found_after, [["tags.md"], ["cleaning.md"], ["post.md"]]);
    assert_eq!(
        after["index_embedding_models"],
        json!([{
            "model": SECOND_MODEL,
            "dimension": 384,
            "configured": true,
            "chunk_count": 6,
            "truncated_chunk_count": 0,
        }])
    );
    // Resetting the setting is changing it too: the default model is now
    // another model than the one that made the index's embeddings.
    assert_success(&reset);
    assert_eq!(reset.stdout, "", "{reset:?}");
    assert_eq!(
        reset.stderr,
        format!(
            "Changed embedding.model from {SECOND_MODEL} to {DEFAULT_MODEL}.\n{}",
            made_with_another_model(SECOND_MODEL)
        )
    );
}

#[test]
#[ignore = "runs the real embedding model and a second one; run it with `just integration`"]
fn a_running_mcp_server_searches_with_the_model_it_started_with_until_it_starts_again() {
    let workspace = with_notes();
    let vector_search = || json!({ "query": "タグ", "mode": "vector" });
    let every_key = ["cooking.md", "release.md", "tags.md"];

    let mut server = StdioServer::start(workspace.executable());
    server.initialize();
    let before = succeeded(&server.call("search", vector_search()));
    assert_success(&workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]));
    let changed = succeeded(&server.call("search", vector_search()));
    assert_success(&workspace.run(&["embedding", "rebuild"]));
    let rebuilt = succeeded(&server.call("search", vector_search()));
    let ended = server.finish();
    let mut started_again = StdioServer::start(workspace.executable());
    started_again.initialize();
    let after = succeeded(&started_again.call("search", vector_search()));
    let ended_again = started_again.finish();

    // The server makes the query's embedding with the model of the setting
    // it started with: it finds the documents after the setting changed, as
    // the index's embeddings are still of that model.
    for result in [&before, &changed] {
        assert_eq!(sorted(keys(result)), every_key, "{result}");
        assert_eq!(result["warnings"], json!([]), "{result}");
    }
    // After the rebuild, the index's embeddings are of the new setting's
    // model, and the server's queries are not.
    let warning = "The index's embeddings were made with sentence-transformers/all-MiniLM-L6-v2 (384 dimensions), but the query's embedding was made with cl-nagoya/ruri-v3-30m (256 dimensions).";
    assert_eq!(rebuilt["hits"], json!([]), "{rebuilt}");
    let warnings = rebuilt["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{rebuilt}");
    let text = warnings[0].as_str().unwrap();
    assert!(
        text.starts_with(warning) && text.contains("restart the MCP server"),
        "{text}"
    );
    assert!(ended.status.success(), "{}", ended.stderr);
    assert!(
        line_with(&ended.stderr, &["WARN", warning]).is_some(),
        "{}",
        ended.stderr
    );
    // A server started again makes the query's embedding with the new model.
    assert_eq!(sorted(keys(&after)), every_key, "{after}");
    assert_eq!(after["warnings"], json!([]), "{after}");
    assert!(ended_again.status.success(), "{}", ended_again.stderr);
}

/// The index's chunks were made with the default model, and are all gone:
/// no embedding is there to differ from the configured model.
#[test]
#[ignore = "runs the real embedding model and a second one; run it with `just integration`"]
fn an_index_whose_chunks_are_all_gone_takes_documents_with_another_model() {
    let workspace = with_notes();
    let notes = workspace.dir("notes");
    for (file, _) in NOTES {
        std::fs::remove_file(notes.join(file)).unwrap();
    }
    assert_success(&workspace.run(&["update", "notes"]));
    let emptied = status(&workspace);

    let changed = workspace.run(&["config", "set", "embedding.model", SECOND_MODEL]);
    std::fs::write(notes.join("cooking.md"), COOKING).unwrap();
    let updated = workspace.run(&["update", "notes"]);
    let after = status(&workspace);
    let searched = search(&workspace, "料理", "vector");

    assert_eq!(emptied["index_embedding_models"], json!([]));
    assert_eq!(emptied["chunk_count"], 0);
    assert_success(&changed);
    assert_eq!(
        changed.stderr,
        format!("Changed embedding.model from {DEFAULT_MODEL} to {SECOND_MODEL}.\n")
    );
    assert_success(&updated);
    assert!(
        updated.stderr.ends_with(
            "Updated the collection \"notes\": 1 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
        ),
        "{updated:?}"
    );
    assert_eq!(
        after["index_embedding_models"],
        json!([{
            "model": SECOND_MODEL,
            "dimension": 384,
            "configured": true,
            "chunk_count": 1,
            "truncated_chunk_count": 0,
        }])
    );
    assert_eq!(keys(&searched), ["cooking.md"]);
    assert_eq!(searched["warnings"], json!([]));
}
