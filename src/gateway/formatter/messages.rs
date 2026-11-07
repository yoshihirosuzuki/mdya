//! The messages and the confirmation prompts that the command line writes to
//! stderr, in the human-readable output: every string from outside has its
//! control characters and the characters that change the direction of text
//! replaced. The messages have no colors. Each message ends with a line
//! break; a prompt does not, so that the answer is typed after it.

use std::fmt::Write as _;
use std::net::SocketAddr;

use crate::collection::CollectionInfo;
use crate::config::SettingChange;
use crate::ingest::UpdateReport;
use crate::types::{CollectionName, DocumentOutcome, Key, TypeAttributes};

use super::chars::replace_controls;
use super::results::value_text;

#[cfg(test)]
mod tests;

/// What a message tells. The command line decides which to write.
#[derive(Debug)]
pub(crate) enum Message<'a> {
    CollectionCreated(&'a CollectionInfo),
    CollectionChanged(&'a CollectionInfo),
    CollectionRemoved(&'a CollectionName),
    CollectionNotRemoved(&'a CollectionName),
    /// One collection updated.
    CollectionUpdated(&'a CollectionName, &'a UpdateReport),
    /// Every collection updated: how many, and the counts summed over them.
    CollectionsUpdated(usize, &'a UpdateReport),
    /// The counts up to where an update stopped, after its error.
    UpdateStopped(&'a UpdateReport),
    DocumentPut(&'a CollectionName, &'a Key, DocumentOutcome),
    DocumentRemoved {
        collection: &'a CollectionName,
        key: &'a Key,
        /// Whether the collection can be updated, so that the document comes
        /// back at the next update if it is still in the source.
        may_come_back: bool,
    },
    DocumentNotRemoved(&'a CollectionName, &'a Key),
    SettingChanged(&'a SettingChange),
    /// After the embedding model setting changed: the index's embeddings
    /// were made with this other model.
    IndexModelDiffers(&'a str),
    /// After the embedding model setting changed: whether the index's
    /// embeddings were made with the new model is not known. `cause` is the
    /// error and its causes; `fix` is how to fix it, one sentence a line,
    /// and empty when nothing is to be fixed.
    IndexModelUnknown {
        why: ModelUnknown,
        cause: &'a str,
        fix: &'a [&'a str],
    },
    EmbeddingsRebuilt(u64),
    NothingRebuilt,
    IndexMaintained,
    /// Where the MCP server (HTTP) listens.
    Listening(SocketAddr),
}

/// The message as one or more lines.
pub(crate) fn human_message(message: &Message) -> String {
    match message {
        Message::CollectionCreated(info) => {
            let mut text = format!(
                "Created the collection {} ({}).\n",
                quoted(&info.name.0),
                replace_controls(&info.collection_type.0)
            );
            text.push_str(&attribute_lines(&info.attributes));
            if let Some(description) = &info.description {
                let _ = writeln!(text, "  description: {}", replace_controls(description));
            }
            text
        }
        Message::CollectionChanged(info) => {
            let mut text = format!("Changed the collection {}.\n", quoted(&info.name.0));
            text.push_str(&attribute_lines(&info.attributes));
            let description = match &info.description {
                Some(description) => replace_controls(description).into_owned(),
                None => "(none)".to_owned(),
            };
            let _ = writeln!(text, "  description: {description}");
            text
        }
        Message::CollectionRemoved(name) => {
            format!("Removed the collection {}.\n", quoted(&name.0))
        }
        Message::CollectionNotRemoved(name) => {
            format!("Did not remove the collection {}.\n", quoted(&name.0))
        }
        Message::CollectionUpdated(name, counts) => format!(
            "Updated the collection {}: {}.\n",
            quoted(&name.0),
            update_counts(counts)
        ),
        Message::CollectionsUpdated(collections, counts) => {
            let collections = match collections {
                1 => "1 collection".to_owned(),
                count => format!("{count} collections"),
            };
            format!("Updated {collections}: {}.\n", update_counts(counts))
        }
        Message::UpdateStopped(counts) => {
            format!("Before it stopped: {}.\n", update_counts(counts))
        }
        Message::DocumentPut(collection, key, outcome) => {
            let (key, collection) = (quoted(&key.0), quoted(&collection.0));
            match outcome {
                DocumentOutcome::Added => format!("Added {key} to the collection {collection}.\n"),
                DocumentOutcome::Changed => {
                    format!("Changed {key} in the collection {collection}.\n")
                }
                DocumentOutcome::Unchanged => {
                    format!("Skipped {key} in the collection {collection}: it has not changed.\n")
                }
                DocumentOutcome::Reingested => format!(
                    "Ingested {key} in the collection {collection} again, though it had not changed.\n"
                ),
            }
        }
        Message::DocumentRemoved {
            collection,
            key,
            may_come_back,
        } => {
            let mut text = format!(
                "Removed {} from the collection {}.\n",
                quoted(&key.0),
                quoted(&collection.0)
            );
            if *may_come_back {
                text.push_str(
                    "If it is still in the collection's source, it comes back at the next update.\n",
                );
            }
            text
        }
        Message::DocumentNotRemoved(collection, key) => format!(
            "Did not remove {} from the collection {}.\n",
            quoted(&key.0),
            quoted(&collection.0)
        ),
        Message::SettingChanged(change) => format!(
            "Changed {} from {} to {}.\n",
            change.name.as_str(),
            value_text(&change.before),
            value_text(&change.after)
        ),
        Message::IndexModelDiffers(model) => format!(
            "The index's embeddings were made with another model ({}).\nUntil you run mdya embedding rebuild, vector search finds nothing, and update and put fail.\n",
            replace_controls(model)
        ),
        Message::IndexModelUnknown { why, cause, fix } => {
            let why = match why {
                ModelUnknown::StatusUnreadable => "the index status could not be read",
                ModelUnknown::DimensionUnchecked => "its dimension could not be checked",
            };
            let mut text = format!(
                "Whether the index's embeddings were made with the new model is not known: {why}.\n  cause: {}\n",
                replace_controls(cause)
            );
            for line in fix.iter() {
                let _ = writeln!(text, "  {}", replace_controls(line));
            }
            text
        }
        Message::EmbeddingsRebuilt(chunks) => {
            format!("Rebuilt the embeddings of {}.\n", chunk_count(*chunks))
        }
        Message::NothingRebuilt => {
            "Nothing was rebuilt: the embeddings were already made with the configured model.\n"
                .to_owned()
        }
        Message::IndexMaintained => "Maintained the index.\n".to_owned(),
        Message::Listening(addr) => format!("Listening on http://{addr}/mcp\n"),
    }
}

/// Why it is not known whether the index's embeddings were made with the new
/// embedding model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelUnknown {
    StatusUnreadable,
    /// The new model's dimension could not be checked, and the names are the
    /// same.
    DimensionUnchecked,
}

/// A confirmation that the command line asks for before it removes
/// something.
#[derive(Debug)]
pub(crate) enum Prompt<'a> {
    RemoveCollection(&'a CollectionName),
    RemoveDocument(&'a CollectionName, &'a Key),
}

/// The prompt, without a line break at its end.
pub(crate) fn human_prompt(prompt: &Prompt) -> String {
    match prompt {
        Prompt::RemoveCollection(name) => format!(
            "Remove the collection {} and its documents from the index? [y/N] ",
            quoted(&name.0)
        ),
        Prompt::RemoveDocument(collection, key) => format!(
            "Remove {} from the collection {}? [y/N] ",
            quoted(&key.0),
            quoted(&collection.0)
        ),
    }
}

/// The text in double quotes, with its control characters and the
/// characters that change the direction of text replaced.
fn quoted(text: &str) -> String {
    format!("\"{}\"", replace_controls(text))
}

/// The type-specific attributes, one indented `name: value` line each.
fn attribute_lines(attributes: &TypeAttributes) -> String {
    attributes
        .0
        .iter()
        .map(|(name, value)| format!("  {}: {}\n", replace_controls(name), value_text(value)))
        .collect()
}

/// The six counts of an update, in one order, zeros included.
fn update_counts(counts: &UpdateReport) -> String {
    format!(
        "{} added, {} changed, {} unchanged, {} re-ingested, {} deleted, {} failed",
        counts.added,
        counts.changed,
        counts.unchanged,
        counts.reingested,
        counts.deleted,
        counts.failed
    )
}

fn chunk_count(chunks: u64) -> String {
    match chunks {
        1 => "1 chunk".to_owned(),
        chunks => format!("{chunks} chunks"),
    }
}
