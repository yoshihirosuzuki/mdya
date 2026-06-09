//! Read-only introspection of the local index, backing `mdya collection
//! list` and `mdya status`. This module *gathers* the data; [`output`]
//! renders it.
//!
//! The report types are the CLI / MCP shared shape that the MCP
//! `list_collections` / `get_status` tools mirror, so they live in the
//! library (not the CLI) and serialise through serde — `mdya search`
//! ⇄ `search_*` works the same way (`search::SearchResponse`).

pub mod output;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arrow_array::cast::AsArray;
use arrow_array::{Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::Table;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{self, ConfigError};
use crate::store::metadata_check::{METADATA_KEY_EMBEDDING_MODEL, METADATA_KEY_VECTOR_DIM};
use crate::store::{CHUNKS_TABLE_NAME, COL_COLLECTION, SOURCES_TABLE_NAME};

#[derive(Debug, Error)]
pub enum IntrospectError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("LanceDB path {path} is not valid UTF-8")]
    LancedbPathNotUtf8 { path: PathBuf },

    #[error("connect LanceDB at {path}: {source}")]
    LancedbConnect {
        path: PathBuf,
        #[source]
        source: lancedb::Error,
    },

    #[error("open {table} table: {source}")]
    OpenTable {
        table: &'static str,
        #[source]
        source: lancedb::Error,
    },

    #[error("query {table} table: {source}")]
    Query {
        table: &'static str,
        #[source]
        source: lancedb::Error,
    },

    #[error("read chunks table schema: {0}")]
    Schema(#[source] lancedb::Error),

    /// The chunks table exists but its `Schema::metadata` lacks one or both
    /// pin keys. Mirrors `SearchError::SchemaMetadataMissing` so
    /// `mdya status` fails the same loud way `mdya search` does.
    #[error(
        "schema metadata pin missing: chunks table's Schema::metadata lacks \
         key(s) {absent_keys:?}. Remove `~/.mdya/index/`, run `mdya init`, \
         and re-run `mdya update-all` to rebuild."
    )]
    SchemaMetadataMissing { absent_keys: Vec<&'static str> },

    #[error("chunks table vector_dim pin is not an integer: {value:?}")]
    MalformedVectorDimPin { value: String },
}

/// One row of `mdya collection list`: a registered collection plus the
/// number of documents (rows in the `sources` table) under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CollectionInfo {
    pub name: String,
    pub path: String,
    /// `null` when the collection was added without `--description`. Kept
    /// always-present (not skipped) so the JSON shape is stable for machine
    /// consumers and the mirroring MCP tool.
    pub description: Option<String>,
    pub document_count: u64,
}

/// Envelope for `mdya collection list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CollectionListReport {
    pub collections: Vec<CollectionInfo>,
}

/// Output of `mdya status`: index health from `config.yml`, the chunks
/// table's schema pin, and the two tables' row counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusReport {
    pub version: String,
    pub embedding_model: String,
    pub vector_dim: u32,
    pub collections: u64,
    pub chunks: u64,
    pub sources: u64,
}

/// Gather the collection list: declared collections from `config.yml`, each
/// joined with its document count from the `sources` table.
pub async fn collection_list(
    config_dir: Option<&Path>,
) -> Result<CollectionListReport, IntrospectError> {
    let base = config::resolve_config_dir(config_dir)?;
    let cfg = config::load(&base.join("config.yml"))?;
    let counts = document_counts(&base).await?;
    let collections = cfg
        .collections
        .into_iter()
        .map(|(name, entry)| CollectionInfo {
            document_count: counts.get(&name).copied().unwrap_or(0),
            name,
            path: entry.path,
            description: entry.description,
        })
        .collect();
    Ok(CollectionListReport { collections })
}

/// Gather index status. Opens the chunks table (which carries the schema
/// pin) and the sources table; a missing pin is a loud error, matching
/// `mdya search`.
pub async fn status(config_dir: Option<&Path>) -> Result<StatusReport, IntrospectError> {
    let base = config::resolve_config_dir(config_dir)?;
    let cfg = config::load(&base.join("config.yml"))?;
    let chunks = open_table(&base, CHUNKS_TABLE_NAME).await?;
    let (embedding_model, vector_dim) = read_pins(&chunks).await?;
    let sources = open_table(&base, SOURCES_TABLE_NAME).await?;
    Ok(StatusReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        embedding_model,
        vector_dim,
        collections: cfg.collections.len() as u64,
        chunks: count_rows(&chunks, CHUNKS_TABLE_NAME).await?,
        sources: count_rows(&sources, SOURCES_TABLE_NAME).await?,
    })
}

/// Count `sources` rows per collection in a single scan, grouped in Rust so
/// no per-collection SQL predicate string is built — collection names never
/// reach a hand-assembled query string.
async fn document_counts(base: &Path) -> Result<HashMap<String, u64>, IntrospectError> {
    let table = open_table(base, SOURCES_TABLE_NAME).await?;
    let stream = table
        .query()
        .select(Select::Columns(vec![COL_COLLECTION.to_string()]))
        .execute()
        .await
        .map_err(|source| IntrospectError::Query {
            table: SOURCES_TABLE_NAME,
            source,
        })?;
    let batches =
        stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(|source| IntrospectError::Query {
                table: SOURCES_TABLE_NAME,
                source,
            })?;
    Ok(tally_collections(&batches))
}

