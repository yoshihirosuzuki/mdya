//! The operations' results in the human-readable output: each line ends with
//! a line break, and every string from outside has its control characters
//! and the characters that change the direction of text replaced. The
//! results have no colors.

use std::fmt::Write as _;

use serde_json::Value;

use super::super::operations::{
    CollectionList, IndexEmbeddingModel, SafeInteger, SearchOutput, SettingsOutput, StatusOutput,
};
use super::chars::replace_controls;
use super::numbers::UnsafeNumber;

#[cfg(test)]
mod tests;

/// The collections, one block each, separated by an empty line: the name,
/// the type, and the number of documents, then the description and the
/// type-specific attributes indented under them.
pub(crate) fn human_collection_list(list: &CollectionList) -> String {
    if list.collections.is_empty() {
        return "(no collections)\n".to_owned();
    }
    let blocks: Vec<String> = list
        .collections
        .iter()
        .map(|collection| {
            let mut block = format!(
                "{} ({}, {})\n",
                replace_controls(&collection.name),
                replace_controls(&collection.collection_type),
                documents(collection.document_count)
            );
            if let Some(description) = &collection.description {
                let _ = writeln!(block, "  {}", replace_controls(description));
            }
            for (name, value) in collection.attributes.get() {
                let _ = writeln!(block, "  {}: {}", replace_controls(name), value_text(value));
            }
            block
        })
        .collect();
    blocks.join("\n")
}

fn documents(count: SafeInteger) -> String {
    match count.get() {
        1 => "1 document".to_owned(),
        count => format!("{count} documents"),
    }
}

/// The hits, three lines each, separated by an empty line: the rank, the
/// collection, and the key; the score, rounded to 4 decimal places, and the
/// chunk; and the snippet.
pub(crate) fn human_search(output: &SearchOutput) -> Result<String, UnsafeNumber> {
    if output.hits.is_empty() {
        return Ok("(no hits)\n".to_owned());
    }
    let mut blocks = Vec::with_capacity(output.hits.len());
    for (rank, hit) in output.hits.iter().enumerate() {
        let score = hit.score.get();
        if !score.is_finite() {
            return Err(UnsafeNumber::NotFinite(score.to_string()));
        }
        blocks.push(format!(
            "{}. {}  {}\n   score {score:.4}, chunk {} of {}\n   {}\n",
            rank + 1,
            replace_controls(&hit.collection),
            replace_controls(&hit.key),
            hit.chunk_index.get(),
            hit.chunk_count.get(),
            replace_controls(&hit.snippet)
        ));
    }
    Ok(blocks.join("\n"))
}

/// The index status, one `name: value` line an item.
pub(crate) fn human_status(output: &StatusOutput) -> String {
    let mut text = format!("version: mdya {}\n", replace_controls(&output.version));
    let dimensions = match output.configured_embedding_dimension {
        Some(dimension) => format!("{} dimensions", dimension.get()),
        None => "dimensions unknown".to_owned(),
    };
    let _ = writeln!(
        text,
        "configured embedding model: {} ({dimensions})",
        replace_controls(&output.configured_embedding_model)
    );
    match output.index_embedding_models.as_slice() {
        [] => text.push_str("index embedding model: none\n"),
        models => {
            for model in models {
                text.push_str(&index_embedding_model(model));
            }
        }
    }
    let _ = writeln!(
        text,
        "chunks without embeddings: {}",
        output.chunks_without_embedding_count.get()
    );
    let _ = writeln!(text, "collections: {}", output.collection_count.get());
    let _ = writeln!(text, "documents: {}", output.document_count.get());
    let _ = writeln!(text, "chunks: {}", output.chunk_count.get());
    text
}

fn index_embedding_model(model: &IndexEmbeddingModel) -> String {
    let configured = match model.configured {
        Some(true) => "the configured one",
        Some(false) => "not the configured one",
        None => "unknown whether it is the configured one",
    };
    format!(
        "index embedding model: {} ({} dimensions, {configured})\n  chunks with embeddings: {} ({} truncated)\n",
        replace_controls(&model.model),
        model.dimension.get(),
        model.chunk_count.get(),
        model.truncated_chunk_count.get()
    )
}

/// The settings, one `name = value` line each, with strings as they are,
/// without quotes.
pub(crate) fn human_settings(output: &SettingsOutput) -> String {
    output
        .entries
        .iter()
        .map(|(name, value)| format!("{} = {}\n", name.as_str(), value_text(value.get())))
        .collect()
}

/// A JSON value as text: a string as it is (without quotes), anything else
/// as JSON. Control characters and the characters that change the direction
/// of text are replaced either way.
pub(super) fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => replace_controls(text).into_owned(),
        other => replace_controls(&other.to_string()).into_owned(),
    }
}
