use serde_json::json;

use crate::config;

use super::super::super::operations::{
    CollectionEntry, CollectionList, IndexEmbeddingModel, SafeInteger, SafeJsonObject,
    SafeJsonValue, Score, SearchHit, SearchLimit, SearchModeName, SearchOutput, SearchUnitName,
    SettingsOutput, StatusOutput,
};
use super::*;

fn entry(
    name: &str,
    collection_type: &str,
    description: Option<&str>,
    count: u64,
) -> CollectionEntry {
    CollectionEntry {
        name: name.to_owned(),
        collection_type: collection_type.to_owned(),
        description: description.map(str::to_owned),
        document_count: SafeInteger::new(count),
        attributes: SafeJsonObject::new(serde_json::Map::new()),
    }
}

// The example that the user approved.
#[test]
fn the_collections_are_blocks_with_the_description_and_the_attributes_indented() {
    let mut notes = entry("notes", "directory", Some("個人のメモ"), 42);
    let mut attributes = serde_json::Map::new();
    attributes.insert("path".to_owned(), json!("/Users/me/notes"));
    notes.attributes = SafeJsonObject::new(attributes);
    let list = CollectionList {
        collections: vec![entry("memo", "raw", None, 3), notes],
    };

    assert_eq!(
        human_collection_list(&list),
        "memo (raw, 3 documents)\n\nnotes (directory, 42 documents)\n  個人のメモ\n  path: /Users/me/notes\n"
    );
}

#[test]
fn one_document_is_said_in_the_singular() {
    let list = CollectionList {
        collections: vec![entry("memo", "raw", None, 1)],
    };

    assert_eq!(human_collection_list(&list), "memo (raw, 1 document)\n");
}

#[test]
fn no_collection_is_said_so() {
    let list = CollectionList {
        collections: Vec::new(),
    };

    assert_eq!(human_collection_list(&list), "(no collections)\n");
}

#[test]
fn the_strings_of_a_collection_have_their_controls_replaced() {
    let list = CollectionList {
        collections: vec![entry("a\u{1b}b", "raw", Some("x\ny\u{202e}z"), 0)],
    };

    let text = human_collection_list(&list);

    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{202e}'));
    assert_eq!(text.lines().count(), 2, "{text}");
}

fn hit(
    collection: &str,
    key: &str,
    score: f64,
    snippet: &str,
    chunk: u64,
    chunks: u64,
) -> SearchHit {
    SearchHit {
        collection: collection.to_owned(),
        key: key.to_owned(),
        score: Score::new(score),
        snippet: snippet.to_owned(),
        chunk_index: SafeInteger::new(chunk),
        chunk_count: SafeInteger::new(chunks),
    }
}

fn search_output(hits: Vec<SearchHit>) -> SearchOutput {
    SearchOutput {
        query: "q".to_owned(),
        mode: SearchModeName::Hybrid,
        unit: SearchUnitName::Document,
        collections: Vec::new(),
        limit: SearchLimit::new(20),
        hit_count: SafeInteger::from_usize(hits.len()),
        hits,
        warnings: vec!["a warning".to_owned()],
    }
}

// The example that the user approved. The warnings are not in it: they go to
// stderr, as logs.
#[test]
fn the_hits_are_three_lines_each_with_the_score_rounded() {
    let output = search_output(vec![
        hit(
            "notes",
            "release.md",
            0.033_064_516,
            "# リリースの手順 タグを打つ前に、変更履歴を…",
            0,
            12,
        ),
        hit(
            "notes",
            "ops/deploy.md",
            0.0325,
            "…本番に出す前に、リリースの手順を確かめる…",
            3,
            7,
        ),
    ]);

    assert_eq!(
        human_search(&output).unwrap(),
        "1. notes  release.md\n   score 0.0331, chunk 0 of 12\n   # リリースの手順 タグを打つ前に、変更履歴を…\n\n2. notes  ops/deploy.md\n   score 0.0325, chunk 3 of 7\n   …本番に出す前に、リリースの手順を確かめる…\n"
    );
}

#[test]
fn no_hit_is_said_so() {
    assert_eq!(
        human_search(&search_output(Vec::new())).unwrap(),
        "(no hits)\n"
    );
}

