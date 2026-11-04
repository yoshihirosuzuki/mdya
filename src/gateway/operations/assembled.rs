//! The operations over the assembled components: each calls the components'
//! functions in the order its operation needs.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;

use crate::collection::{CollectionChange, CollectionInfo};
use crate::config::SettingName;
use crate::embedding::{EmbeddingError, EmbeddingSettings};
use crate::index::{
    DimensionCheck, DimensionResult, IndexError, IndexStatus, ModelStatus, StatusScope,
};
use crate::ingest::{Ingest, IngestError, PutInstruction, UpdateInstruction, UpdateReport};
use crate::query::{Hit, SearchMode, SearchRequest, SearchUnit};
use crate::types::{
    BoxError, CollectionName, CollectionType, Key, Progress, ProgressSink, TypeAttributes,
};

use super::super::write_lock::WriteLockGuard;
use super::{
    ChangeCollectionInput, ChangeSettingInput, CollectionEntry, CollectionList,
    CreateCollectionInput, DeleteCollectionInput, DeleteCollectionOutcome, DeleteDocumentInput,
    DeleteDocumentOutcome, DocumentText, ErrorContext, GatewayProgressSink, GetDocumentInput,
    IndexEmbeddingModel, MODEL_CACHE_DIR_FIX, OperationError, OperationParts, Operations,
    OptimizeInput, OutputPart, PutInput, PutOutcome, RebuildInput, RebuildOutcome, SearchHit,
    SearchInput, SearchModeName, SearchOutput, SearchUnitName, SettingChangeOutcome,
    SettingsOutput, StatusOutput, UpdateAllInput, UpdateInput, UpdateOutcome,
    model_cache_dir_undetermined,
};

#[cfg(test)]
mod tests;

pub(super) struct AssembledOperations {
    parts: OperationParts,
}

impl AssembledOperations {
    pub(super) fn new(parts: OperationParts) -> Self {
        Self { parts }
    }

    /// Takes the write lock, without waiting.
    async fn lock(&self) -> Result<WriteLockGuard, OperationError> {
        self.parts
            .write_lock
            .try_acquire()
            .await
            .map_err(OperationError::WriteLock)
    }

    /// The ingest implementation for a stored collection's type. `None` for a
    /// type with none, written by another version of mdya or by hand; the
    /// caller turns that into [`unknown_type`].
    fn ingest(&self, collection: &CollectionInfo) -> Option<&dyn Ingest> {
        self.parts
            .ingests
            .get(&collection.collection_type)
            .map(AsRef::as_ref)
    }

    /// The ingest implementation for the type of a collection to create.
    #[expect(clippy::expect_used)]
    fn ingest_for_new(&self, collection_type: &CollectionType) -> &dyn Ingest {
        self.parts
            .ingests
            .get(collection_type)
            .expect(
                "the command line takes only the collection types the assembly gives an ingest \
                 implementation for",
            )
            .as_ref()
    }

    /// The attributes given by the user, as `ingest` accepts them, with the
    /// places made absolute.
    async fn check_attributes(
        &self,
        ingest: &dyn Ingest,
        attributes: TypeAttributes,
    ) -> Result<TypeAttributes, OperationError> {
        ingest
            .check_attributes(attributes, &self.parts.dirs)
            .await
            .map_err(|source| OperationError::Ingest {
                context: ErrorContext::default(),
                source,
            })
    }

