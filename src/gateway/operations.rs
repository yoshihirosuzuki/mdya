//! Carries out each operation by calling the components in order, for both
//! the command line and the MCP server.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;

use crate::collection::{CollectionChange, CollectionError, CollectionInfo, Collections};
use crate::config::{Config, ConfigError, SettingChange, SettingName};
use crate::embedding::{Embedding, EmbeddingSettings};
use crate::index::{Index, IndexError, IndexStatus};
use crate::ingest::{Ingest, IngestError, UpdateReport};
use crate::query::{ChunkRangeRequest, Query, QueryError, SearchMode, SearchUnit};
use crate::types::{
    CollectionName, CollectionType, DocumentOutcome, Key, ProgressSink, TypeAttributes,
};

use super::write_lock::{WriteLock, WriteLockError};

#[cfg(test)]
pub(crate) mod test_double;

/// The operations, one function each. `cli` and `mcp` call them; a trait, so
/// that their tests can replace it.
#[async_trait]
pub(crate) trait Operations: Send + Sync {
    async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CollectionInfo, OperationError>;
    async fn delete_collection(
        &self,
        input: DeleteCollectionInput,
    ) -> Result<DeleteCollectionOutcome, OperationError>;
    async fn list_collections(&self) -> Result<CollectionList, OperationError>;
    async fn change_collection(
        &self,
        input: ChangeCollectionInput,
    ) -> Result<CollectionInfo, OperationError>;
    async fn update_collections(&self, input: UpdateInput)
    -> Result<UpdateOutcome, OperationError>;
    async fn update_all(&self, input: UpdateAllInput) -> Result<UpdateOutcome, OperationError>;
    async fn put_document(&self, input: PutInput) -> Result<PutOutcome, OperationError>;
    async fn delete_document(
        &self,
        input: DeleteDocumentInput,
    ) -> Result<DeleteDocumentOutcome, OperationError>;
    async fn search(&self, input: SearchInput) -> Result<SearchOutput, OperationError>;
    async fn get_document(&self, input: GetDocumentInput) -> Result<DocumentText, OperationError>;
    async fn status(&self) -> Result<StatusOutput, OperationError>;
    async fn list_settings(&self) -> Result<SettingsOutput, OperationError>;
    async fn change_setting(
        &self,
        input: ChangeSettingInput,
    ) -> Result<SettingChangeOutcome, OperationError>;
    async fn rebuild_embeddings(
        &self,
        input: RebuildInput,
    ) -> Result<RebuildOutcome, OperationError>;
    async fn optimize(&self, input: OptimizeInput) -> Result<(), OperationError>;
}

/// The operation that shows mdya's version. It uses no component, so it is
/// not part of [`Operations`].
pub(crate) fn version() -> VersionOutput {
    VersionOutput {
        version: format!("mdya {}", env!("CARGO_PKG_VERSION")),
    }
}

/// Creates the operations over the assembled components.
pub(crate) fn operations(parts: OperationParts) -> Arc<dyn Operations> {
    let _ = parts;
    todo!()
}

/// What the operations are created with.
pub(crate) struct OperationParts {
    pub(crate) config: Arc<dyn Config>,
    /// The one for the configured embedding model.
    pub(crate) embedding: Arc<dyn Embedding>,
    pub(crate) index: Arc<dyn Index>,
    pub(crate) collections: Arc<dyn Collections>,
    pub(crate) query: Arc<dyn Query>,
    /// The ingest implementation for each collection type.
    pub(crate) ingests: BTreeMap<CollectionType, Arc<dyn Ingest>>,
    /// Creates the `embedding` for a newly set embedding model.
    pub(crate) create_embedding: Arc<dyn Fn(EmbeddingSettings) -> Arc<dyn Embedding> + Send + Sync>,
    pub(crate) model_cache_dir: PathBuf,
    pub(crate) hf_token: Option<String>,
    /// How many documents to process at once (1 to 1024).
    pub(crate) parallelism: usize,
    pub(crate) write_lock: Arc<dyn WriteLock>,
}

/// Receives progress, and also when index maintenance starts. Passed to the
/// components as a [`ProgressSink`].
pub(crate) trait GatewayProgressSink: ProgressSink {
    /// Index maintenance has started. Returns immediately.
    fn maintenance_started(&self);
}

/// Asks the user to confirm. `cli` creates one for each operation that asks.
#[async_trait]
pub(crate) trait Confirmation: Send + Sync {
    /// Whether the user confirmed.
    async fn confirm(&self) -> bool;
}

/// Reads the content of the document to ingest, from a file or stdin.
#[async_trait]
pub(crate) trait ContentReader: Send + Sync {
    async fn read(&self) -> Result<Vec<u8>, std::io::Error>;
}

pub(crate) struct CreateCollectionInput {
    pub(crate) name: CollectionName,
    pub(crate) collection_type: CollectionType,
    pub(crate) description: Option<String>,
    /// As the user gave them.
    pub(crate) attributes: TypeAttributes,
}