#[test]
fn the_strings_of_a_hit_have_their_controls_replaced() {
    let output = search_output(vec![hit("c\u{7}", "k\u{202e}", 1.0, "s\u{1b}", 0, 1)]);

    let text = human_search(&output).unwrap();

    assert!(!text.contains(['\u{7}', '\u{202e}', '\u{1b}']), "{text}");
}

fn status(models: Vec<IndexEmbeddingModel>, dimension: Option<u64>) -> StatusOutput {
    StatusOutput {
        version: "1.0.0".to_owned(),
        configured_embedding_model: "cl-nagoya/ruri-v3-30m".to_owned(),
        configured_embedding_dimension: dimension.map(SafeInteger::new),
        index_embedding_models: models,
        chunks_without_embedding_count: SafeInteger::new(0),
        collection_count: SafeInteger::new(3),
        document_count: SafeInteger::new(120),
        chunk_count: SafeInteger::new(1534),
        warnings: Vec::new(),
    }
}

fn index_model(configured: Option<bool>) -> IndexEmbeddingModel {
    IndexEmbeddingModel {
        model: "cl-nagoya/ruri-v3-30m".to_owned(),
        dimension: SafeInteger::new(256),
        configured,
        chunk_count: SafeInteger::new(1534),
        truncated_chunk_count: SafeInteger::new(0),
    }
}

// The example that the user approved.
#[test]
fn the_status_is_one_line_an_item() {
    assert_eq!(
        human_status(&status(vec![index_model(Some(true))], Some(256))),
        "version: mdya 1.0.0\nconfigured embedding model: cl-nagoya/ruri-v3-30m (256 dimensions)\nindex embedding model: cl-nagoya/ruri-v3-30m (256 dimensions, the configured one)\n  chunks with embeddings: 1534 (0 truncated)\nchunks without embeddings: 0\ncollections: 3\ndocuments: 120\nchunks: 1534\n"
    );
}

#[test]
fn the_status_says_what_could_not_be_checked_and_what_is_not_there() {
    let text = human_status(&status(Vec::new(), None));
    assert!(
        text.contains("configured embedding model: cl-nagoya/ruri-v3-30m (dimensions unknown)\n")
    );
    assert!(text.contains("index embedding model: none\n"));

    let unknown = human_status(&status(vec![index_model(None)], Some(256)));
    assert!(unknown.contains("(256 dimensions, unknown whether it is the configured one)"));

    let other = human_status(&status(vec![index_model(Some(false))], Some(256)));
    assert!(other.contains("(256 dimensions, not the configured one)"));
}

fn setting(name: &str, value: serde_json::Value) -> (config::SettingName, SafeJsonValue) {
    (
        config::check_setting(name, None).unwrap(),
        SafeJsonValue::new(value),
    )
}

// The example that the user approved.
#[test]
fn the_settings_are_name_equals_value_with_strings_unquoted() {
    let output = SettingsOutput {
        entries: vec![
            setting("mcp.show_version", json!(true)),
            setting("embedding.model", json!("cl-nagoya/ruri-v3-30m")),
            setting("embedding.ollama_endpoint", json!("http://127.0.0.1:11434")),
            setting("resources.memory_limit_mb", json!(8192)),
            setting("resources.embedding_parallelism", json!(8)),
            setting("get.cli_limit_bytes", json!(1_048_576)),
            setting("get.mcp_limit_bytes", json!(1_048_576)),
        ],
    };

    assert_eq!(
        human_settings(&output),
        "mcp.show_version = true\nembedding.model = cl-nagoya/ruri-v3-30m\nembedding.ollama_endpoint = http://127.0.0.1:11434\nresources.memory_limit_mb = 8192\nresources.embedding_parallelism = 8\nget.cli_limit_bytes = 1048576\nget.mcp_limit_bytes = 1048576\n"
    );
}

#[test]
fn a_setting_string_has_its_controls_replaced() {
    let output = SettingsOutput {
        entries: vec![setting("embedding.model", json!("a\u{1b}b"))],
    };

    assert!(!human_settings(&output).contains('\u{1b}'));
}