    /// The collection named `name`, or `CollectionNotFound`.
    async fn existing_collection(
        &self,
        name: &CollectionName,
    ) -> Result<CollectionInfo, OperationError> {
        self.parts
            .collections
            .get_collection(name)
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::collection(name),
                source,
            })?
            .ok_or_else(|| OperationError::CollectionNotFound {
                collection: name.clone(),
            })
    }

    /// Maintains the index at the end of a write operation, unless
    /// `optimize` is false. Tells `progress`, if any, first. A failure does
    /// not change the operation's result; it is logged as a warning.
    async fn maintain(&self, optimize: bool, progress: Option<&dyn GatewayProgressSink>) {
        if !optimize {
            return;
        }
        if let Some(progress) = progress {
            progress.maintenance_started();
        }
        if let Err(error) = self.parts.index.maintain().await {
            tracing::warn!(cause = error_chain(&error).as_str(), "{MAINTENANCE_FAILED}");
        }
    }

    async fn delete_collection_unmaintained(
        &self,
        input: &DeleteCollectionInput,
    ) -> Result<DeleteCollectionOutcome, OperationError> {
        self.existing_collection(&input.name).await?;
        if !input.confirmation.confirm().await {
            return Ok(DeleteCollectionOutcome::Declined);
        }
        self.parts
            .collections
            .delete_collection(&input.name)
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::collection(&input.name),
                source,
            })?;
        Ok(DeleteCollectionOutcome::Deleted {
            collection: input.name.clone(),
        })
    }

    async fn delete_document_unmaintained(
        &self,
        input: &DeleteDocumentInput,
    ) -> Result<DeleteDocumentOutcome, OperationError> {
        let collection = self.existing_collection(&input.collection).await?;
        // Before anything is deleted, so that an unknown type deletes nothing.
        // The type is needed only to say whether the document may come back;
        // the collection itself can still be deleted, which needs no type.
        let supports_update = self
            .ingest(&collection)
            .ok_or_else(|| unknown_type(&collection))?
            .supports_update();
        let context = ErrorContext::document(&input.collection, &input.key);
        let index_error = |source| OperationError::Index {
            context: context.clone(),
            source,
        };
        let exists = self
            .parts
            .index
            .has_document(&input.collection, &input.key)
            .await
            .map_err(index_error)?;
        if !exists {
            return Err(OperationError::DocumentNotFound {
                collection: input.collection.clone(),
                key: input.key.clone(),
            });
        }
        if !input.confirmation.confirm().await {
            return Ok(DeleteDocumentOutcome::Declined);
        }
        self.parts
            .index
            .delete_document(&input.collection, &input.key)
            .await
            .map_err(index_error)?;
        Ok(DeleteDocumentOutcome::Deleted {
            collection: input.collection.clone(),
            key: input.key.clone(),
            supports_update,
        })
    }

    /// Whether the index can take documents now (its embedding model, and
    /// no rebuild left unfinished).
    async fn check_ingestable(&self) -> Result<(), OperationError> {
        self.parts
            .index
            .check_ingestable()
            .await
            .map_err(|source| OperationError::Index {
                context: ErrorContext::default(),
                source,
            })
    }

    async fn put_unmaintained(&self, input: PutInput) -> Result<PutOutcome, OperationError> {
        let collection = self.existing_collection(&input.collection).await?;
        self.check_ingestable().await?;
        let ingest = self
            .ingest(&collection)
            .ok_or_else(|| unknown_type(&collection))?;
        if !ingest.supports_put() {
            return Err(OperationError::PutNotSupported {
                collection: input.collection,
            });
        }
        let content = input
            .content
            .read()
            .await
            .map_err(|source| OperationError::ContentUnreadable { source })?;
        let context = ErrorContext::document(&input.collection, &input.key);
        let outcome = ingest
            .put(PutInstruction {
                collection: input.collection.clone(),
                attributes: collection.attributes,
                key: input.key.clone(),
                media_type: input.media_type,
                content,
                overwrite: input.overwrite,
            })
            .await
            .map_err(|source| OperationError::Ingest { context, source })?;
        let status_error = self
            .warn_about_status(
                StatusScope::Collections(vec![input.collection.clone()]),
                DimensionCheck::OnlyIfSameName,
            )
            .await;
        Ok(PutOutcome {
            collection: input.collection,
            key: input.key,
            outcome,
            status_error,
        })
    }

    /// Updates `collections` one after another and reads the status of those
    /// it finished. A collection whose source cannot be read is skipped. Any
    /// other error stops the update, after the status of the collections
    /// finished so far is read for its warnings.
    async fn update_each(
        &self,
        collections: Vec<(CollectionInfo, &dyn Ingest)>,
        overwrite: bool,
        progress: &dyn GatewayProgressSink,
    ) -> Result<UpdateOutcome, OperationError> {
        let mut counts = UpdateReport::default();
        let mut finished = Vec::new();
        let mut unreadable_sources = Vec::new();
        let mut before = DocumentCounts::default();
        for (collection, ingest) in collections {
            let summed = SummedProgress::new(progress, before);
            let result = ingest
                .update(
                    UpdateInstruction {
                        collection: collection.name.clone(),
                        attributes: collection.attributes,
                        overwrite,
                        parallelism: self.parts.parallelism,
                    },
                    &summed,
                )
                .await;
            before = summed.after();
            match result {
                Ok(report) => {
                    add_counts(&mut counts, report);
                    finished.push(collection.name);
                }
                Err(stopped) if matches!(stopped.error, IngestError::SourceUnreadable { .. }) => {
                    add_counts(&mut counts, stopped.counts);
                    unreadable_sources.push(collection.name);
                }
                Err(stopped) => {
                    add_counts(&mut counts, stopped.counts);
                    // The update ends with an error, which has no place for
                    // a status that cannot be read; it is logged.
                    self.warn_about_finished(finished).await;
                    return Err(OperationError::UpdateStopped {
                        counts,
                        context: ErrorContext::collection(&collection.name),
                        source: stopped.error,
                    });
                }
            }
        }
        let status_error = self.warn_about_finished(finished).await;
        Ok(UpdateOutcome {
            counts,
            unreadable_sources,
            status_error,
        })
    }

    /// Reads the status of the collections an update finished, for the
    /// warnings it calls for. Reads nothing when it finished none.
    async fn warn_about_finished(&self, finished: Vec<CollectionName>) -> Option<IndexError> {
        if finished.is_empty() {
            return None;
        }
        self.warn_about_status(
            StatusScope::Collections(finished),
            DimensionCheck::OnlyIfSameName,
        )
        .await
    }

    async fn update_collection_unmaintained(
        &self,
        input: &UpdateInput,
    ) -> Result<UpdateOutcome, OperationError> {
        let collection = self.existing_collection(&input.collection).await?;
        self.check_ingestable().await?;
        let ingest = self
            .ingest(&collection)
            .ok_or_else(|| unknown_type(&collection))?;
        if !ingest.supports_update() {
            return Err(OperationError::UpdateNotSupported {
                collection: collection.name,
            });
        }
        self.update_each(
            vec![(collection, ingest)],
            input.overwrite,
            input.progress.as_ref(),
        )
        .await
    }

    async fn update_all_unmaintained(
        &self,
        input: &UpdateAllInput,
    ) -> Result<UpdateOutcome, OperationError> {
        let collections = self
            .parts
            .collections
            .list_collections()
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::default(),
                source,
            })?;
        self.check_ingestable().await?;
        let mut updatable = Vec::with_capacity(collections.len());
        for collection in collections {
            let ingest = self
                .ingest(&collection)
                .ok_or_else(|| unknown_type(&collection))?;
            if ingest.supports_update() {
                updatable.push((collection, ingest));
            }
        }
        self.update_each(updatable, input.overwrite, input.progress.as_ref())
            .await
    }

    async fn rebuild_unmaintained(
        &self,
        input: &RebuildInput,
    ) -> Result<RebuildOutcome, OperationError> {
        let rebuilt_chunks = self
            .parts
            .index
            .rebuild_embeddings(
                input.include_configured_model,
                self.parts.parallelism,
                input.progress.as_ref(),
            )
            .await
            .map_err(|source| OperationError::Index {
                context: ErrorContext::default(),
                source,
            })?;
        let status_error = self
            .warn_about_status(StatusScope::All, DimensionCheck::Never)
            .await;
        Ok(RebuildOutcome {
            rebuilt_chunks,
            status_error,
        })
    }

    /// Reads the index status after a write operation and logs the warnings
    /// it calls for: the collections with chunks whose embedding vector does
    /// not cover the whole chunk, and a dimension that could not be checked.
    /// Returns why the status could not be read, after logging it.
    async fn warn_about_status(
        &self,
        scope: StatusScope,
        check: DimensionCheck,
    ) -> Option<IndexError> {
        let status = match self
            .parts
            .index
            .status(scope, self.parts.embedding.as_ref(), check)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(cause = error_chain(&error).as_str(), "{STATUS_UNREADABLE}");
                return Some(error);
            }
        };
        for (collection, models) in &status.collections {
            let truncated: u64 = models.iter().map(|model| model.truncated_chunk_count).sum();
            if truncated > 0 {
                tracing::warn!(
                    collection = %collection.0,
                    chunks = truncated,
                    "{TRUNCATED_CHUNKS}"
                );
            }
        }
        if let DimensionResult::Unavailable(error) = &status.compared_dimension {
            // A `fix` of `None` adds no field to the warning.
            let fix = model_cache_dir_undetermined(error).then(|| MODEL_CACHE_DIR_FIX.join(" "));
            tracing::warn!(
                cause = error_chain(error).as_str(),
                fix = fix.as_deref(),
                "{DIMENSION_UNCHECKED}"
            );
        }
        None
    }
}

