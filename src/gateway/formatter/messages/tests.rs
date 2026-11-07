use std::net::SocketAddr;

use serde_json::json;

use crate::collection::CollectionInfo;
use crate::config::{self, SettingChange};
use crate::ingest::UpdateReport;
use crate::types::{CollectionName, CollectionType, DocumentOutcome, Key, TypeAttributes};

use super::*;

fn info(description: Option<&str>, path: Option<&str>) -> CollectionInfo {
    let mut attributes = serde_json::Map::new();
    if let Some(path) = path {
        attributes.insert("path".to_owned(), json!(path));
    }
    CollectionInfo {
        name: CollectionName("notes".to_owned()),
        collection_type: CollectionType("directory".to_owned()),
        attributes: TypeAttributes(attributes),
        description: description.map(str::to_owned),
    }
}

fn name(name: &str) -> CollectionName {
    CollectionName(name.to_owned())
}

fn key(key: &str) -> Key {
    Key(key.to_owned())
}

// The examples that the user approved, here and below.
#[test]
fn a_created_collection_is_named_with_its_type_attributes_and_description() {
    assert_eq!(
        human_message(&Message::CollectionCreated(&info(
            Some("個人のメモ"),
            Some("/Users/me/notes")
        ))),
        "Created the collection \"notes\" (directory).\n  path: /Users/me/notes\n  description: 個人のメモ\n"
    );
}

#[test]
fn a_created_collection_without_a_description_has_no_description_line() {
    assert_eq!(
        human_message(&Message::CollectionCreated(&info(
            None,
            Some("/Users/me/notes")
        ))),
        "Created the collection \"notes\" (directory).\n  path: /Users/me/notes\n"
    );
}

#[test]
fn a_changed_collection_shows_its_description_or_none() {
    assert_eq!(
        human_message(&Message::CollectionChanged(&info(
            None,
            Some("/Users/me/notes")
        ))),
        "Changed the collection \"notes\".\n  path: /Users/me/notes\n  description: (none)\n"
    );
}

#[test]
fn a_collection_removed_or_not() {
    assert_eq!(
        human_message(&Message::CollectionRemoved(&name("notes"))),
        "Removed the collection \"notes\".\n"
    );
    assert_eq!(
        human_message(&Message::CollectionNotRemoved(&name("notes"))),
        "Did not remove the collection \"notes\".\n"
    );
}

fn counts() -> UpdateReport {
    UpdateReport {
        added: 3,
        changed: 1,
        unchanged: 38,
        ..UpdateReport::default()
    }
}

#[test]
fn an_update_gives_the_six_counts_zeros_included() {
    assert_eq!(
        human_message(&Message::CollectionUpdated(&name("notes"), &counts())),
        "Updated the collection \"notes\": 3 added, 1 changed, 38 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
    );
    let all = UpdateReport {
        added: 5,
        changed: 1,
        unchanged: 120,
        deleted: 2,
        failed: 1,
        ..UpdateReport::default()
    };
    assert_eq!(
        human_message(&Message::CollectionsUpdated(3, &all)),
        "Updated 3 collections: 5 added, 1 changed, 120 unchanged, 0 re-ingested, 2 deleted, 1 failed.\n"
    );
    assert_eq!(
        human_message(&Message::CollectionsUpdated(1, &UpdateReport::default())),
        "Updated 1 collection: 0 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
    );
}

#[test]
fn a_stopped_update_gives_the_counts_up_to_where_it_stopped() {
    let stopped = UpdateReport {
        added: 2,
        unchanged: 10,
        ..UpdateReport::default()
    };

    assert_eq!(
        human_message(&Message::UpdateStopped(&stopped)),
        "Before it stopped: 2 added, 0 changed, 10 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
    );
}

#[test]
fn a_put_document_says_what_happened_to_it() {
    let put = |outcome| {
        human_message(&Message::DocumentPut(
            &name("notes"),
            &key("memo.md"),
            outcome,
        ))
    };

    assert_eq!(
        put(DocumentOutcome::Added),
        "Added \"memo.md\" to the collection \"notes\".\n"
    );
    assert_eq!(
        put(DocumentOutcome::Changed),
        "Changed \"memo.md\" in the collection \"notes\".\n"
    );
    assert_eq!(
        put(DocumentOutcome::Unchanged),
        "Skipped \"memo.md\" in the collection \"notes\": it has not changed.\n"
    );
    assert_eq!(
        put(DocumentOutcome::Reingested),
        "Ingested \"memo.md\" in the collection \"notes\" again, though it had not changed.\n"
    );
}

#[test]
fn a_removed_document_may_come_back_from_the_source() {
    let removed = |may_come_back| {
        human_message(&Message::DocumentRemoved {
            collection: &name("notes"),
            key: &key("memo.md"),
            may_come_back,
        })
    };

    assert_eq!(
        removed(false),
        "Removed \"memo.md\" from the collection \"notes\".\n"
    );
    assert_eq!(
        removed(true),
        "Removed \"memo.md\" from the collection \"notes\".\nIf it is still in the collection's source, it comes back at the next update.\n"
    );
    assert_eq!(
        human_message(&Message::DocumentNotRemoved(
            &name("notes"),
            &key("memo.md")
        )),
        "Did not remove \"memo.md\" from the collection \"notes\".\n"
    );
}