fn tally_collections(batches: &[RecordBatch]) -> HashMap<String, u64> {
    let mut counts = HashMap::new();
    for batch in batches {
        let column: &StringArray = batch
            .column_by_name(COL_COLLECTION)
            .expect("sources batch is missing the collection column despite the explicit select")
            .as_string();
        for i in 0..column.len() {
            if column.is_valid(i) {
                *counts.entry(column.value(i).to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

async fn read_pins(chunks: &Table) -> Result<(String, u32), IntrospectError> {
    let schema = chunks.schema().await.map_err(IntrospectError::Schema)?;
    let meta = schema.metadata();
    let model = meta.get(METADATA_KEY_EMBEDDING_MODEL);
    let dim = meta.get(METADATA_KEY_VECTOR_DIM);
    let absent = absent_pin_keys(model, dim);
    if !absent.is_empty() {
        return Err(IntrospectError::SchemaMetadataMissing {
            absent_keys: absent,
        });
    }
    let model = model
        .expect("the absent_pin_keys empty-check above guarantees the model key is present")
        .clone();
    let dim = dim.expect("the absent_pin_keys empty-check above guarantees the dim key is present");
    let vector_dim = dim
        .parse()
        .map_err(|_| IntrospectError::MalformedVectorDimPin { value: dim.clone() })?;
    Ok((model, vector_dim))
}

fn absent_pin_keys(model: Option<&String>, dim: Option<&String>) -> Vec<&'static str> {
    let mut absent = Vec::new();
    if model.is_none() {
        absent.push(METADATA_KEY_EMBEDDING_MODEL);
    }
    if dim.is_none() {
        absent.push(METADATA_KEY_VECTOR_DIM);
    }
    absent
}

async fn count_rows(table: &Table, name: &'static str) -> Result<u64, IntrospectError> {
    table
        .count_rows(None)
        .await
        .map(|n| n as u64)
        .map_err(|source| IntrospectError::Query {
            table: name,
            source,
        })
}

async fn open_table(base: &Path, name: &'static str) -> Result<Table, IntrospectError> {
    let index_dir = base.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| IntrospectError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    let db = lancedb::connect(index_str)
        .execute()
        .await
        .map_err(|source| IntrospectError::LancedbConnect {
            path: index_dir.clone(),
            source,
        })?;
    db.open_table(name)
        .execute()
        .await
        .map_err(|source| IntrospectError::OpenTable {
            table: name,
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::RecordBatch;
    use std::sync::Arc;

    fn sources_batch(collections: &[&str]) -> RecordBatch {
        let schema = arrow_schema::Schema::new(vec![arrow_schema::Field::new(
            COL_COLLECTION,
            arrow_schema::DataType::Utf8,
            false,
        )]);
        let column = StringArray::from(collections.to_vec());
        RecordBatch::try_new(Arc::new(schema), vec![Arc::new(column)]).expect("batch")
    }

    #[test]
    fn tally_counts_documents_per_collection() {
        let batch = sources_batch(&["notes", "notes", "docs"]);
        let counts = tally_collections(&[batch]);
        assert_eq!(counts.get("notes"), Some(&2));
        assert_eq!(counts.get("docs"), Some(&1));
        assert_eq!(counts.get("absent"), None);
    }

    #[test]
    fn absent_pin_keys_lists_both_when_metadata_empty() {
        assert_eq!(
            absent_pin_keys(None, None),
            vec![METADATA_KEY_EMBEDDING_MODEL, METADATA_KEY_VECTOR_DIM]
        );
    }

    #[test]
    fn absent_pin_keys_is_empty_when_both_present() {
        let model = "ruri".to_string();
        let dim = "256".to_string();
        assert!(absent_pin_keys(Some(&model), Some(&dim)).is_empty());
    }

    #[test]
    fn collection_info_json_keeps_null_description_present() {
        let info = CollectionInfo {
            name: "notes".to_string(),
            path: "~/notes".to_string(),
            description: None,
            document_count: 0,
        };
        let value = serde_json::to_value(&info).expect("serialize");
        assert_eq!(value["description"], serde_json::Value::Null);
        assert_eq!(value["document_count"], 0);
    }

    #[test]
    fn status_report_json_shape_is_six_flat_fields() {
        let report = StatusReport {
            version: "0.3.0".to_string(),
            embedding_model: "cl-nagoya/ruri-v3-30m".to_string(),
            vector_dim: 256,
            collections: 3,
            chunks: 12,
            sources: 4,
        };
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["version"], "0.3.0");
        assert_eq!(value["vector_dim"], 256);
        assert_eq!(value["chunks"], 12);
    }
}