/// The error for a stored collection whose type has no ingest
/// implementation.
fn unknown_type(collection: &CollectionInfo) -> OperationError {
    OperationError::UnknownCollectionType {
        collection: collection.name.clone(),
        collection_type: collection.collection_type.clone(),
    }
}

/// Adds `counts` to `total`, outcome by outcome. Every field is named, so
/// that a field added to `UpdateReport` cannot be left out.
fn add_counts(total: &mut UpdateReport, counts: UpdateReport) {
    let UpdateReport {
        added,
        changed,
        unchanged,
        reingested,
        deleted,
        failed,
        unreadable_places,
    } = counts;
    total.added += added;
    total.changed += changed;
    total.unchanged += unchanged;
    total.reingested += reingested;
    total.deleted += deleted;
    total.failed += failed;
    total.unreadable_places += unreadable_places;
}

/// Passes on the progress of the collection being updated as the progress
/// of the whole update: the documents of the collections updated before it
/// are added, so that moving on to the next collection does not take the
/// progress back. The progress within the collection is passed on as it
/// comes. The embedded chunks are passed on as they are.
struct SummedProgress<'a> {
    sink: &'a dyn GatewayProgressSink,
    /// The documents of the collections before.
    before: DocumentCounts,
    /// The furthest progress of this collection reported so far.
    furthest: Mutex<Progress>,
}

