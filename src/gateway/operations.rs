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
use crate::path::MakePathAbsoluteError;
use crate::query::{ChunkRangeRequest, Query, QueryError, SearchMode, SearchUnit};
use crate::types::{
    CollectionName, CollectionType, DirLookup, DocumentOutcome, Key, ProgressSink, TypeAttributes,
};

use super::write_lock::{WriteLock, WriteLockError};

mod assembled;
mod numbers;

pub(crate) use numbers::{MAX_SAFE_INTEGER, SafeInteger, SafeJsonObject, SafeJsonValue, Score};
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
    async fn update_collection(&self, input: UpdateInput) -> Result<UpdateOutcome, OperationError>;
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
    Arc::new(assembled::AssembledOperations::new(parts))
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
    /// Passed on in the [`EmbeddingSettings`] for a newly set embedding
    /// model, with its error boxed, where [`model_cache_dir_undetermined`]
    /// finds it.
    pub(crate) model_cache_dir:
        Arc<dyn Fn() -> Result<PathBuf, ModelCacheDirUndetermined> + Send + Sync>,
    pub(crate) hf_token: Option<String>,
    /// Passed to the ingest implementations to make the places in
    /// type-specific attributes absolute.
    pub(crate) dirs: DirLookup,
    /// How many documents to process at once (1 to 1024).
    pub(crate) parallelism: usize,
    pub(crate) write_lock: Arc<dyn WriteLock>,
}

/// The embedding model cache directory (`--model-cache-dir`) could not be
/// made absolute. Kept here, where the operations can find it in a cause
/// chain, as `error_report` uses the operations and not the other way round.
#[derive(Debug, thiserror::Error)]
#[error("the embedding model cache directory cannot be determined")]
pub(crate) struct ModelCacheDirUndetermined {
    #[source]
    pub(crate) source: MakePathAbsoluteError,
}

/// How to fix an embedding model cache directory that could not be
/// determined, one sentence a line. Added to the warning about a dimension
/// that could not be checked and to the explanation of an unusable embedding
/// model. The command line is to add them to its message after the embedding
/// model setting is changed, too.
pub(crate) const MODEL_CACHE_DIR_FIX: [&str; 3] = [
    "Give an absolute path for the embedding model cache directory with --model-cache-dir, then run the operation again.",
    "If the working directory cannot be read, you can also move to a working directory that mdya can read, and run the operation again.",
    "For the MCP server, start it again after either of these.",
];

/// Whether the cause chain from `error` has the embedding model cache
/// directory that could not be determined.
pub(crate) fn model_cache_dir_undetermined(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |error| error.source())
        .any(|error| error.is::<ModelCacheDirUndetermined>())
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
    pub(crate) collection: CollectionName,
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

/// The search limit as the user gave it: a whole number from 1 to
/// [`SearchLimit::MAX`]. The operation passes it on as a `usize`, capped at
/// `usize::MAX`, and returns it in the result as given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct SearchLimit(u64);

impl SearchLimit {
    /// The largest search limit: 2^53 − 1. JSON implementations agree exactly
    /// on the value of an integer up to it (RFC 8259, section 6), and the
    /// result returns the limit as given.
    pub(crate) const MAX: u64 = MAX_SAFE_INTEGER;

    /// Panics when `limit` is 0 or larger than [`SearchLimit::MAX`]. The
    /// command line and the MCP server must turn those away as errors before
    /// they make a search limit: such a limit here is a bug in them.
    #[expect(clippy::panic)]
    pub(crate) fn new(limit: u64) -> Self {
        if !(1..=Self::MAX).contains(&limit) {
            panic!("the search limit {limit} is not from 1 to 2^53 - 1");
        }
        Self(limit)
    }

    /// The limit to pass to `query`.
    pub(crate) fn to_usize(&self) -> usize {
        capped(self.0, usize::MAX)
    }
}

/// A whole number from 1 to [`SearchLimit::MAX`].
impl JsonSchema for SearchLimit {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SearchLimit".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer", "minimum": 1, "maximum": SearchLimit::MAX })
    }
}