fn change(name: &str, before: serde_json::Value, after: serde_json::Value) -> SettingChange {
    SettingChange {
        name: config::check_setting(name, None).unwrap(),
        before,
        after,
    }
}

#[test]
fn a_changed_setting_gives_both_values_unquoted() {
    assert_eq!(
        human_message(&Message::SettingChanged(&change(
            "resources.memory_limit_mb",
            json!(8192),
            json!(4096)
        ))),
        "Changed resources.memory_limit_mb from 8192 to 4096.\n"
    );
    assert_eq!(
        human_message(&Message::SettingChanged(&change(
            "embedding.model",
            json!("cl-nagoya/ruri-v3-310m"),
            json!("cl-nagoya/ruri-v3-30m")
        ))),
        "Changed embedding.model from cl-nagoya/ruri-v3-310m to cl-nagoya/ruri-v3-30m.\n"
    );
}

#[test]
fn an_index_model_that_differs_says_what_to_run() {
    assert_eq!(
        human_message(&Message::IndexModelDiffers("cl-nagoya/ruri-v3-310m")),
        "The index's embeddings were made with another model (cl-nagoya/ruri-v3-310m).\nUntil you run mdya embedding rebuild, vector search finds nothing, and update and put fail.\n"
    );
}

// The examples that the user approved.
#[test]
fn an_index_model_that_cannot_be_compared_says_why_with_the_cause_and_the_fix() {
    assert_eq!(
        human_message(&Message::IndexModelUnknown {
            why: ModelUnknown::StatusUnreadable,
            cause: "the index cannot be read: denied",
            fix: &[],
        }),
        "Whether the index's embeddings were made with the new model is not known: the index status could not be read.\n  cause: the index cannot be read: denied\n"
    );
    assert_eq!(
        human_message(&Message::IndexModelUnknown {
            why: ModelUnknown::DimensionUnchecked,
            cause: "the model cannot be loaded",
            fix: &["Give an absolute path.", "Or move."],
        }),
        "Whether the index's embeddings were made with the new model is not known: its dimension could not be checked.\n  cause: the model cannot be loaded\n  Give an absolute path.\n  Or move.\n"
    );
}

#[test]
fn the_cause_of_an_unknown_index_model_has_its_controls_replaced() {
    let text = human_message(&Message::IndexModelUnknown {
        why: ModelUnknown::StatusUnreadable,
        cause: "a\nb\u{202e}",
        fix: &[],
    });

    assert_eq!(text.matches('\n').count(), 2, "{text}");
    assert!(!text.contains('\u{202e}'));
}

#[test]
fn rebuilding_and_maintaining() {
    assert_eq!(
        human_message(&Message::EmbeddingsRebuilt(1534)),
        "Rebuilt the embeddings of 1534 chunks.\n"
    );
    assert_eq!(
        human_message(&Message::EmbeddingsRebuilt(1)),
        "Rebuilt the embeddings of 1 chunk.\n"
    );
    assert_eq!(
        human_message(&Message::NothingRebuilt),
        "Nothing was rebuilt: the embeddings were already made with the configured model.\n"
    );
    assert_eq!(
        human_message(&Message::IndexMaintained),
        "Maintained the index.\n"
    );
}

#[test]
fn listening_gives_the_url_with_an_ipv6_address_in_brackets() {
    let v4: SocketAddr = "127.0.0.1:8000".parse().unwrap();
    let v6: SocketAddr = "[::1]:8000".parse().unwrap();

    assert_eq!(
        human_message(&Message::Listening(v4)),
        "Listening on http://127.0.0.1:8000/mcp\n"
    );
    assert_eq!(
        human_message(&Message::Listening(v6)),
        "Listening on http://[::1]:8000/mcp\n"
    );
}

#[test]
fn the_prompts_end_with_y_n_and_no_line_break() {
    assert_eq!(
        human_prompt(&Prompt::RemoveCollection(&name("notes"))),
        "Remove the collection \"notes\" and its documents from the index? [y/N] "
    );
    assert_eq!(
        human_prompt(&Prompt::RemoveDocument(&name("notes"), &key("memo.md"))),
        "Remove \"memo.md\" from the collection \"notes\"? [y/N] "
    );
}

#[test]
fn the_strings_in_messages_and_prompts_have_their_controls_replaced() {
    let message = human_message(&Message::DocumentPut(
        &name("n\u{1b}"),
        &key("k\u{202e}\n"),
        DocumentOutcome::Added,
    ));
    let prompt = human_prompt(&Prompt::RemoveCollection(&name("n\u{7}")));

    assert!(!message.contains(['\u{1b}', '\u{202e}']), "{message}");
    assert_eq!(message.matches('\n').count(), 1, "{message}");
    assert!(!prompt.contains('\u{7}'), "{prompt}");
}