/// How many documents are done, and how many there are in all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DocumentCounts {
    done: u64,
    total: u64,
}

impl<'a> SummedProgress<'a> {
    fn new(sink: &'a dyn GatewayProgressSink, before: DocumentCounts) -> Self {
        Self {
            sink,
            before,
            furthest: Mutex::new(Progress {
                done: 0,
                total: None,
            }),
        }
    }

    /// The documents up to the end of this collection: what the next
    /// collection adds to its own. A collection whose total was never
    /// reported counts the documents it did.
    fn after(&self) -> DocumentCounts {
        let furthest = *self.furthest.lock().unwrap_or_else(PoisonError::into_inner);
        DocumentCounts {
            done: self.before.done + furthest.done,
            total: self.before.total + furthest.total.unwrap_or(furthest.done),
        }
    }
}

impl ProgressSink for SummedProgress<'_> {
    fn report(&self, progress: Progress) {
        {
            let mut furthest = self.furthest.lock().unwrap_or_else(PoisonError::into_inner);
            furthest.done = furthest.done.max(progress.done);
            furthest.total = furthest.total.max(progress.total);
        }
        self.sink.report(Progress {
            done: self.before.done + progress.done,
            total: progress.total.map(|total| self.before.total + total),
        });
    }

    fn add_embedded_chunks(&self, count: u64) {
        self.sink.add_embedded_chunks(count);
    }
}

/// The warning logged when the index could not be maintained at the end of
/// a write operation. The reason is in its `cause` field.
const MAINTENANCE_FAILED: &str = "the index could not be maintained";

/// The warning logged when the index status could not be read after a write
/// operation. The reason is in its `cause` field.
const STATUS_UNREADABLE: &str = "the index status could not be read";