/// `value` as a `T`, or `max` when a `T` cannot hold it.
fn capped<T: TryFrom<u64>>(value: u64, max: T) -> T {
    T::try_from(value).unwrap_or(max)
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
    pub(crate) document_count: SafeInteger,
    pub(crate) attributes: SafeJsonObject,
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
    pub(crate) hit_count: SafeInteger,
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
    pub(crate) score: Score,
    pub(crate) snippet: String,
    pub(crate) chunk_index: SafeInteger,
    pub(crate) chunk_count: SafeInteger,
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
    pub(crate) configured_embedding_dimension: Option<SafeInteger>,
    pub(crate) index_embedding_models: Vec<IndexEmbeddingModel>,
    pub(crate) chunks_without_embedding_count: SafeInteger,
    pub(crate) collection_count: SafeInteger,
    pub(crate) document_count: SafeInteger,
    pub(crate) chunk_count: SafeInteger,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct IndexEmbeddingModel {
    pub(crate) model: String,
    pub(crate) dimension: SafeInteger,
    /// `None` when it could not be checked.
    pub(crate) configured: Option<bool>,
    pub(crate) chunk_count: SafeInteger,
    pub(crate) truncated_chunk_count: SafeInteger,
}

/// The settings and their current values, in the order the settings
/// operation shows them. Written as one JSON object whose member names are the
/// setting names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsOutput {
    pub(crate) entries: Vec<(SettingName, SafeJsonValue)>,
}

impl Serialize for SettingsOutput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            self.entries
                .iter()
                .map(|(name, value)| (name.as_str(), value)),
        )
    }
}

/// An object whose members are not named: the settings operation has no MCP
/// tool, so no client reads this schema to find them.
impl JsonSchema for SettingsOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SettingsOutput".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "object" })
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

/// The anomalies whose condition holds, in the order given. Each `anomalies`
/// gives them in the order `OutcomeAnomaly` declares them, each once.
fn anomalies_where<const N: usize>(conditions: [(OutcomeAnomaly, bool); N]) -> Vec<OutcomeAnomaly> {
    conditions
        .into_iter()
        .filter_map(|(anomaly, holds)| holds.then_some(anomaly))
        .collect()
}

impl DeleteCollectionOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([(OutcomeAnomaly::Declined, matches!(self, Self::Declined))])
    }
}

impl UpdateOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([
            (OutcomeAnomaly::DocumentsFailed, self.counts.failed > 0),
            (
                OutcomeAnomaly::SourcesUnreadable,
                !self.unreadable_sources.is_empty() || self.counts.unreadable_places > 0,
            ),
            (
                OutcomeAnomaly::StatusUnreadable,
                self.status_error.is_some(),
            ),
        ])
    }
}

impl PutOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([(
            OutcomeAnomaly::StatusUnreadable,
            self.status_error.is_some(),
        )])
    }
}

impl DeleteDocumentOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([(OutcomeAnomaly::Declined, matches!(self, Self::Declined))])
    }
}

impl SettingChangeOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([(
            OutcomeAnomaly::StatusUnreadable,
            matches!(self.index_status, Some(Err(_))),
        )])
    }
}

impl RebuildOutcome {
    pub(crate) fn anomalies(&self) -> Vec<OutcomeAnomaly> {
        anomalies_where([(
            OutcomeAnomaly::StatusUnreadable,
            self.status_error.is_some(),
        )])
    }
}

/// The error of an operation. One type for all of them, so that
/// `error_report` maps every case in one place.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OperationError {
    /// No collection has the name.
    #[error("the collection does not exist")]
    CollectionNotFound { collection: CollectionName },
    /// The collection's type has no update.
    #[error("the collection cannot be updated")]
    UpdateNotSupported { collection: CollectionName },
    /// The collection's type does not take document contents.
    #[error("the collection does not take document contents")]
    PutNotSupported { collection: CollectionName },
    /// The collection has no document with the key.
    #[error("the document does not exist")]
    DocumentNotFound {
        collection: CollectionName,
        key: Key,
    },
    #[error("the write lock was not taken")]
    WriteLock(#[source] WriteLockError),
    #[error("the output is too large")]
    OutputTooLarge {
        part: OutputPart,
        size_bytes: u64,
        limit_bytes: u64,
    },
    /// The collection's stored type has no ingest implementation in this
    /// version of mdya.
    #[error("the collection type is unknown")]
    UnknownCollectionType {
        collection: CollectionName,
        collection_type: CollectionType,
    },
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
    /// `counts` is summed over the collections updated so far: those finished,
    /// those skipped because their source could not be read, and the one the
    /// update stopped in.
    #[error("the update stopped")]
    UpdateStopped {
        counts: UpdateReport,
        context: ErrorContext,
        source: IngestError,
    },
}