pub(crate) struct DeleteCollectionInput {
    pub(crate) name: CollectionName,
    pub(crate) confirmation: Box<dyn Confirmation>,
    /// Whether to maintain the index at the end. False only with
    /// `--no-optimize`; the same in the other write operations.
    pub(crate) optimize: bool,
}

pub(crate) struct ChangeCollectionInput {
    pub(crate) name: CollectionName,
    /// The type-specific attributes in it are as the user gave them.
    pub(crate) change: CollectionChange,
}

pub(crate) struct UpdateInput {
    /// One or more.
    pub(crate) collections: Vec<CollectionName>,
    pub(crate) overwrite: bool,
    pub(crate) progress: Arc<dyn GatewayProgressSink>,
    pub(crate) optimize: bool,
}

pub(crate) struct UpdateAllInput {
    pub(crate) overwrite: bool,
    pub(crate) progress: Arc<dyn GatewayProgressSink>,
    pub(crate) optimize: bool,
}

pub(crate) struct PutInput {
    pub(crate) collection: CollectionName,
    pub(crate) key: Key,
    /// The MIME type or alias the user gave.
    pub(crate) media_type: String,
    pub(crate) content: Box<dyn ContentReader>,
    pub(crate) overwrite: bool,
    pub(crate) optimize: bool,
}

pub(crate) struct DeleteDocumentInput {
    pub(crate) collection: CollectionName,
    pub(crate) key: Key,
    pub(crate) confirmation: Box<dyn Confirmation>,
    pub(crate) optimize: bool,
}

pub(crate) struct SearchInput {
    pub(crate) query: String,
    pub(crate) mode: SearchMode,
    /// Empty means all collections.
    pub(crate) collections: Vec<CollectionName>,
    pub(crate) limit: SearchLimit,
    pub(crate) unit: SearchUnit,
}

/// The search limit as the user gave it. What it can hold is up to the
/// command line and the MCP server, which read it; the operation passes it on
/// as a `usize`, capped at `usize::MAX`, and returns it in the result as
/// given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub(crate) struct SearchLimit(u64);

impl SearchLimit {
    pub(crate) fn new(limit: u64) -> Self {
        Self(limit)
    }

    /// The limit to pass to `query`.
    pub(crate) fn to_usize(&self) -> usize {
        todo!()
    }
}

pub(crate) struct GetDocumentInput {
    pub(crate) collection: CollectionName,
    pub(crate) key: Key,
    pub(crate) range: ChunkRangeRequest,
    /// The output size limit to apply, in bytes. `None` applies none.
    pub(crate) size_limit: Option<u64>,
}

#[derive(Debug)]
pub(crate) enum ChangeSettingInput {
    Set { name: String, value: String },
    Reset { name: String },
}

pub(crate) struct RebuildInput {
    /// Whether to rebuild even when the index's embedding model is the
    /// configured one.
    pub(crate) include_configured_model: bool,
    pub(crate) progress: Arc<dyn GatewayProgressSink>,
    pub(crate) optimize: bool,
}

pub(crate) struct OptimizeInput {
    pub(crate) progress: Arc<dyn GatewayProgressSink>,
}

/// The result of listing the collections, in the JSON structure that
/// `mdya collection list --json` writes and the `list_collections` tool returns.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub(crate) struct CollectionList {
    pub(crate) collections: Vec<CollectionEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub(crate) struct CollectionEntry {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) collection_type: String,
    pub(crate) description: Option<String>,
    pub(crate) document_count: u64,
    pub(crate) attributes: serde_json::Map<String, serde_json::Value>,
}

/// The result of a search, in the JSON structure that `mdya search --json`
/// writes and the `search` tool returns.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub(crate) struct SearchOutput {
    pub(crate) query: String,
    pub(crate) mode: SearchModeName,
    pub(crate) unit: SearchUnitName,
    pub(crate) collections: Vec<String>,
    pub(crate) limit: SearchLimit,
    pub(crate) hit_count: u64,
    pub(crate) hits: Vec<SearchHit>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchModeName {
    Fts,
    Vector,
    Hybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchUnitName {
    Document,
    Chunk,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub(crate) struct SearchHit {
    pub(crate) collection: String,
    pub(crate) key: String,
    pub(crate) score: f64,
    pub(crate) snippet: String,
    pub(crate) chunk_index: u64,
    pub(crate) chunk_count: u64,
}

/// The result of getting a document, in the JSON structure of the MCP tool's
/// result. The command line writes only `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct DocumentText {
    pub(crate) collection: String,
    pub(crate) key: String,
    pub(crate) text: String,
}

/// The index status, in the JSON structure that `mdya status --json` writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct StatusOutput {
    /// Without the leading `mdya`.
    pub(crate) version: String,
    pub(crate) configured_embedding_model: String,
    /// `None` when it could not be checked.
    pub(crate) configured_embedding_dimension: Option<u64>,
    pub(crate) index_embedding_models: Vec<IndexEmbeddingModel>,
    pub(crate) chunks_without_embedding_count: u64,
    pub(crate) collection_count: u64,
    pub(crate) document_count: u64,
    pub(crate) chunk_count: u64,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct IndexEmbeddingModel {
    pub(crate) model: String,
    pub(crate) dimension: u64,
    /// `None` when it could not be checked.
    pub(crate) configured: Option<bool>,
    pub(crate) chunk_count: u64,
    pub(crate) truncated_chunk_count: u64,
}

/// The settings and their current values, in the order the settings
/// operation shows them. Written as one JSON object whose member names are the
/// setting names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsOutput {
    pub(crate) entries: Vec<(SettingName, serde_json::Value)>,
}