/// The warning logged for each collection with chunks whose embedding vector
/// does not cover the whole chunk, after a write operation, with the
/// collection and the number of those chunks in its fields. Only the
/// command line has write operations, so the fix names its commands.
const TRUNCATED_CHUNKS: &str = "some chunks are longer than the embedding model's context length, \
     so their embeddings cover only part of them; set an embedding model with a longer context \
     length (mdya config set embedding.model) and run mdya embedding rebuild";

/// Said when the configured embedding model's dimension could not be
/// checked: the warning logged after a write operation, with the reason in
/// its `cause` field, and the start of the status warning, followed by the
/// reason. When the embedding model cache directory could not be
/// determined, the warning has the fix in its `fix` field, and the status
/// warning ends with it.
const DIMENSION_UNCHECKED: &str =
    "the dimension of the configured embedding model could not be checked";

/// The status warning for a dimension that could not be checked because of
/// `error`.
fn dimension_unchecked_warning(error: &EmbeddingError) -> String {
    let warning = format!("{DIMENSION_UNCHECKED}: {}", error_chain(error));
    if !model_cache_dir_undetermined(error) {
        return warning;
    }
    let end = if warning.ends_with('.') { "" } else { "." };
    format!("{warning}{end} {}", MODEL_CACHE_DIR_FIX.join(" "))
}

/// `error` and its causes, joined by `: `.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

impl From<ModelStatus> for IndexEmbeddingModel {
    fn from(status: ModelStatus) -> Self {
        Self {
            model: status.model.name,
            dimension: status.model.dimension as u64,
            configured: status.same_as_compared,
            chunk_count: status.chunk_count,
            truncated_chunk_count: status.truncated_chunk_count,
        }
    }
}

/// The status output of `status`, with `collection_count` collections.
fn status_output(status: IndexStatus, collection_count: u64) -> StatusOutput {
    let (configured_embedding_dimension, warnings) = match status.compared_dimension {
        DimensionResult::Known(dimension) => (Some(dimension as u64), Vec::new()),
        DimensionResult::Unavailable(error) => (None, vec![dimension_unchecked_warning(&error)]),
        DimensionResult::NotChecked => (None, Vec::new()),
    };
    StatusOutput {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        configured_embedding_model: status.compared_model_name,
        configured_embedding_dimension,
        index_embedding_models: status.models.into_iter().map(Into::into).collect(),
        chunks_without_embedding_count: status.chunks_without_vector,
        collection_count,
        document_count: status.document_count,
        chunk_count: status.chunk_count,
        warnings,
    }
}

impl ErrorContext {
    fn collection(name: &CollectionName) -> Self {
        Self {
            collection: Some(name.clone()),
            key: None,
        }
    }

    fn document(collection: &CollectionName, key: &Key) -> Self {
        Self {
            collection: Some(collection.clone()),
            key: Some(key.clone()),
        }
    }
}

impl From<Hit> for SearchHit {
    fn from(hit: Hit) -> Self {
        Self {
            collection: hit.collection.0,
            key: hit.key.0,
            score: hit.score,
            snippet: hit.snippet,
            chunk_index: hit.chunk_index as u64,
            chunk_count: hit.chunk_count as u64,
        }
    }
}

impl From<SearchMode> for SearchModeName {
    fn from(mode: SearchMode) -> Self {
        match mode {
            SearchMode::FullText => Self::Fts,
            SearchMode::Vector => Self::Vector,
            SearchMode::Hybrid => Self::Hybrid,
        }
    }
}

impl From<SearchUnit> for SearchUnitName {
    fn from(unit: SearchUnit) -> Self {
        match unit {
            SearchUnit::Document => Self::Document,
            SearchUnit::Chunk => Self::Chunk,
        }
    }
}