/// What [`OperationError::OutputTooLarge`] found too large.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputPart {
    /// The document's whole body.
    Body,
    /// One chunk.
    Chunk,
    /// A range of chunks, with its end given.
    ChunkRange,
}

impl OutputPart {
    /// What the range asks for.
    pub(crate) fn of(range: ChunkRangeRequest) -> Self {
        match (range.start, range.end) {
            // An end without a start is not a valid range; it is taken as
            // the body, which is what is asked for without a start.
            (None, _) => Self::Body,
            (Some(_), None) => Self::Chunk,
            (Some(_), Some(_)) => Self::ChunkRange,
        }
    }
}

/// The collection name and key passed to the component function that failed,
/// or the collection the operation found the problem in. `None` for what was
/// not passed.
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

    #[test]
    fn a_search_limit_of_20_is_passed_on_as_20() {
        assert_eq!(SearchLimit::new(20).to_usize(), 20);
    }

    #[test]
    fn the_largest_search_limit_is_2_to_the_53_minus_1() {
        assert_eq!(SearchLimit::MAX, (1 << 53) - 1);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn the_largest_search_limit_is_passed_on_as_is_on_64_bits() {
        assert_eq!(
            SearchLimit::new(SearchLimit::MAX).to_usize(),
            9_007_199_254_740_991
        );
    }

    #[test]
    #[should_panic(expected = "the search limit 0 is not from 1 to 2^53 - 1")]
    fn a_search_limit_of_0_is_an_unexpected_failure() {
        SearchLimit::new(0);
    }

    #[test]
    #[should_panic(expected = "the search limit 9007199254740992 is not from 1 to 2^53 - 1")]
    fn a_search_limit_larger_than_2_to_the_53_minus_1_is_an_unexpected_failure() {
        SearchLimit::new(SearchLimit::MAX + 1);
    }

    #[test]
    fn the_schema_of_the_search_limit_is_a_whole_number_from_1_to_the_largest() {
        let schema = schemars::schema_for!(SearchLimit);

        assert_eq!(schema.get("type"), Some(&serde_json::json!("integer")));
        assert_eq!(schema.get("minimum"), Some(&serde_json::json!(1)));
        assert_eq!(
            schema.get("maximum"),
            Some(&serde_json::json!(9_007_199_254_740_991_u64))
        );
    }

    // A `u64` always fits in a 64-bit `usize`, so the cap is checked with a
    // narrower type.
    #[test]
    fn a_value_too_large_for_the_type_is_capped_at_its_largest_value() {
        assert_eq!(capped(70_000, u16::MAX), u16::MAX);
    }

    #[test]
    fn a_value_the_type_can_hold_is_kept() {
        assert_eq!(capped(7, u16::MAX), 7);
    }

    #[test]
    fn the_search_limit_is_written_as_given() {
        let json = serde_json::to_value(SearchLimit::new(SearchLimit::MAX)).unwrap();

        assert_eq!(json, serde_json::json!(9_007_199_254_740_991_u64));
    }

    #[test]
    fn the_settings_are_written_as_one_object_in_the_order_of_the_entries() {
        let output = SettingsOutput {
            entries: vec![
                (
                    SettingName::McpShowVersion,
                    SafeJsonValue::new(serde_json::json!(false)),
                ),
                (
                    SettingName::EmbeddingModel,
                    SafeJsonValue::new(serde_json::json!("all-minilm")),
                ),
                (
                    SettingName::GetCliLimitBytes,
                    SafeJsonValue::new(serde_json::json!(4096)),
                ),
            ],
        };

        let json = serde_json::to_string(&output).unwrap();

        assert_eq!(
            json,
            r#"{"mcp.show_version":false,"embedding.model":"all-minilm","get.cli_limit_bytes":4096}"#
        );
    }

    #[test]
    fn the_schema_of_the_settings_is_an_object() {
        let schema = schemars::schema_for!(SettingsOutput);

        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
        assert_eq!(
            schema.get("title"),
            Some(&serde_json::json!("SettingsOutput"))
        );
    }

    fn collection(name: &str) -> CollectionName {
        CollectionName(name.to_owned())
    }

    fn key(key: &str) -> Key {
        Key(key.to_owned())
    }

    fn update_outcome(
        counts: UpdateReport,
        unreadable: &[&str],
        status_unreadable: bool,
    ) -> UpdateOutcome {
        UpdateOutcome {
            counts,
            unreadable_sources: unreadable.iter().map(|name| collection(name)).collect(),
            status_error: status_unreadable.then_some(IndexError::RebuildIncomplete),
        }
    }

    #[test]
    fn a_deleted_collection_has_no_anomaly() {
        let outcome = DeleteCollectionOutcome::Deleted {
            collection: collection("notes"),
        };

        assert_eq!(outcome.anomalies(), []);
    }

    #[test]
    fn a_declined_collection_deletion_is_declined() {
        assert_eq!(
            DeleteCollectionOutcome::Declined.anomalies(),
            [OutcomeAnomaly::Declined]
        );
    }

    #[test]
    fn an_update_that_read_everything_has_no_anomaly() {
        let counts = UpdateReport {
            added: 3,
            changed: 1,
            unchanged: 5,
            reingested: 1,
            deleted: 2,
            ..UpdateReport::default()
        };

        assert_eq!(update_outcome(counts, &[], false).anomalies(), []);
    }

    #[test]
    fn an_update_with_a_failed_document_has_documents_failed() {
        let counts = UpdateReport {
            failed: 1,
            ..UpdateReport::default()
        };

        assert_eq!(
            update_outcome(counts, &[], false).anomalies(),
            [OutcomeAnomaly::DocumentsFailed]
        );
    }

    #[test]
    fn an_update_that_skipped_a_collection_has_sources_unreadable() {
        assert_eq!(
            update_outcome(UpdateReport::default(), &["notes"], false).anomalies(),
            [OutcomeAnomaly::SourcesUnreadable]
        );
    }

    #[test]
    fn an_update_that_could_not_read_a_place_has_sources_unreadable() {
        let counts = UpdateReport {
            unreadable_places: 1,
            ..UpdateReport::default()
        };

        assert_eq!(
            update_outcome(counts, &[], false).anomalies(),
            [OutcomeAnomaly::SourcesUnreadable]
        );
    }

    #[test]
    fn an_update_has_each_anomaly_once_in_the_order_of_the_declaration() {
        let counts = UpdateReport {
            failed: 2,
            unreadable_places: 3,
            ..UpdateReport::default()
        };

        assert_eq!(
            update_outcome(counts, &["notes", "memos"], true).anomalies(),
            [
                OutcomeAnomaly::DocumentsFailed,
                OutcomeAnomaly::SourcesUnreadable,
                OutcomeAnomaly::StatusUnreadable,
            ]
        );
    }

    fn put_outcome(status_unreadable: bool) -> PutOutcome {
        PutOutcome {
            collection: collection("notes"),
            key: key("a.md"),
            outcome: DocumentOutcome::Added,
            status_error: status_unreadable.then_some(IndexError::RebuildIncomplete),
        }
    }

    #[test]
    fn a_put_that_read_the_status_has_no_anomaly() {
        assert_eq!(put_outcome(false).anomalies(), []);
    }

    #[test]
    fn a_put_that_could_not_read_the_status_has_status_unreadable() {
        assert_eq!(
            put_outcome(true).anomalies(),
            [OutcomeAnomaly::StatusUnreadable]
        );
    }

    #[test]
    fn a_deleted_document_has_no_anomaly() {
        let outcome = DeleteDocumentOutcome::Deleted {
            collection: collection("notes"),
            key: key("a.md"),
            supports_update: true,
        };

        assert_eq!(outcome.anomalies(), []);
    }

    #[test]
    fn a_declined_document_deletion_is_declined() {
        assert_eq!(
            DeleteDocumentOutcome::Declined.anomalies(),
            [OutcomeAnomaly::Declined]
        );
    }

    fn setting_change_outcome(
        index_status: Option<Result<IndexStatus, IndexError>>,
    ) -> SettingChangeOutcome {
        SettingChangeOutcome {
            change: SettingChange {
                name: SettingName::EmbeddingModel,
                before: serde_json::json!("all-minilm"),
                after: serde_json::json!("embeddinggemma"),
            },
            index_status,
        }
    }

    #[test]
    fn a_setting_change_without_the_index_status_has_no_anomaly() {
        assert_eq!(setting_change_outcome(None).anomalies(), []);
    }

    #[test]
    fn a_setting_change_with_the_index_status_has_no_anomaly() {
        let status = IndexStatus {
            compared_model_name: "embeddinggemma".to_owned(),
            compared_dimension: crate::index::DimensionResult::NotChecked,
            models: Vec::new(),
            collections: BTreeMap::new(),
            document_count: 0,
            chunk_count: 0,
            chunks_without_vector: 0,
        };

        assert_eq!(setting_change_outcome(Some(Ok(status))).anomalies(), []);
    }

    #[test]
    fn a_setting_change_that_could_not_read_the_index_status_has_status_unreadable() {
        assert_eq!(
            setting_change_outcome(Some(Err(IndexError::RebuildIncomplete))).anomalies(),
            [OutcomeAnomaly::StatusUnreadable]
        );
    }

    #[test]
    fn a_rebuild_that_read_the_status_has_no_anomaly() {
        let outcome = RebuildOutcome {
            rebuilt_chunks: 120,
            status_error: None,
        };

        assert_eq!(outcome.anomalies(), []);
    }

    #[test]
    fn a_rebuild_that_could_not_read_the_status_has_status_unreadable() {
        let outcome = RebuildOutcome {
            rebuilt_chunks: 0,
            status_error: Some(IndexError::RebuildIncomplete),
        };

        assert_eq!(outcome.anomalies(), [OutcomeAnomaly::StatusUnreadable]);
    }

    fn range(start: Option<usize>, end: Option<usize>) -> ChunkRangeRequest {
        ChunkRangeRequest { start, end }
    }

    #[test]
    fn a_range_without_a_start_asks_for_the_body() {
        assert_eq!(OutputPart::of(range(None, None)), OutputPart::Body);
    }

    #[test]
    fn a_range_with_only_an_end_is_taken_as_the_body() {
        assert_eq!(OutputPart::of(range(None, Some(3))), OutputPart::Body);
    }

    #[test]
    fn a_range_with_only_a_start_asks_for_a_chunk() {
        assert_eq!(OutputPart::of(range(Some(2), None)), OutputPart::Chunk);
    }

    #[test]
    fn a_range_with_a_start_and_an_end_asks_for_a_chunk_range() {
        assert_eq!(
            OutputPart::of(range(Some(2), Some(3))),
            OutputPart::ChunkRange
        );
    }

    fn cache_dir_undetermined() -> ModelCacheDirUndetermined {
        ModelCacheDirUndetermined {
            source: MakePathAbsoluteError::HomeDirUnknown,
        }
    }

    #[test]
    fn a_model_cache_directory_deep_in_the_cause_chain_is_found() {
        let error = crate::embedding::EmbeddingError::ModelUnavailable {
            source: Box::new(cache_dir_undetermined()),
        };

        assert!(model_cache_dir_undetermined(&error));
    }

    #[test]
    fn a_path_that_could_not_be_made_absolute_alone_is_not_the_model_cache_directory() {
        let error = crate::embedding::EmbeddingError::ModelUnavailable {
            source: Box::new(MakePathAbsoluteError::HomeDirUnknown),
        };

        assert!(!model_cache_dir_undetermined(&error));
    }

    #[test]
    fn a_cause_chain_without_a_directory_has_no_model_cache_directory() {
        let error = crate::embedding::EmbeddingError::ModelUnavailable {
            source: "Ollama is not running".into(),
        };

        assert!(!model_cache_dir_undetermined(&error));
    }

    #[test]
    fn a_model_cache_directory_that_cannot_be_determined_says_so() {
        assert_eq!(
            cache_dir_undetermined().to_string(),
            "the embedding model cache directory cannot be determined"
        );
    }
}