impl Serialize for SettingsOutput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let _ = serializer;
        todo!()
    }
}

impl JsonSchema for SettingsOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        todo!()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let _ = generator;
        todo!()
    }
}

/// The result of the version operation, in the JSON structure of the MCP
/// tool's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct VersionOutput {
    /// `mdya` and the version, such as `mdya 1.2.3`.
    pub(crate) version: String,
}

/// An abnormal case that does not end an operation with an error but makes
/// the command line exit with code 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutcomeAnomaly {
    /// The user declined to confirm.
    Declined,
    /// Some documents could not be read.
    DocumentsFailed,
    /// A source, or part of one, could not be read.
    SourcesUnreadable,
    /// The index status could not be read after the operation.
    StatusUnreadable,
}

#[derive(Debug)]
pub(crate) enum DeleteCollectionOutcome {
    Deleted { collection: CollectionName },
    Declined,
}

#[derive(Debug)]
pub(crate) struct UpdateOutcome {
    /// Summed over the collections.
    pub(crate) counts: UpdateReport,
    /// The collections skipped because their source could not be read.
    pub(crate) unreadable_sources: Vec<CollectionName>,
    /// Why the index status could not be read after the operation.
    pub(crate) status_error: Option<IndexError>,
}

#[derive(Debug)]
pub(crate) struct PutOutcome {
    pub(crate) collection: CollectionName,
    pub(crate) key: Key,
    pub(crate) outcome: DocumentOutcome,
    pub(crate) status_error: Option<IndexError>,
}

#[derive(Debug)]
pub(crate) enum DeleteDocumentOutcome {
    Deleted {
        collection: CollectionName,
        key: Key,
        /// Whether the collection's type can be updated, so that the
        /// document may come back.
        supports_update: bool,
    },
    Declined,
}

#[derive(Debug)]
pub(crate) struct SettingChangeOutcome {
    pub(crate) change: SettingChange,
    /// Only when the embedding model setting changed: the index status
    /// against the new model, or why it could not be read.
    pub(crate) index_status: Option<Result<IndexStatus, IndexError>>,
}

#[derive(Debug)]
pub(crate) struct RebuildOutcome {
    /// 0 when nothing was rebuilt.
    pub(crate) rebuilt_chunks: u64,
    pub(crate) status_error: Option<IndexError>,
}

impl DeleteCollectionOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

impl UpdateOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

impl PutOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

impl DeleteDocumentOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

impl SettingChangeOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

impl RebuildOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        todo!()
    }
}

/// The error of an operation. One type for all of them, so that
/// `error_report` maps every case in one place.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OperationError {
    #[error("the collection does not exist")]
    CollectionNotFound { collection: CollectionName },
    #[error("the write lock was not taken")]
    WriteLock(#[source] WriteLockError),
    #[error("the output is too large")]
    OutputTooLarge { size_bytes: u64, limit_bytes: u64 },
    #[error("the document content cannot be read")]
    ContentUnreadable { source: std::io::Error },
    #[error("the collection component failed")]
    Collection {
        context: ErrorContext,
        source: CollectionError,
    },
    #[error("the index failed")]
    Index {
        context: ErrorContext,
        source: IndexError,
    },
    #[error("the query failed")]
    Query {
        context: ErrorContext,
        source: QueryError,
    },
    #[error("the ingest implementation failed")]
    Ingest {
        context: ErrorContext,
        source: IngestError,
    },
    #[error("the settings failed")]
    Config(#[source] ConfigError),
    /// `counts` includes the collections whose update had finished.
    #[error("the update stopped")]
    UpdateStopped {
        counts: UpdateReport,
        context: ErrorContext,
        source: IngestError,
    },
}

/// The collection name and key passed to the component function that failed.
/// `None` for what was not passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ErrorContext {
    pub(crate) collection: Option<CollectionName>,
    pub(crate) key: Option<Key>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_mdya_and_the_package_version() {
        assert_eq!(
            version(),
            VersionOutput {
                version: format!("mdya {}", env!("CARGO_PKG_VERSION"))
            }
        );
    }

    #[test]
    fn the_version_output_is_written_as_the_mcp_tool_result() {
        let json = serde_json::to_value(VersionOutput {
            version: "mdya 1.2.3".to_owned(),
        })
        .unwrap();

        assert_eq!(json, serde_json::json!({ "version": "mdya 1.2.3" }));
    }
}