#[async_trait]
impl Operations for AssembledOperations {
    async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CollectionInfo, OperationError> {
        let _lock = self.lock().await?;
        let attributes = self
            .check_attributes(
                self.ingest_for_new(&input.collection_type),
                input.attributes,
            )
            .await?;
        let collection = CollectionInfo {
            name: input.name,
            collection_type: input.collection_type,
            attributes,
            description: input.description,
        };
        self.parts
            .collections
            .create_collection(collection.clone())
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::collection(&collection.name),
                source,
            })?;
        Ok(collection)
    }

    async fn delete_collection(
        &self,
        input: DeleteCollectionInput,
    ) -> Result<DeleteCollectionOutcome, OperationError> {
        let _lock = self.lock().await?;
        let outcome = self.delete_collection_unmaintained(&input).await;
        let declined = matches!(outcome, Ok(DeleteCollectionOutcome::Declined));
        self.maintain(input.optimize && !declined, None).await;
        outcome
    }

    async fn list_collections(&self) -> Result<CollectionList, OperationError> {
        let collections = self
            .parts
            .collections
            .list_collections()
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::default(),
                source,
            })?;
        let counts =
            self.parts
                .index
                .document_counts()
                .await
                .map_err(|source| OperationError::Index {
                    context: ErrorContext::default(),
                    source,
                })?;
        let collections = collections
            .into_iter()
            .map(|collection| CollectionEntry {
                // A collection without documents is not in the counts.
                document_count: counts.get(&collection.name).copied().unwrap_or(0),
                name: collection.name.0,
                collection_type: collection.collection_type.0,
                description: collection.description,
                attributes: collection.attributes.0,
            })
            .collect();
        Ok(CollectionList { collections })
    }

    async fn change_collection(
        &self,
        input: ChangeCollectionInput,
    ) -> Result<CollectionInfo, OperationError> {
        let _lock = self.lock().await?;
        let collection = self.existing_collection(&input.name).await?;
        // Only attributes need the type's ingest implementation, so the
        // description can be changed even when the type is unknown.
        let attributes = match input.change.attributes {
            Some(attributes) => Some(
                self.check_attributes(
                    self.ingest(&collection)
                        .ok_or_else(|| unknown_type(&collection))?,
                    attributes,
                )
                .await?,
            ),
            None => None,
        };
        let change = CollectionChange {
            description: input.change.description,
            attributes,
        };
        self.parts
            .collections
            .change_collection(&input.name, change)
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::collection(&input.name),
                source,
            })
    }

    async fn update_collection(&self, input: UpdateInput) -> Result<UpdateOutcome, OperationError> {
        let _lock = self.lock().await?;
        let outcome = self.update_collection_unmaintained(&input).await;
        self.maintain(input.optimize, Some(input.progress.as_ref()))
            .await;
        outcome
    }

    async fn update_all(&self, input: UpdateAllInput) -> Result<UpdateOutcome, OperationError> {
        let _lock = self.lock().await?;
        let outcome = self.update_all_unmaintained(&input).await;
        self.maintain(input.optimize, Some(input.progress.as_ref()))
            .await;
        outcome
    }

    async fn put_document(&self, input: PutInput) -> Result<PutOutcome, OperationError> {
        let _lock = self.lock().await?;
        let optimize = input.optimize;
        let outcome = self.put_unmaintained(input).await;
        self.maintain(optimize, None).await;
        outcome
    }

    async fn delete_document(
        &self,
        input: DeleteDocumentInput,
    ) -> Result<DeleteDocumentOutcome, OperationError> {
        let _lock = self.lock().await?;
        let outcome = self.delete_document_unmaintained(&input).await;
        let declined = matches!(outcome, Ok(DeleteDocumentOutcome::Declined));
        self.maintain(input.optimize && !declined, None).await;
        outcome
    }

    async fn search(&self, input: SearchInput) -> Result<SearchOutput, OperationError> {
        for collection in &input.collections {
            self.existing_collection(collection).await?;
        }
        let request = SearchRequest {
            query: input.query.clone(),
            mode: input.mode,
            collections: input.collections.clone(),
            limit: input.limit.to_usize(),
            unit: input.unit,
        };
        let result =
            self.parts
                .query
                .search(request)
                .await
                .map_err(|source| OperationError::Query {
                    // No key: `error_report` tells a search from getting a
                    // document by it.
                    context: ErrorContext::default(),
                    source,
                })?;
        let hits: Vec<SearchHit> = result.hits.into_iter().map(SearchHit::from).collect();
        Ok(SearchOutput {
            query: input.query,
            mode: input.mode.into(),
            unit: input.unit.into(),
            collections: input
                .collections
                .into_iter()
                .map(|collection| collection.0)
                .collect(),
            limit: input.limit,
            hit_count: hits.len() as u64,
            hits,
            warnings: result.warnings,
        })
    }

    async fn get_document(&self, input: GetDocumentInput) -> Result<DocumentText, OperationError> {
        self.existing_collection(&input.collection).await?;
        let text = self
            .parts
            .query
            .get_document(&input.collection, &input.key, input.range)
            .await
            .map_err(|source| OperationError::Query {
                // The key tells `error_report` this is not a search.
                context: ErrorContext::document(&input.collection, &input.key),
                source,
            })?;
        let size_bytes = text.len() as u64;
        if let Some(limit_bytes) = input.size_limit
            && size_bytes > limit_bytes
        {
            return Err(OperationError::OutputTooLarge {
                part: OutputPart::of(input.range),
                size_bytes,
                limit_bytes,
            });
        }
        Ok(DocumentText {
            collection: input.collection.0,
            key: input.key.0,
            text,
        })
    }

    async fn status(&self) -> Result<StatusOutput, OperationError> {
        let status = self
            .parts
            .index
            .status(
                StatusScope::All,
                self.parts.embedding.as_ref(),
                DimensionCheck::Always,
            )
            .await
            .map_err(|source| OperationError::Index {
                context: ErrorContext::default(),
                source,
            })?;
        let collections = self
            .parts
            .collections
            .list_collections()
            .await
            .map_err(|source| OperationError::Collection {
                context: ErrorContext::default(),
                source,
            })?;
        Ok(status_output(status, collections.len() as u64))
    }

    async fn list_settings(&self) -> Result<SettingsOutput, OperationError> {
        let settings = self
            .parts
            .config
            .current()
            .await
            .map_err(OperationError::Config)?;
        Ok(SettingsOutput {
            entries: settings.entries(),
        })
    }

    async fn change_setting(
        &self,
        input: ChangeSettingInput,
    ) -> Result<SettingChangeOutcome, OperationError> {
        let _lock = self.lock().await?;
        let config = &self.parts.config;
        let change = match &input {
            ChangeSettingInput::Set { name, value } => config.set(name, value).await,
            ChangeSettingInput::Reset { name } => config.reset(name).await,
        }
        .map_err(OperationError::Config)?;
        if change.name != SettingName::EmbeddingModel {
            return Ok(SettingChangeOutcome {
                change,
                index_status: None,
            });
        }
        // The setting stays changed when the settings cannot be read now.
        let settings = config.current().await.map_err(OperationError::Config)?;
        let model_cache_dir = Arc::clone(&self.parts.model_cache_dir);
        let embedding = (self.parts.create_embedding)(EmbeddingSettings {
            model: settings.embedding_model,
            model_cache_dir: Arc::new(move || {
                model_cache_dir().map_err(|error| -> BoxError { Box::new(error) })
            }),
            hf_token: self.parts.hf_token.clone(),
            ollama_endpoint: settings.embedding_ollama_endpoint,
        });
        let index_status = self
            .parts
            .index
            .status(
                StatusScope::All,
                embedding.as_ref(),
                DimensionCheck::OnlyIfSameName,
            )
            .await;
        Ok(SettingChangeOutcome {
            change,
            index_status: Some(index_status),
        })
    }

    async fn rebuild_embeddings(
        &self,
        input: RebuildInput,
    ) -> Result<RebuildOutcome, OperationError> {
        let _lock = self.lock().await?;
        let outcome = self.rebuild_unmaintained(&input).await;
        self.maintain(input.optimize, Some(input.progress.as_ref()))
            .await;
        outcome
    }

    async fn optimize(&self, input: OptimizeInput) -> Result<(), OperationError> {
        let _lock = self.lock().await?;
        input.progress.maintenance_started();
        self.parts
            .index
            .maintain()
            .await
            .map_err(|source| OperationError::Index {
                context: ErrorContext::default(),
                source,
            })
    }
}
