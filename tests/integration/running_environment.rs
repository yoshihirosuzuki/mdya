//! Integration tests of the surroundings mdya runs in: without the network
//! once the embedding model is in its cache, and with one model cache shared
//! by several settings and index directories. The executable runs in a
//! separate process with an HTTPS proxy that refuses connections, so that it
//! cannot fetch anything.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which the helpers below are not.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Output};

use serde_json::{Value, json};

use crate::common::{Outcome, Workspace};
use crate::mcp_server::{StdioServer, succeeded};

const RELEASE: &str = "# リリースの手順\n\nタグを打つ前に、変更履歴を確かめます。\n";
const TAGS: &str = "# タグの付け方\n\nタグの名前は、版の番号にします。\n";
const MEMO: &str = "# 覚え書き\n\n古いタグは消さずに残します。\n";
const BACKUP: &str = "# バックアップ\n\n毎晩、外付けのディスクに写します。\n";

fn assert_success(outcome: &Outcome) {
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{outcome:?}");
}

/// A workspace with the collection `notes` of `notes` (file name and
/// text), updated in the test process. The update fetches the default
/// embedding model into the shared model cache if it is not there yet.
fn updated(notes: &[(&str, &str)]) -> (Workspace, PathBuf) {
    let workspace = Workspace::with_real_model();
    let dir = workspace.dir("notes");
    for (name, text) in notes {
        std::fs::write(dir.join(name), text).unwrap();
    }
    assert_success(&workspace.run(&["collection", "add", "notes", dir.to_str().unwrap()]));
    assert_success(&workspace.run(&["update", "notes"]));
    (workspace, dir)
}

/// Runs the executable with `args` on the workspace's directories, unable to
/// fetch anything, and returns its output. It must succeed.
fn offline(workspace: &Workspace, args: &[&str]) -> Output {
    let output = workspace
        .executable_that_cannot_fetch()
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "`mdya {}`: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// The keys of the hits of a vector search for `query`, unable to fetch
/// anything.
fn found_offline(workspace: &Workspace, query: &str) -> Vec<String> {
    let output = offline(workspace, &["search", query, "--mode", "vector", "--json"]);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["key"].as_str().unwrap().to_owned())
        .collect()
}

/// The files under `dir` that belong to an embedding model: its weights and
/// its tokenizer, as the model cache holds them.
fn model_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(model_files(&path));
            continue;
        }
        let is_weights = path
            .extension()
            .is_some_and(|extension| extension == "safetensors");
        let is_tokenizer = path
            .file_name()
            .is_some_and(|name| name == "tokenizer.json");
        if is_weights || is_tokenizer {
            found.push(path);
        }
    }
    found
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn with_the_model_in_its_cache_mdya_updates_and_searches_without_the_network() {
    let (workspace, dir) = updated(&[
        ("release.md", RELEASE),
        ("tags.md", TAGS),
        ("memo.md", MEMO),
    ]);
    std::fs::write(dir.join("backup.md"), BACKUP).unwrap();

    let update = offline(&workspace, &["update", "notes"]);
    let found = found_offline(&workspace, "バックアップ");
    let mut server = StdioServer::start(workspace.executable_that_cannot_fetch());
    server.initialize();
    let answer = server.call(
        "search",
        json!({ "query": "バックアップ", "mode": "vector" }),
    );
    let ended = server.finish();

    assert!(
        String::from_utf8_lossy(&update.stderr).ends_with(
            "Updated the collection \"notes\": 1 added, 0 changed, 3 unchanged, 0 re-ingested, \
             0 deleted, 0 failed.\n"
        ),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert_eq!(found[0], "backup.md", "{found:?}");
    assert_eq!(
        succeeded(&answer)["hits"][0]["key"],
        "backup.md",
        "{answer}"
    );
    assert_eq!(ended.status.code(), Some(0), "{}", ended.stderr);
}

#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn two_settings_and_index_directories_share_one_model_cache() {
    let (first, _) = updated(&[("release.md", RELEASE)]);
    let second = Workspace::with_real_model();
    let dir = second.dir("notes");
    std::fs::write(dir.join("backup.md"), BACKUP).unwrap();
    std::fs::write(dir.join("tags.md"), TAGS).unwrap();
    assert_success(&second.run(&["collection", "add", "notes", dir.to_str().unwrap()]));

    // Unable to fetch, the second directory's update and searches use the
    // model in the cache that the first directory's update used.
    offline(&second, &["update", "notes"]);
    let found_in_second = found_offline(&second, "バックアップ");
    let found_in_first = found_offline(&first, "バックアップ");

    assert_eq!(found_in_second[0], "backup.md", "{found_in_second:?}");
    // Each directory holds its own documents only.
    let mut keys_in_second = found_in_second.clone();
    keys_in_second.sort();
    assert_eq!(keys_in_second, ["backup.md", "tags.md"]);
    assert_eq!(found_in_first, ["release.md"]);
    for workspace in [&first, &second] {
        assert_eq!(model_files(workspace.data_dir()), Vec::<PathBuf>::new());
    }
}
