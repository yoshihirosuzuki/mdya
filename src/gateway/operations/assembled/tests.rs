#![expect(
    clippy::result_large_err,
    reason = "the ingest handlers return `update`'s error, as the trait does"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::collection::{CollectionChange, CollectionError, CollectionInfo};
use crate::config::{ConfigError, SettingChange, SettingName, Settings};
use crate::embedding::test_double::TestEmbedding;
use crate::embedding::{Embedding, EmbeddingError};
use crate::gateway::test_double::collections::{CollectionsCall, TestCollections};
use crate::gateway::test_double::config::{ConfigCall, TestConfig};
use crate::gateway::test_double::ingest::{IngestCall, TestIngest, report_documents};
use crate::gateway::test_double::query::{QueryCall, TestQuery};
use crate::gateway::write_lock::WriteLockError;
use crate::gateway::write_lock::test_double::{LockState, TestWriteLock};
use crate::index::test_double::{IndexCall, TestIndex};
use crate::index::{
    DimensionCheck, DimensionResult, IndexError, IndexStatus, ModelStatus, StatusScope,
};
use crate::ingest::{
    Ingest, IngestError, PutInstruction, UpdateInstruction, UpdateReport, UpdateStopped,
};
use crate::path::MakePathAbsoluteError;
use crate::query::{
    ChunkRangeRequest, Hit, QueryError, SearchMode, SearchRequest, SearchResult, SearchUnit,
};
use crate::test_support::log_receiver::{LogRecord, capture_async};
use crate::types::test_double::test_dir_lookup;
use crate::types::{
    BoxError, CollectionName, CollectionType, DocumentOutcome, EmbeddingModel, Key, Progress,
    ProgressSink, TypeAttributes,
};

use super::super::test_double::{ProgressEvent, TestConfirmation, TestContentReader, TestProgress};
use super::super::{
    ChangeCollectionInput, ChangeSettingInput, CollectionEntry, CollectionList,
    CreateCollectionInput, DeleteCollectionInput, DeleteCollectionOutcome, DeleteDocumentInput,
    DeleteDocumentOutcome, DocumentText, ErrorContext, GetDocumentInput, IndexEmbeddingModel,
    ModelCacheDirUndetermined, OperationError, OperationParts, Operations, OptimizeInput,
    OutcomeAnomaly, OutputPart, PutInput, RebuildInput, SafeInteger, SafeJsonObject, SafeJsonValue,
    Score, SearchHit, SearchInput, SearchLimit, SearchModeName, SearchOutput, SearchUnitName,
    SettingsOutput, StatusOutput, UpdateAllInput, UpdateInput, model_cache_dir_undetermined,
};
use super::{
    AssembledOperations, DIMENSION_UNCHECKED, DocumentCounts, MAINTENANCE_FAILED,
    STATUS_UNREADABLE, SummedProgress, TRUNCATED_CHUNKS,
};

/// The doubles of the components, kept so that a test can check what each
/// received after handing them to the operations.
struct Doubles {
    config: Arc<TestConfig>,
    embedding: Arc<TestEmbedding>,
    index: Arc<TestIndex>,
    collections: Arc<TestCollections>,
    query: Arc<TestQuery>,
    ingests: BTreeMap<CollectionType, Arc<TestIngest>>,
    write_lock: Arc<TestWriteLock>,
    /// The function that returns the embedding model cache directory.
    model_cache_dir: Arc<dyn Fn() -> Result<PathBuf, ModelCacheDirUndetermined> + Send + Sync>,
    /// What `create_embedding` was given, each time.
    created_embeddings: Arc<Mutex<Vec<CreatedEmbedding>>>,
}

/// The settings `create_embedding` was given. `EmbeddingSettings` holds a
/// function and a token, so the double keeps what the function returned.
#[derive(Debug, PartialEq)]
struct CreatedEmbedding {
    model: String,
    ollama_endpoint: String,
    hf_token: Option<String>,
    model_cache_dir: ReturnedCacheDir,
}

/// What the function for the embedding model cache directory in
/// `EmbeddingSettings` returned.
#[derive(Debug, PartialEq)]
enum ReturnedCacheDir {
    Dir(PathBuf),
    /// An error in which `model_cache_dir_undetermined` finds that the
    /// directory could not be determined.
    Undetermined,
    /// Any other error.
    OtherError,
}

impl ReturnedCacheDir {
    fn of(returned: Result<PathBuf, BoxError>) -> Self {
        match returned {
            Ok(dir) => Self::Dir(dir),
            Err(error) if model_cache_dir_undetermined(&*error) => Self::Undetermined,
            Err(_) => Self::OtherError,
        }
    }
}

impl Doubles {
    fn new() -> Self {
        Self {
            config: Arc::new(TestConfig::new()),
            embedding: Arc::new(TestEmbedding::new("all-minilm", 384)),
            index: Arc::new(TestIndex::new()),
            collections: Arc::new(TestCollections::new()),
            query: Arc::new(TestQuery::new()),
            ingests: BTreeMap::from([
                (directory(), Arc::new(TestIngest::new())),
                (raw(), Arc::new(TestIngest::new().supporting(false, true))),
            ]),
            write_lock: Arc::new(TestWriteLock::new()),
            model_cache_dir: Arc::new(|| Ok("/home/user/.mdya-models".into())),
            created_embeddings: Arc::default(),
        }
    }

    fn config(mut self, config: TestConfig) -> Self {
        self.config = Arc::new(config);
        self
    }

    fn index(mut self, index: TestIndex) -> Self {
        self.index = Arc::new(index);
        self
    }

    fn collections(mut self, collections: TestCollections) -> Self {
        self.collections = Arc::new(collections);
        self
    }

    fn ingest(mut self, collection_type: CollectionType, ingest: TestIngest) -> Self {
        self.ingests.insert(collection_type, Arc::new(ingest));
        self
    }

    fn query(mut self, query: TestQuery) -> Self {
        self.query = Arc::new(query);
        self
    }

    fn write_lock(mut self, write_lock: Arc<TestWriteLock>) -> Self {
        self.write_lock = write_lock;
        self
    }

    fn model_cache_dir(
        mut self,
        model_cache_dir: impl Fn() -> Result<PathBuf, ModelCacheDirUndetermined> + Send + Sync + 'static,
    ) -> Self {
        self.model_cache_dir = Arc::new(model_cache_dir);
        self
    }

    fn operations(&self) -> AssembledOperations {
        let ingests = self
            .ingests
            .iter()
            .map(|(collection_type, ingest)| {
                (
                    collection_type.clone(),
                    Arc::clone(ingest) as Arc<dyn Ingest>,
                )
            })
            .collect();
        AssembledOperations::new(OperationParts {
            config: self.config.clone(),
            embedding: self.embedding.clone(),
            index: self.index.clone(),
            collections: self.collections.clone(),
            query: self.query.clone(),
            ingests,
            create_embedding: {
                let created = Arc::clone(&self.created_embeddings);
                Arc::new(move |settings| {
                    created.lock().unwrap().push(CreatedEmbedding {
                        model: settings.model.clone(),
                        ollama_endpoint: settings.ollama_endpoint.clone(),
                        hf_token: settings.hf_token.clone(),
                        model_cache_dir: ReturnedCacheDir::of((settings.model_cache_dir)()),
                    });
                    Arc::new(TestEmbedding::new(&settings.model, 768)) as Arc<dyn Embedding>
                })
            },
            model_cache_dir: self.model_cache_dir.clone(),
            hf_token: Some("hf-token".to_owned()),
            dirs: test_dir_lookup(Some("/home/user"), Ok("/home/user/work")),
            parallelism: 4,
            write_lock: self.write_lock.clone(),
        })
    }
}

fn directory() -> CollectionType {
    CollectionType("directory".to_owned())
}

fn raw() -> CollectionType {
    CollectionType("raw".to_owned())
}

fn name(name: &str) -> CollectionName {
    CollectionName(name.to_owned())
}

fn notes() -> CollectionInfo {
    CollectionInfo {
        name: name("notes"),
        collection_type: directory(),
        attributes: TypeAttributes::default(),
        description: None,
    }
}

/// An index whose `maintain` records whether the lock was held then.
fn index_seeing_lock(lock: &Arc<TestWriteLock>, seen: &Arc<Mutex<Vec<LockState>>>) -> TestIndex {
    let lock = Arc::clone(lock);
    let seen = Arc::clone(seen);
    TestIndex::new().on_maintain(move || {
        seen.lock().unwrap().push(lock.state());
        Ok(())
    })
}

fn delete_notes(confirmation: &TestConfirmation, optimize: bool) -> DeleteCollectionInput {
    DeleteCollectionInput {
        name: name("notes"),
        confirmation: Box::new(confirmation.clone()),
        optimize,
    }
}

#[tokio::test]
async fn deleting_a_collection_asks_then_deletes_and_maintains_holding_the_lock() {
    let lock = Arc::new(TestWriteLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_seeing_lock(&lock, &seen))
        .write_lock(Arc::clone(&lock));
    let confirmation = TestConfirmation::answering(true);

    let outcome = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        DeleteCollectionOutcome::Deleted { collection } if collection == name("notes")
    ));
    assert_eq!(confirmation.asked(), 1);
    assert_eq!(
        doubles.collections.calls(),
        [
            CollectionsCall::Get(name("notes")),
            CollectionsCall::Delete(name("notes")),
        ]
    );
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
    assert_eq!(
        *seen.lock().unwrap(),
        [LockState {
            acquired: 1,
            held: true
        }]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn deleting_a_missing_collection_does_not_ask_and_still_maintains() {
    let doubles = Doubles::new();
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("notes")
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

#[tokio::test]
async fn a_declined_collection_deletion_deletes_nothing_and_does_not_maintain() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));
    let confirmation = TestConfirmation::answering(false);

    let outcome = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap();

    assert!(matches!(outcome, DeleteCollectionOutcome::Declined));
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn a_failed_collection_deletion_returns_the_error_with_the_name_and_still_maintains() {
    let doubles = Doubles::new().collections(
        TestCollections::with(vec![notes()]).on_delete(|_| Err(CollectionError::NotFound)),
    );
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext { collection: Some(collection), key: None },
            source: CollectionError::NotFound,
        } if collection == name("notes")
    ));
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

#[tokio::test]
async fn deleting_a_collection_without_optimizing_does_not_maintain() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));
    let confirmation = TestConfirmation::answering(true);

    doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, false))
        .await
        .unwrap();

    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn deleting_a_collection_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(doubles.collections.calls(), []);
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn a_failed_maintenance_is_logged_as_a_warning_and_keeps_the_result() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_maintain(|| Err(IndexError::RebuildIncomplete)));
    let confirmation = TestConfirmation::answering(true);
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.delete_collection(delete_notes(&confirmation, true))).await;

    assert!(matches!(
        outcome,
        Ok(DeleteCollectionOutcome::Deleted { .. })
    ));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_rebuild_incomplete_is_the_cause(&logs[0]);
    assert_eq!(logs[0].message, MAINTENANCE_FAILED);
}

#[tokio::test]
async fn a_collection_that_cannot_be_read_returns_the_error_with_the_name_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::new().on_get(|_| {
        Err(CollectionError::CannotRead {
            source: "unreadable".into(),
        })
    }));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_collection(delete_notes(&confirmation, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext { collection: Some(collection), key: None },
            source: CollectionError::CannotRead { .. },
        } if collection == name("notes")
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

fn attributes(json: serde_json::Value) -> TypeAttributes {
    match json {
        serde_json::Value::Object(map) => TypeAttributes(map),
        _ => panic!("the attributes must be an object"),
    }
}

fn create_raw_memos() -> CreateCollectionInput {
    CreateCollectionInput {
        name: name("memos"),
        collection_type: raw(),
        description: Some("memos".to_owned()),
        attributes: attributes(serde_json::json!({ "given": true })),
    }
}

#[tokio::test]
async fn creating_a_collection_stores_the_attributes_its_ingest_returns() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .ingest(
            raw(),
            TestIngest::new().on_check_attributes(|_, dirs| {
                let home = (dirs.home_dir)().unwrap();
                Ok(attributes(serde_json::json!({ "home": home })))
            }),
        )
        .write_lock(Arc::clone(&lock));

    let created = doubles
        .operations()
        .create_collection(create_raw_memos())
        .await
        .unwrap();

    let expected = CollectionInfo {
        name: name("memos"),
        collection_type: raw(),
        attributes: attributes(serde_json::json!({ "home": "/home/user" })),
        description: Some("memos".to_owned()),
    };
    assert_eq!(created, expected);
    assert_eq!(
        doubles.ingests[&raw()].calls(),
        [IngestCall::CheckAttributes(attributes(
            serde_json::json!({ "given": true })
        ))]
    );
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Create(expected)]
    );
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn attributes_the_ingest_refuses_create_nothing() {
    let doubles = Doubles::new().ingest(
        raw(),
        TestIngest::new().on_check_attributes(|_, _| Err(IngestError::AttributesMissingOrExtra)),
    );

    let error = doubles
        .operations()
        .create_collection(create_raw_memos())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Ingest {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IngestError::AttributesMissingOrExtra,
        }
    ));
    assert_eq!(doubles.collections.calls(), []);
}

#[tokio::test]
async fn a_failed_creation_returns_the_error_with_the_name() {
    let doubles = Doubles::new()
        .collections(TestCollections::new().on_create(|_| Err(CollectionError::AlreadyExists)));

    let error = doubles
        .operations()
        .create_collection(create_raw_memos())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext { collection: Some(collection), key: None },
            source: CollectionError::AlreadyExists,
        } if collection == name("memos")
    ));
}

#[tokio::test]
async fn creating_a_collection_while_another_write_is_in_progress_checks_nothing() {
    let doubles = Doubles::new().write_lock(Arc::new(
        TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
    ));

    let error = doubles
        .operations()
        .create_collection(create_raw_memos())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(doubles.collections.calls(), []);
}

fn memos() -> CollectionInfo {
    CollectionInfo {
        name: name("memos"),
        collection_type: raw(),
        attributes: TypeAttributes::default(),
        description: Some("short notes".to_owned()),
    }
}

#[tokio::test]
async fn the_collection_list_has_each_collection_with_its_document_count() {
    let lock = Arc::new(TestWriteLock::new());
    let notes = CollectionInfo {
        attributes: attributes(serde_json::json!({ "path": "/home/user/notes" })),
        ..notes()
    };
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos(), notes]))
        .index(TestIndex::new().on_document_counts(|| Ok(BTreeMap::from([(name("notes"), 12)]))))
        .write_lock(Arc::clone(&lock));

    let list = doubles.operations().list_collections().await.unwrap();

    assert_eq!(
        list,
        CollectionList {
            collections: vec![
                CollectionEntry {
                    name: "memos".to_owned(),
                    collection_type: "raw".to_owned(),
                    description: Some("short notes".to_owned()),
                    document_count: SafeInteger::new(0),
                    attributes: SafeJsonObject::new(serde_json::Map::new()),
                },
                CollectionEntry {
                    name: "notes".to_owned(),
                    collection_type: "directory".to_owned(),
                    description: None,
                    document_count: SafeInteger::new(12),
                    attributes: SafeJsonObject::new(
                        attributes(serde_json::json!({ "path": "/home/user/notes" })).0
                    ),
                },
            ]
        }
    );
    assert_eq!(doubles.collections.calls(), [CollectionsCall::List]);
    assert_eq!(doubles.index.calls(), [IndexCall::DocumentCounts]);
    assert_eq!(lock.state(), LockState::default());
}

#[tokio::test]
async fn a_collection_list_that_cannot_be_read_returns_the_error_without_a_name() {
    let doubles = Doubles::new().collections(TestCollections::new().on_list(|| {
        Err(CollectionError::CannotRead {
            source: "unreadable".into(),
        })
    }));

    let error = doubles.operations().list_collections().await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: CollectionError::CannotRead { .. },
        }
    ));
}

#[tokio::test]
async fn document_counts_that_cannot_be_read_return_the_error_without_a_name() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_document_counts(|| Err(IndexError::RebuildIncomplete)));

    let error = doubles.operations().list_collections().await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
}

/// Collections holding `notes`, whose `change` applies the change to it.
fn collections_changing_notes() -> TestCollections {
    TestCollections::with(vec![notes()]).on_change(|_, change| {
        let mut changed = notes();
        if let Some(description) = &change.description {
            changed.description.clone_from(description);
        }
        if let Some(attributes) = &change.attributes {
            changed.attributes = attributes.clone();
        }
        Ok(changed)
    })
}

fn change_notes(change: CollectionChange) -> ChangeCollectionInput {
    ChangeCollectionInput {
        name: name("notes"),
        change,
    }
}

#[tokio::test]
async fn changing_the_attributes_stores_what_the_collections_ingest_returns() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .collections(collections_changing_notes())
        .ingest(
            directory(),
            TestIngest::new().on_check_attributes(|_, dirs| {
                let work = (dirs.current_dir)().unwrap();
                Ok(attributes(serde_json::json!({ "path": work })))
            }),
        )
        .write_lock(Arc::clone(&lock));
    let given = attributes(serde_json::json!({ "path": "." }));
    let checked = attributes(serde_json::json!({ "path": "/home/user/work" }));

    let changed = doubles
        .operations()
        .change_collection(change_notes(CollectionChange {
            description: Some(Some("notes".to_owned())),
            attributes: Some(given.clone()),
        }))
        .await
        .unwrap();

    assert_eq!(
        changed,
        CollectionInfo {
            attributes: checked.clone(),
            description: Some("notes".to_owned()),
            ..notes()
        }
    );
    assert_eq!(
        doubles.ingests[&directory()].calls(),
        [IngestCall::CheckAttributes(given)]
    );
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(
        doubles.collections.calls(),
        [
            CollectionsCall::Get(name("notes")),
            CollectionsCall::Change(
                name("notes"),
                CollectionChange {
                    description: Some(Some("notes".to_owned())),
                    attributes: Some(checked),
                }
            ),
        ]
    );
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn changing_only_the_description_does_not_check_attributes() {
    let doubles = Doubles::new().collections(collections_changing_notes());
    let change = CollectionChange {
        description: Some(None),
        attributes: None,
    };

    doubles
        .operations()
        .change_collection(change_notes(change.clone()))
        .await
        .unwrap();

    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.collections.calls(),
        [
            CollectionsCall::Get(name("notes")),
            CollectionsCall::Change(name("notes"), change),
        ]
    );
}

#[tokio::test]
async fn changing_a_missing_collection_checks_and_changes_nothing() {
    let doubles = Doubles::new();

    let error = doubles
        .operations()
        .change_collection(change_notes(CollectionChange {
            description: None,
            attributes: Some(TypeAttributes::default()),
        }))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("notes")
    ));
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
}

#[tokio::test]
async fn changed_attributes_the_ingest_refuses_change_nothing() {
    let doubles = Doubles::new()
        .collections(collections_changing_notes())
        .ingest(
            directory(),
            TestIngest::new().on_check_attributes(|_, _| Err(IngestError::LocationTypeMismatch)),
        );

    let error = doubles
        .operations()
        .change_collection(change_notes(CollectionChange {
            description: None,
            attributes: Some(TypeAttributes::default()),
        }))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Ingest {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IngestError::LocationTypeMismatch,
        }
    ));
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
}

#[tokio::test]
async fn a_failed_change_returns_the_error_with_the_name() {
    let doubles =
        Doubles::new().collections(TestCollections::with(vec![notes()]).on_change(|_, _| {
            Err(CollectionError::CannotWrite {
                source: "read-only".into(),
            })
        }));

    let error = doubles
        .operations()
        .change_collection(change_notes(CollectionChange {
            description: Some(None),
            attributes: None,
        }))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext { collection: Some(collection), key: None },
            source: CollectionError::CannotWrite { .. },
        } if collection == name("notes")
    ));
}

#[tokio::test]
async fn changing_a_collection_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(collections_changing_notes())
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));

    let error = doubles
        .operations()
        .change_collection(change_notes(CollectionChange {
            description: Some(None),
            attributes: None,
        }))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.collections.calls(), []);
}

#[tokio::test]
async fn the_settings_are_the_current_entries_in_their_order() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new().write_lock(Arc::clone(&lock));

    let settings = doubles.operations().list_settings().await.unwrap();

    assert_eq!(
        settings,
        SettingsOutput {
            entries: TestConfig::default_settings()
                .entries()
                .into_iter()
                .map(|(name, value)| (name, SafeJsonValue::new(value)))
                .collect()
        }
    );
    assert_eq!(doubles.config.calls(), [ConfigCall::Current]);
    assert_eq!(lock.state(), LockState::default());
}

#[tokio::test]
async fn settings_that_cannot_be_read_return_the_config_error() {
    let doubles = Doubles::new().config(TestConfig::new().on_current(|| {
        Err(ConfigError::CannotRead {
            source: "unreadable".into(),
        })
    }));

    let error = doubles.operations().list_settings().await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::Config(ConfigError::CannotRead { .. })
    ));
}

fn key(key: &str) -> Key {
    Key(key.to_owned())
}

/// An index that has the document `a.md` in `notes` and in `memos`.
fn index_with_a_md() -> TestIndex {
    TestIndex::new().on_has_document(|collection, key| {
        Ok(key.0 == "a.md" && ["notes", "memos"].contains(&collection.0.as_str()))
    })
}

fn delete_a_md(collection: &str, confirmation: &TestConfirmation) -> DeleteDocumentInput {
    DeleteDocumentInput {
        collection: name(collection),
        key: key("a.md"),
        confirmation: Box::new(confirmation.clone()),
        optimize: true,
    }
}

#[tokio::test]
async fn deleting_a_document_asks_then_deletes_and_maintains_holding_the_lock() {
    let lock = Arc::new(TestWriteLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in_maintain = Arc::clone(&seen);
    let lock_in_maintain = Arc::clone(&lock);
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_with_a_md().on_maintain(move || {
            seen_in_maintain
                .lock()
                .unwrap()
                .push(lock_in_maintain.state());
            Ok(())
        }))
        .write_lock(Arc::clone(&lock));
    let confirmation = TestConfirmation::answering(true);

    let outcome = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        DeleteDocumentOutcome::Deleted { collection, key: deleted, supports_update: true }
            if collection == name("notes") && deleted == key("a.md")
    ));
    assert_eq!(confirmation.asked(), 1);
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::HasDocument(name("notes"), key("a.md")),
            IndexCall::DeleteDocument(name("notes"), key("a.md")),
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [LockState {
            acquired: 1,
            held: true
        }]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn a_document_deleted_from_a_collection_without_update_says_so() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos()]))
        .index(index_with_a_md());
    let confirmation = TestConfirmation::answering(true);

    let outcome = doubles
        .operations()
        .delete_document(delete_a_md("memos", &confirmation))
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        DeleteDocumentOutcome::Deleted {
            supports_update: false,
            ..
        }
    ));
}

#[tokio::test]
async fn deleting_a_document_of_a_missing_collection_does_not_ask_and_still_maintains() {
    let doubles = Doubles::new().index(index_with_a_md());
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("notes")
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

#[tokio::test]
async fn deleting_a_missing_document_does_not_ask_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::DocumentNotFound { collection, key: missing }
            if collection == name("notes") && missing == key("a.md")
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::HasDocument(name("notes"), key("a.md")),
            IndexCall::Maintain,
        ]
    );
}

#[tokio::test]
async fn a_declined_document_deletion_deletes_nothing_and_does_not_maintain() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_with_a_md());
    let confirmation = TestConfirmation::answering(false);

    let outcome = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap();

    assert!(matches!(outcome, DeleteDocumentOutcome::Declined));
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::HasDocument(name("notes"), key("a.md"))]
    );
}

#[tokio::test]
async fn an_index_that_cannot_tell_whether_the_document_exists_returns_its_error_with_the_name_and_key()
 {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_has_document(|_, _| Err(IndexError::RebuildIncomplete)));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext { collection: Some(collection), key: Some(failed) },
            source: IndexError::RebuildIncomplete,
        } if collection == name("notes") && failed == key("a.md")
    ));
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::HasDocument(name("notes"), key("a.md")),
            IndexCall::Maintain,
        ]
    );
}

#[tokio::test]
async fn a_failed_document_deletion_returns_the_error_with_the_name_and_key_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_with_a_md().on_delete_document(|_, _| Err(IndexError::RebuildIncomplete)));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext { collection: Some(collection), key: Some(failed) },
            source: IndexError::RebuildIncomplete,
        } if collection == name("notes") && failed == key("a.md")
    ));
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::HasDocument(name("notes"), key("a.md")),
            IndexCall::DeleteDocument(name("notes"), key("a.md")),
            IndexCall::Maintain,
        ]
    );
}

#[tokio::test]
async fn deleting_a_document_without_optimizing_does_not_maintain() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_with_a_md());
    let confirmation = TestConfirmation::answering(true);
    let input = DeleteDocumentInput {
        optimize: false,
        ..delete_a_md("notes", &confirmation)
    };

    doubles.operations().delete_document(input).await.unwrap();

    assert!(!doubles.index.calls().contains(&IndexCall::Maintain));
}

#[tokio::test]
async fn deleting_a_document_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(index_with_a_md())
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("notes", &confirmation))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.collections.calls(), []);
    assert_eq!(doubles.index.calls(), []);
}

fn search_input(collections: &[&str]) -> SearchInput {
    SearchInput {
        query: "rust".to_owned(),
        mode: SearchMode::Hybrid,
        collections: collections
            .iter()
            .map(|collection| name(collection))
            .collect(),
        limit: SearchLimit::new(5),
        unit: SearchUnit::Chunk,
    }
}

#[tokio::test]
async fn a_search_returns_the_hits_and_warnings_of_query_with_its_input() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos(), notes()]))
        .query(TestQuery::new().on_search(|_| {
            Ok(SearchResult {
                hits: vec![Hit {
                    collection: name("notes"),
                    key: key("a.md"),
                    score: 0.5,
                    snippet: "rust".to_owned(),
                    chunk_index: 2,
                    chunk_count: 3,
                }],
                warnings: vec!["a warning".to_owned()],
            })
        }))
        .write_lock(Arc::clone(&lock));

    let output = doubles
        .operations()
        .search(search_input(&["notes", "memos"]))
        .await
        .unwrap();

    assert_eq!(
        output,
        SearchOutput {
            query: "rust".to_owned(),
            mode: SearchModeName::Hybrid,
            unit: SearchUnitName::Chunk,
            collections: vec!["notes".to_owned(), "memos".to_owned()],
            limit: SearchLimit::new(5),
            hit_count: SafeInteger::new(1),
            hits: vec![SearchHit {
                collection: "notes".to_owned(),
                key: "a.md".to_owned(),
                score: Score::new(0.5),
                snippet: "rust".to_owned(),
                chunk_index: SafeInteger::new(2),
                chunk_count: SafeInteger::new(3),
            }],
            warnings: vec!["a warning".to_owned()],
        }
    );
    assert_eq!(
        doubles.collections.calls(),
        [
            CollectionsCall::Get(name("notes")),
            CollectionsCall::Get(name("memos")),
        ]
    );
    assert_eq!(
        doubles.query.calls(),
        [QueryCall::Search(SearchRequest {
            query: "rust".to_owned(),
            mode: SearchMode::Hybrid,
            collections: vec![name("notes"), name("memos")],
            limit: 5,
            unit: SearchUnit::Chunk,
        })]
    );
    assert_eq!(lock.state(), LockState::default());
}

#[tokio::test]
#[should_panic(expected = "the score NaN is not finite")]
async fn a_hit_whose_score_is_not_finite_is_an_unexpected_failure() {
    let doubles = Doubles::new().query(TestQuery::new().on_search(|_| {
        Ok(SearchResult {
            hits: vec![Hit {
                collection: name("notes"),
                key: key("a.md"),
                score: f64::NAN,
                snippet: "rust".to_owned(),
                chunk_index: 0,
                chunk_count: 1,
            }],
            warnings: Vec::new(),
        })
    }));

    let _ = doubles.operations().search(search_input(&[])).await;
}

#[tokio::test]
async fn a_search_of_every_collection_checks_no_collection() {
    let doubles = Doubles::new();
    let input = SearchInput {
        mode: SearchMode::FullText,
        unit: SearchUnit::Document,
        ..search_input(&[])
    };

    let output = doubles.operations().search(input).await.unwrap();

    assert_eq!(output.mode, SearchModeName::Fts);
    assert_eq!(output.unit, SearchUnitName::Document);
    assert!(output.collections.is_empty());
    assert_eq!(output.hit_count, SafeInteger::new(0));
    assert_eq!(doubles.collections.calls(), []);
}

#[tokio::test]
async fn a_vector_search_is_written_as_vector() {
    let doubles = Doubles::new();
    let input = SearchInput {
        mode: SearchMode::Vector,
        ..search_input(&[])
    };

    let output = doubles.operations().search(input).await.unwrap();

    assert_eq!(output.mode, SearchModeName::Vector);
}

#[tokio::test]
async fn searching_a_missing_collection_does_not_search() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));

    let error = doubles
        .operations()
        .search(search_input(&["notes", "memos"]))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("memos")
    ));
    assert_eq!(doubles.query.calls(), []);
}

#[tokio::test]
async fn a_failed_search_returns_the_query_error_without_a_name() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(TestQuery::new().on_search(|_| Err(QueryError::EmptyQuery)));

    let error = doubles
        .operations()
        .search(search_input(&["notes"]))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Query {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: QueryError::EmptyQuery,
        }
    ));
}

fn get_a_md(size_limit: Option<u64>) -> GetDocumentInput {
    GetDocumentInput {
        collection: name("notes"),
        key: key("a.md"),
        range: ChunkRangeRequest {
            start: Some(1),
            end: None,
        },
        size_limit,
    }
}

/// A query whose `a.md` is "あいう", 9 bytes in UTF-8.
fn query_with_a_md() -> TestQuery {
    TestQuery::new().on_get_document(|_, _, _| Ok("あいう".to_owned()))
}

#[tokio::test]
async fn getting_a_document_returns_its_text_with_the_name_and_key() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(query_with_a_md())
        .write_lock(Arc::clone(&lock));

    let text = doubles
        .operations()
        .get_document(get_a_md(None))
        .await
        .unwrap();

    assert_eq!(
        text,
        DocumentText {
            collection: "notes".to_owned(),
            key: "a.md".to_owned(),
            text: "あいう".to_owned(),
        }
    );
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
    assert_eq!(
        doubles.query.calls(),
        [QueryCall::GetDocument(
            name("notes"),
            key("a.md"),
            ChunkRangeRequest {
                start: Some(1),
                end: None
            }
        )]
    );
    assert_eq!(lock.state(), LockState::default());
}

#[tokio::test]
async fn a_document_as_large_as_the_limit_is_returned() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(query_with_a_md());

    let text = doubles
        .operations()
        .get_document(get_a_md(Some(9)))
        .await
        .unwrap();

    assert_eq!(text.text, "あいう");
}

#[tokio::test]
async fn a_chunk_larger_than_the_limit_is_not_returned() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(query_with_a_md());

    let error = doubles
        .operations()
        .get_document(get_a_md(Some(8)))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::OutputTooLarge {
            part: OutputPart::Chunk,
            size_bytes: 9,
            limit_bytes: 8
        }
    ));
}

#[tokio::test]
async fn a_body_larger_than_the_limit_is_said_to_be_the_body() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(query_with_a_md());
    let input = GetDocumentInput {
        range: ChunkRangeRequest {
            start: None,
            end: None,
        },
        ..get_a_md(Some(8))
    };

    let error = doubles.operations().get_document(input).await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::OutputTooLarge {
            part: OutputPart::Body,
            ..
        }
    ));
}

#[tokio::test]
async fn a_chunk_range_larger_than_the_limit_is_said_to_be_the_range() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .query(query_with_a_md());
    let input = GetDocumentInput {
        range: ChunkRangeRequest {
            start: Some(0),
            end: Some(1),
        },
        ..get_a_md(Some(8))
    };

    let error = doubles.operations().get_document(input).await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::OutputTooLarge {
            part: OutputPart::ChunkRange,
            ..
        }
    ));
}

#[tokio::test]
async fn getting_a_document_of_a_missing_collection_reads_no_document() {
    let doubles = Doubles::new().query(query_with_a_md());

    let error = doubles
        .operations()
        .get_document(get_a_md(None))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("notes")
    ));
    assert_eq!(doubles.query.calls(), []);
}

#[tokio::test]
async fn a_failed_get_returns_the_query_error_with_the_name_and_key() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));

    let error = doubles
        .operations()
        .get_document(get_a_md(None))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Query {
            context: ErrorContext { collection: Some(collection), key: Some(missing) },
            source: QueryError::DocumentNotFound,
        } if collection == name("notes") && missing == key("a.md")
    ));
}

fn status_with(dimension: DimensionResult) -> IndexStatus {
    IndexStatus {
        compared_model_name: "all-minilm".to_owned(),
        compared_dimension: dimension,
        models: vec![ModelStatus {
            model: EmbeddingModel {
                name: "all-minilm".to_owned(),
                dimension: 384,
            },
            same_as_compared: Some(true),
            chunk_count: 30,
            truncated_chunk_count: 2,
        }],
        collections: BTreeMap::new(),
        document_count: 7,
        chunk_count: 35,
        chunks_without_vector: 5,
    }
}

#[tokio::test]
async fn the_status_is_the_whole_index_compared_with_the_configured_model() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos(), notes()]))
        .index(TestIndex::new().on_status(|_, _, _| Ok(status_with(DimensionResult::Known(384)))))
        .write_lock(Arc::clone(&lock));

    let status = doubles.operations().status().await.unwrap();

    assert_eq!(
        status,
        StatusOutput {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            configured_embedding_model: "all-minilm".to_owned(),
            configured_embedding_dimension: Some(SafeInteger::new(384)),
            index_embedding_models: vec![IndexEmbeddingModel {
                model: "all-minilm".to_owned(),
                dimension: SafeInteger::new(384),
                configured: Some(true),
                chunk_count: SafeInteger::new(30),
                truncated_chunk_count: SafeInteger::new(2),
            }],
            chunks_without_embedding_count: SafeInteger::new(5),
            collection_count: SafeInteger::new(2),
            document_count: SafeInteger::new(7),
            chunk_count: SafeInteger::new(35),
            warnings: Vec::new(),
        }
    );
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::Status {
            scope: StatusScope::All,
            compared_model_name: "all-minilm".to_owned(),
            check: DimensionCheck::Always,
        }]
    );
    assert_eq!(doubles.collections.calls(), [CollectionsCall::List]);
    assert_eq!(lock.state(), LockState::default());
}

#[tokio::test]
#[should_panic(expected = "9007199254740992 is larger than 2^53 - 1")]
async fn a_status_with_more_chunks_than_2_to_the_53_minus_1_is_an_unexpected_failure() {
    let doubles = Doubles::new().index(TestIndex::new().on_status(|_, _, _| {
        let mut status = status_with(DimensionResult::Known(384));
        status.chunk_count = 1 << 53;
        Ok(status)
    }));

    let _ = doubles.operations().status().await;
}

#[tokio::test]
async fn a_dimension_that_could_not_be_checked_is_null_with_a_warning_giving_the_reason() {
    let doubles = Doubles::new().index(TestIndex::new().on_status(|_, _, _| {
        let mut status = status_with(DimensionResult::Unavailable(
            EmbeddingError::ModelUnavailable {
                source: "Ollama is not running".into(),
            },
        ));
        status.models[0].same_as_compared = None;
        Ok(status)
    }));

    let status = doubles.operations().status().await.unwrap();

    assert_eq!(status.configured_embedding_dimension, None);
    assert_eq!(status.index_embedding_models[0].configured, None);
    assert_eq!(status.warnings.len(), 1);
    assert_eq!(
        status.warnings[0],
        format!("{DIMENSION_UNCHECKED}: the embedding model cannot be used: Ollama is not running")
    );
}

#[tokio::test]
async fn a_status_that_cannot_be_read_returns_the_index_error_without_a_name() {
    let doubles = Doubles::new()
        .index(TestIndex::new().on_status(|_, _, _| Err(IndexError::RebuildIncomplete)));

    let error = doubles.operations().status().await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
}

#[tokio::test]
async fn collections_that_cannot_be_read_for_the_status_return_the_error_without_a_name() {
    let doubles = Doubles::new().collections(TestCollections::new().on_list(|| {
        Err(CollectionError::CannotRead {
            source: "unreadable".into(),
        })
    }));

    let error = doubles.operations().status().await.unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: CollectionError::CannotRead { .. },
        }
    ));
}

#[tokio::test]
async fn optimizing_tells_the_progress_then_maintains_holding_the_lock() {
    let lock = Arc::new(TestWriteLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let doubles = Doubles::new()
        .index(index_seeing_lock(&lock, &seen))
        .write_lock(Arc::clone(&lock));
    let progress = Arc::new(TestProgress::new());

    doubles
        .operations()
        .optimize(OptimizeInput {
            progress: progress.clone(),
        })
        .await
        .unwrap();

    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
    assert_eq!(
        *seen.lock().unwrap(),
        [LockState {
            acquired: 1,
            held: true
        }]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn a_failed_optimization_is_an_error() {
    let doubles =
        Doubles::new().index(TestIndex::new().on_maintain(|| Err(IndexError::RebuildIncomplete)));

    let error = doubles
        .operations()
        .optimize(OptimizeInput {
            progress: Arc::new(TestProgress::new()),
        })
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
}

#[tokio::test]
async fn optimizing_while_another_write_is_in_progress_does_nothing() {
    let doubles = Doubles::new().write_lock(Arc::new(
        TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
    ));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .optimize(OptimizeInput {
            progress: progress.clone(),
        })
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(progress.events(), []);
    assert_eq!(doubles.index.calls(), []);
}

fn rebuild_input(progress: &Arc<TestProgress>, optimize: bool) -> RebuildInput {
    RebuildInput {
        include_configured_model: true,
        progress: progress.clone(),
        optimize,
    }
}

/// The status of an index whose `notes` has `truncated` chunks whose vector
/// does not cover the whole chunk.
fn status_with_truncated_notes(truncated: u64) -> IndexStatus {
    let mut status = status_with(DimensionResult::NotChecked);
    status.collections = BTreeMap::from([(
        name("notes"),
        vec![ModelStatus {
            model: EmbeddingModel {
                name: "all-minilm".to_owned(),
                dimension: 384,
            },
            same_as_compared: None,
            chunk_count: 30,
            truncated_chunk_count: truncated,
        }],
    )]);
    status
}

#[tokio::test]
async fn rebuilding_passes_the_progress_reads_the_status_then_maintains() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .index(
            TestIndex::new()
                .on_rebuild_embeddings(|_, _, progress| {
                    progress.report(Progress {
                        done: 1,
                        total: Some(2),
                    });
                    Ok(120)
                })
                .on_status(|_, _, _| Ok(status_with_truncated_notes(0))),
        )
        .write_lock(Arc::clone(&lock));
    let progress = Arc::new(TestProgress::new());

    let (outcome, logs) = capture_async(
        doubles
            .operations()
            .rebuild_embeddings(rebuild_input(&progress, true)),
    )
    .await;

    let outcome = outcome.unwrap();
    assert_eq!(outcome.rebuilt_chunks, 120);
    assert!(outcome.status_error.is_none());
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::RebuildEmbeddings {
                include_configured_model: true,
                parallelism: 4,
            },
            IndexCall::Status {
                scope: StatusScope::All,
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::Never,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        progress.events(),
        [
            ProgressEvent::Report(Progress {
                done: 1,
                total: Some(2)
            }),
            ProgressEvent::MaintenanceStarted,
        ]
    );
    assert_eq!(logs, []);
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn truncated_chunks_after_a_rebuild_are_warned_about_for_each_collection() {
    let doubles = Doubles::new().index(TestIndex::new().on_status(|_, _, _| {
        let mut status = status_with(DimensionResult::NotChecked);
        status.collections = BTreeMap::from([
            (
                name("archives"),
                vec![
                    model_status("all-minilm", 1),
                    model_status("embeddinggemma", 2),
                ],
            ),
            (name("memos"), vec![model_status("all-minilm", 0)]),
            (name("notes"), vec![model_status("all-minilm", 4)]),
        ]);
        Ok(status)
    }));
    let progress = Arc::new(TestProgress::new());

    let (outcome, logs) = capture_async(
        doubles
            .operations()
            .rebuild_embeddings(rebuild_input(&progress, false)),
    )
    .await;

    outcome.unwrap();
    let warned: Vec<(&str, &str)> = logs
        .iter()
        .map(|log| {
            assert_eq!(log.level, tracing::Level::WARN);
            assert_eq!(log.message, TRUNCATED_CHUNKS);
            (field(log, "collection"), field(log, "chunks"))
        })
        .collect();
    assert_eq!(warned, [("archives", "3"), ("notes", "4")]);
}

/// A model's chunks in a collection, `truncated` of which are longer than
/// its context length.
fn model_status(model: &str, truncated: u64) -> ModelStatus {
    ModelStatus {
        model: EmbeddingModel {
            name: model.to_owned(),
            dimension: 384,
        },
        same_as_compared: None,
        chunk_count: 30,
        truncated_chunk_count: truncated,
    }
}

/// The value of the log's field `name`.
fn field<'a>(log: &'a LogRecord, name: &str) -> &'a str {
    log.fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.as_str())
        .unwrap()
}

#[tokio::test]
async fn a_status_that_cannot_be_read_after_a_rebuild_is_returned_and_logged() {
    let doubles = Doubles::new()
        .index(TestIndex::new().on_status(|_, _, _| Err(IndexError::RebuildIncomplete)));
    let progress = Arc::new(TestProgress::new());

    let (outcome, logs) = capture_async(
        doubles
            .operations()
            .rebuild_embeddings(rebuild_input(&progress, true)),
    )
    .await;

    let outcome = outcome.unwrap();
    assert!(matches!(
        outcome.status_error,
        Some(IndexError::RebuildIncomplete)
    ));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_rebuild_incomplete_is_the_cause(&logs[0]);
    assert_eq!(logs[0].message, STATUS_UNREADABLE);
    assert!(doubles.index.calls().contains(&IndexCall::Maintain));
}

#[tokio::test]
async fn a_failed_rebuild_reads_no_status_and_still_maintains() {
    let doubles = Doubles::new().index(
        TestIndex::new().on_rebuild_embeddings(|_, _, _| Err(IndexError::RebuildIncomplete)),
    );
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .rebuild_embeddings(rebuild_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::RebuildEmbeddings {
                include_configured_model: true,
                parallelism: 4,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[tokio::test]
async fn rebuilding_without_optimizing_does_not_maintain() {
    let doubles = Doubles::new();
    let progress = Arc::new(TestProgress::new());

    doubles
        .operations()
        .rebuild_embeddings(rebuild_input(&progress, false))
        .await
        .unwrap();

    assert!(!doubles.index.calls().contains(&IndexCall::Maintain));
    assert_eq!(progress.events(), []);
}

#[tokio::test]
async fn rebuilding_while_another_write_is_in_progress_does_nothing() {
    let doubles = Doubles::new().write_lock(Arc::new(
        TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
    ));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .rebuild_embeddings(rebuild_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(progress.events(), []);
}

fn put_into(collection: &str, content: &TestContentReader) -> PutInput {
    PutInput {
        collection: name(collection),
        key: key("a.md"),
        media_type: "markdown".to_owned(),
        content: Box::new(content.clone()),
        overwrite: false,
        optimize: true,
    }
}

#[tokio::test]
async fn putting_a_document_ingests_its_content_reads_the_status_then_maintains() {
    let lock = Arc::new(TestWriteLock::new());
    let memos = CollectionInfo {
        attributes: attributes(serde_json::json!({ "kept": true })),
        ..memos()
    };
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos]))
        .ingest(
            raw(),
            TestIngest::new()
                .supporting(false, true)
                .on_put(|_| Ok(DocumentOutcome::Changed)),
        )
        .write_lock(Arc::clone(&lock));
    let content = TestContentReader::returning(b"# A");

    let outcome = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap();

    assert_eq!(outcome.collection, name("memos"));
    assert_eq!(outcome.key, key("a.md"));
    assert_eq!(outcome.outcome, DocumentOutcome::Changed);
    assert!(outcome.status_error.is_none());
    assert_eq!(content.reads(), 1);
    assert_eq!(
        doubles.ingests[&raw()].calls(),
        [IngestCall::Put(PutInstruction {
            collection: name("memos"),
            attributes: attributes(serde_json::json!({ "kept": true })),
            key: key("a.md"),
            media_type: "markdown".to_owned(),
            content: b"# A".to_vec(),
            overwrite: false,
        })]
    );
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("memos")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn putting_into_a_collection_that_takes_no_content_reads_nothing_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .ingest(directory(), TestIngest::new().supporting(true, false));
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("notes", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::PutNotSupported { collection } if collection == name("notes")
    ));
    assert_eq!(content.reads(), 0);
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn content_that_cannot_be_read_ingests_nothing_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![memos()]));
    let content = TestContentReader::failing(std::io::ErrorKind::PermissionDenied);

    let error = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::ContentUnreadable { source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn an_index_that_cannot_ingest_reads_no_content_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos()]))
        .index(TestIndex::new().on_check_ingestable(|| Err(IndexError::RebuildIncomplete)));
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
    assert_eq!(content.reads(), 0);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn a_failed_put_returns_the_error_with_the_name_and_key_reads_no_status_and_still_maintains()
{
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos()]))
        .ingest(
            raw(),
            TestIngest::new()
                .supporting(false, true)
                .on_put(|_| Err(IngestError::UnsupportedFileType)),
        );
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Ingest {
            context: ErrorContext { collection: Some(collection), key: Some(failed) },
            source: IngestError::UnsupportedFileType,
        } if collection == name("memos") && failed == key("a.md")
    ));
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn putting_into_a_missing_collection_checks_nothing_and_still_maintains() {
    let doubles = Doubles::new();
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("memos")
    ));
    assert_eq!(content.reads(), 0);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

#[tokio::test]
async fn a_status_that_cannot_be_read_after_a_put_is_returned_and_logged() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos()]))
        .index(TestIndex::new().on_status(|_, _, _| Err(IndexError::RebuildIncomplete)));
    let content = TestContentReader::returning(b"# A");
    let operations = doubles.operations();

    let (outcome, logs) = capture_async(operations.put_document(put_into("memos", &content))).await;

    assert!(matches!(
        outcome.unwrap().status_error,
        Some(IndexError::RebuildIncomplete)
    ));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_rebuild_incomplete_is_the_cause(&logs[0]);
    assert_eq!(logs[0].message, STATUS_UNREADABLE);
}

#[tokio::test]
async fn putting_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![memos()]))
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("memos", &content))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(content.reads(), 0);
    assert_eq!(doubles.collections.calls(), []);
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn putting_without_optimizing_does_not_maintain() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![memos()]));
    let content = TestContentReader::returning(b"# A");
    let input = PutInput {
        optimize: false,
        ..put_into("memos", &content)
    };

    doubles.operations().put_document(input).await.unwrap();

    assert!(!doubles.index.calls().contains(&IndexCall::Maintain));
}

fn report(added: u64, failed: u64) -> UpdateReport {
    UpdateReport {
        added,
        failed,
        ..UpdateReport::default()
    }
}

fn archives() -> CollectionInfo {
    CollectionInfo {
        name: name("archives"),
        ..notes()
    }
}

/// A directory ingest that updates each collection as `update` answers for
/// its name, reporting all its documents done first.
fn directory_ingest(
    update: impl Fn(&str) -> Result<UpdateReport, UpdateStopped> + Send + Sync + 'static,
) -> TestIngest {
    TestIngest::new().on_update(move |instruction, progress| {
        let result = update(&instruction.collection.0);
        if let Ok(counts) = &result {
            report_documents(progress, counts.added, counts.added);
        }
        result
    })
}

fn update_all_input(progress: &Arc<TestProgress>, optimize: bool) -> UpdateAllInput {
    UpdateAllInput {
        overwrite: true,
        progress: progress.clone(),
        optimize,
    }
}

fn instruction(collection: &str) -> UpdateInstruction {
    UpdateInstruction {
        collection: name(collection),
        attributes: TypeAttributes::default(),
        overwrite: true,
        parallelism: 4,
    }
}

#[tokio::test]
async fn updating_all_updates_each_updatable_collection_and_sums_the_counts() {
    let lock = Arc::new(TestWriteLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![archives(), memos(), notes()]))
        .ingest(
            directory(),
            directory_ingest(|collection| match collection {
                "archives" => Ok(report(2, 0)),
                _ => Ok(report(3, 1)),
            }),
        )
        .index(index_seeing_lock(&lock, &seen))
        .write_lock(Arc::clone(&lock));
    let progress = Arc::new(TestProgress::new());

    let outcome = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap();

    assert_eq!(
        outcome.counts,
        UpdateReport {
            added: 5,
            failed: 1,
            ..UpdateReport::default()
        }
    );
    assert!(outcome.unreadable_sources.is_empty());
    assert!(outcome.status_error.is_none());
    assert_eq!(
        doubles.ingests[&directory()].calls(),
        [
            IngestCall::Update(instruction("archives")),
            IngestCall::Update(instruction("notes")),
        ]
    );
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("archives"), name("notes")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        progress.events(),
        [
            ProgressEvent::Report(Progress {
                done: 2,
                total: Some(2)
            }),
            ProgressEvent::Report(Progress {
                done: 5,
                total: Some(5)
            }),
            ProgressEvent::MaintenanceStarted,
        ]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [LockState {
            acquired: 1,
            held: true
        }]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn updating_all_goes_on_past_a_collection_whose_source_cannot_be_read() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![archives(), notes()]))
        .ingest(
            directory(),
            directory_ingest(|collection| match collection {
                "archives" => Err(UpdateStopped {
                    counts: UpdateReport {
                        unreadable_places: 1,
                        ..UpdateReport::default()
                    },
                    error: IngestError::SourceUnreadable {
                        source: "missing".into(),
                    },
                }),
                _ => Ok(report(3, 0)),
            }),
        );
    let progress = Arc::new(TestProgress::new());

    let outcome = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap();

    assert_eq!(
        outcome.counts,
        UpdateReport {
            unreadable_places: 1,
            ..report(3, 0)
        }
    );
    assert_eq!(outcome.unreadable_sources, [name("archives")]);
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("notes")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
}

#[tokio::test]
async fn an_update_of_all_that_stops_returns_the_counts_so_far_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![archives(), memos(), notes()]))
        .ingest(
            directory(),
            directory_ingest(|collection| match collection {
                "archives" => Ok(report(2, 0)),
                _ => Err(UpdateStopped {
                    counts: report(1, 0),
                    error: IngestError::Index(IndexError::RebuildIncomplete),
                }),
            }),
        );
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::UpdateStopped {
            counts,
            context: ErrorContext { collection: Some(collection), key: None },
            source: IngestError::Index(IndexError::RebuildIncomplete),
        } if counts == report(3, 0) && collection == name("notes")
    ));
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("archives")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        progress.events().last(),
        Some(&ProgressEvent::MaintenanceStarted)
    );
}

#[tokio::test]
async fn an_update_of_all_that_finishes_skips_then_stops_sums_all_three_counts_and_still_maintains()
{
    let reports = CollectionInfo {
        name: name("reports"),
        ..notes()
    };
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![archives(), notes(), reports]))
        .ingest(
            directory(),
            directory_ingest(|collection| match collection {
                "archives" => Ok(report(2, 0)),
                "notes" => Err(UpdateStopped {
                    counts: UpdateReport {
                        unreadable_places: 1,
                        ..UpdateReport::default()
                    },
                    error: IngestError::SourceUnreadable {
                        source: "missing".into(),
                    },
                }),
                _ => Err(UpdateStopped {
                    counts: report(1, 0),
                    error: IngestError::Index(IndexError::RebuildIncomplete),
                }),
            }),
        );
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    let summed = UpdateReport {
        unreadable_places: 1,
        ..report(3, 0)
    };
    assert!(matches!(
        error,
        OperationError::UpdateStopped {
            counts,
            context: ErrorContext { collection: Some(collection), key: None },
            source: IngestError::Index(IndexError::RebuildIncomplete),
        } if counts == summed && collection == name("reports")
    ));
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("archives")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        progress.events().last(),
        Some(&ProgressEvent::MaintenanceStarted)
    );
}

#[tokio::test]
async fn updating_all_with_no_collection_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new();
    let progress = Arc::new(TestProgress::new());

    let outcome = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap();

    assert_eq!(outcome.counts, UpdateReport::default());
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[tokio::test]
async fn an_index_that_cannot_ingest_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_check_ingestable(|| Err(IndexError::RebuildIncomplete)));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn a_status_that_cannot_be_read_after_updating_all_is_returned_and_logged() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_status(|_, _, _| Err(IndexError::RebuildIncomplete)));
    let progress = Arc::new(TestProgress::new());
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.update_all(update_all_input(&progress, true))).await;

    assert!(matches!(
        outcome.unwrap().status_error,
        Some(IndexError::RebuildIncomplete)
    ));
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_rebuild_incomplete_is_the_cause(&logs[0]);
    assert_eq!(logs[0].message, STATUS_UNREADABLE);
}

#[tokio::test]
async fn updating_all_without_optimizing_does_not_maintain() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes()]));
    let progress = Arc::new(TestProgress::new());

    doubles
        .operations()
        .update_all(update_all_input(&progress, false))
        .await
        .unwrap();

    assert!(!doubles.index.calls().contains(&IndexCall::Maintain));
    assert!(
        !progress
            .events()
            .contains(&ProgressEvent::MaintenanceStarted)
    );
}

#[tokio::test]
async fn updating_all_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.collections.calls(), []);
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(progress.events(), []);
}

#[tokio::test]
async fn truncated_chunks_after_an_update_are_warned_about() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_status(|_, _, _| {
            let mut status = status_with(DimensionResult::NotChecked);
            status.collections =
                BTreeMap::from([(name("notes"), vec![model_status("all-minilm", 2)])]);
            Ok(status)
        }));
    let progress = Arc::new(TestProgress::new());
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.update_all(update_all_input(&progress, false))).await;

    outcome.unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].message, TRUNCATED_CHUNKS);
    assert_eq!(field(&logs[0], "collection"), "notes");
    assert_eq!(field(&logs[0], "chunks"), "2");
}

#[tokio::test]
async fn a_dimension_that_could_not_be_checked_after_an_update_is_warned_about() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_status(|_, _, _| {
            Ok(status_with(DimensionResult::Unavailable(
                EmbeddingError::ModelUnavailable {
                    source: "Ollama is not running".into(),
                },
            )))
        }));
    let progress = Arc::new(TestProgress::new());
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.update_all(update_all_input(&progress, false))).await;

    assert!(outcome.unwrap().status_error.is_none());
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert_eq!(logs[0].message, DIMENSION_UNCHECKED);
    assert!(logs[0].fields.contains(&(
        "cause".to_owned(),
        "the embedding model cannot be used: Ollama is not running".to_owned()
    )));
}

/// How to fix an embedding model cache directory that could not be
/// determined, written out in full on one line, so that a change to the
/// constant the operations use shows here.
const CACHE_DIR_FIX: &str = "Give an absolute path for the embedding model cache directory with \
    --model-cache-dir, then run the operation again. If the working directory cannot be read, you \
    can also move to a working directory that mdya can read, and run the operation again. For the \
    MCP server, start it again after either of these.";

/// The error of an embedding model whose cache directory could not be
/// determined because the home directory is unknown.
fn cache_dir_undetermined() -> EmbeddingError {
    EmbeddingError::ModelUnavailable {
        source: Box::new(ModelCacheDirUndetermined {
            source: MakePathAbsoluteError::HomeDirUnknown,
        }),
    }
}

#[tokio::test]
async fn a_dimension_unchecked_for_a_cache_directory_after_an_update_is_warned_about_with_the_fix()
{
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_status(|_, _, _| {
            Ok(status_with(DimensionResult::Unavailable(
                cache_dir_undetermined(),
            )))
        }));
    let progress = Arc::new(TestProgress::new());
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.update_all(update_all_input(&progress, false))).await;

    assert!(outcome.unwrap().status_error.is_none());
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].message, DIMENSION_UNCHECKED);
    assert_eq!(field(&logs[0], "fix"), CACHE_DIR_FIX);
}

#[tokio::test]
async fn a_dimension_unchecked_for_another_reason_is_warned_about_without_a_fix() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_status(|_, _, _| {
            Ok(status_with(DimensionResult::Unavailable(
                EmbeddingError::ModelUnavailable {
                    source: "Ollama is not running".into(),
                },
            )))
        }));
    let progress = Arc::new(TestProgress::new());
    let operations = doubles.operations();

    let (_, logs) = capture_async(operations.update_all(update_all_input(&progress, false))).await;

    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].message, DIMENSION_UNCHECKED);
    assert!(logs[0].fields.iter().all(|(name, _)| name != "fix"));
}

#[tokio::test]
async fn a_status_whose_dimension_is_unchecked_for_a_cache_directory_has_the_fix_in_its_warning() {
    let doubles = Doubles::new().index(TestIndex::new().on_status(|_, _, _| {
        Ok(status_with(DimensionResult::Unavailable(
            cache_dir_undetermined(),
        )))
    }));

    let status = doubles.operations().status().await.unwrap();

    assert_eq!(
        status.warnings,
        [format!(
            "{DIMENSION_UNCHECKED}: the embedding model cannot be used: the embedding model cache \
             directory cannot be determined: the home directory is unknown, so a leading `~` cannot \
             be expanded. {CACHE_DIR_FIX}"
        )]
    );
}

#[tokio::test]
async fn a_status_warning_whose_cause_ends_with_a_period_gets_no_second_one() {
    let doubles = Doubles::new().index(TestIndex::new().on_status(|_, _, _| {
        Ok(status_with(DimensionResult::Unavailable(
            EmbeddingError::ModelUnavailable {
                source: Box::new(ModelCacheDirUndetermined {
                    source: MakePathAbsoluteError::WorkingDirUnreadable(std::io::Error::other(
                        "the directory was removed.",
                    )),
                }),
            },
        )))
    }));

    let status = doubles.operations().status().await.unwrap();

    assert_eq!(
        status.warnings,
        [format!(
            "{DIMENSION_UNCHECKED}: the embedding model cannot be used: the embedding model cache \
             directory cannot be determined: the working directory cannot be read: the directory \
             was removed. {CACHE_DIR_FIX}"
        )]
    );
}

#[tokio::test]
async fn a_collection_list_that_cannot_be_read_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::new().on_list(|| {
        Err(CollectionError::CannotRead {
            source: "unreadable".into(),
        })
    }));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Collection {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: CollectionError::CannotRead { .. },
        }
    ));
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[test]
fn progress_without_a_total_is_passed_on_without_one() {
    let sink = TestProgress::new();
    let summed = SummedProgress::new(&sink, DocumentCounts { done: 4, total: 4 });

    summed.report(Progress {
        done: 2,
        total: None,
    });

    assert_eq!(
        sink.events(),
        [ProgressEvent::Report(Progress {
            done: 6,
            total: None
        })]
    );
    assert_eq!(summed.after(), DocumentCounts { done: 6, total: 6 });
}

#[test]
fn progress_reported_out_of_order_is_passed_on_as_it_comes_and_the_furthest_is_kept() {
    let sink = TestProgress::new();
    let summed = SummedProgress::new(&sink, DocumentCounts { done: 4, total: 4 });

    summed.report(Progress {
        done: 3,
        total: Some(5),
    });
    summed.report(Progress {
        done: 2,
        total: Some(5),
    });

    assert_eq!(
        sink.events(),
        [
            ProgressEvent::Report(Progress {
                done: 7,
                total: Some(9)
            }),
            ProgressEvent::Report(Progress {
                done: 6,
                total: Some(9)
            }),
        ]
    );
    assert_eq!(summed.after(), DocumentCounts { done: 7, total: 9 });
}

#[test]
fn embedded_chunks_are_passed_on_as_they_are() {
    let sink = TestProgress::new();
    let summed = SummedProgress::new(&sink, DocumentCounts { done: 4, total: 4 });

    summed.add_embedded_chunks(3);

    assert_eq!(sink.events(), [ProgressEvent::EmbeddedChunks(3)]);
}

/// A config whose `set` and `reset` change the setting they name from
/// `before` to `after`.
fn config_changing(before: serde_json::Value, after: serde_json::Value) -> TestConfig {
    TestConfig::new().on_change(move |call| {
        let name = match call {
            ConfigCall::Set { name, .. } | ConfigCall::Reset { name } => name,
            ConfigCall::Current => unreachable!("current is not a change"),
        };
        let name = match name.as_str() {
            "embedding.model" => SettingName::EmbeddingModel,
            _ => SettingName::GetCliLimitBytes,
        };
        Ok(SettingChange {
            name,
            before: before.clone(),
            after: after.clone(),
        })
    })
}

/// A config that changes the embedding model to `embeddinggemma`.
fn config_changing_the_model() -> TestConfig {
    config_changing(
        serde_json::json!("cl-nagoya/ruri-v3-30m"),
        serde_json::json!("embeddinggemma"),
    )
    .on_current(|| {
        Ok(Settings {
            embedding_model: "embeddinggemma".to_owned(),
            embedding_ollama_endpoint: "http://192.0.2.1:11434".to_owned(),
            ..TestConfig::default_settings()
        })
    })
}

fn set(name: &str, value: &str) -> ChangeSettingInput {
    ChangeSettingInput::Set {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

#[tokio::test]
async fn setting_a_setting_other_than_the_model_reads_no_status() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .config(config_changing(
            serde_json::json!(1024),
            serde_json::json!(2048),
        ))
        .write_lock(Arc::clone(&lock));

    let outcome = doubles
        .operations()
        .change_setting(set("get.cli_limit_bytes", "2048"))
        .await
        .unwrap();

    assert_eq!(
        outcome.change,
        SettingChange {
            name: SettingName::GetCliLimitBytes,
            before: serde_json::json!(1024),
            after: serde_json::json!(2048),
        }
    );
    assert!(outcome.index_status.is_none());
    assert_eq!(
        doubles.config.calls(),
        [ConfigCall::Set {
            name: "get.cli_limit_bytes".to_owned(),
            value: "2048".to_owned(),
        }]
    );
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(*doubles.created_embeddings.lock().unwrap(), []);
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn resetting_a_setting_calls_reset() {
    let doubles = Doubles::new().config(config_changing(
        serde_json::json!(2048),
        serde_json::json!(1024),
    ));

    doubles
        .operations()
        .change_setting(ChangeSettingInput::Reset {
            name: "get.cli_limit_bytes".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(
        doubles.config.calls(),
        [ConfigCall::Reset {
            name: "get.cli_limit_bytes".to_owned(),
        }]
    );
}

#[tokio::test]
async fn changing_the_model_reads_the_status_against_the_new_model() {
    let doubles = Doubles::new().config(config_changing_the_model());

    let outcome = doubles
        .operations()
        .change_setting(set("embedding.model", "embeddinggemma"))
        .await
        .unwrap();

    assert!(matches!(outcome.index_status, Some(Ok(_))));
    assert_eq!(
        doubles.config.calls(),
        [
            ConfigCall::Set {
                name: "embedding.model".to_owned(),
                value: "embeddinggemma".to_owned(),
            },
            ConfigCall::Current,
        ]
    );
    assert_eq!(
        *doubles.created_embeddings.lock().unwrap(),
        [CreatedEmbedding {
            model: "embeddinggemma".to_owned(),
            ollama_endpoint: "http://192.0.2.1:11434".to_owned(),
            hf_token: Some("hf-token".to_owned()),
            model_cache_dir: ReturnedCacheDir::Dir("/home/user/.mdya-models".into()),
        }]
    );
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::Status {
            scope: StatusScope::All,
            compared_model_name: "embeddinggemma".to_owned(),
            check: DimensionCheck::OnlyIfSameName,
        }]
    );
}

#[tokio::test]
async fn changing_the_model_passes_on_the_cache_directory_error_as_its_own_type() {
    let doubles = Doubles::new()
        .config(config_changing_the_model())
        .model_cache_dir(|| {
            Err(ModelCacheDirUndetermined {
                source: MakePathAbsoluteError::HomeDirUnknown,
            })
        });

    doubles
        .operations()
        .change_setting(set("embedding.model", "embeddinggemma"))
        .await
        .unwrap();

    let created = doubles.created_embeddings.lock().unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].model_cache_dir, ReturnedCacheDir::Undetermined);
}

#[tokio::test]
async fn a_status_that_cannot_be_read_after_changing_the_model_is_returned_without_a_log() {
    let doubles = Doubles::new()
        .config(config_changing_the_model())
        .index(TestIndex::new().on_status(|_, _, _| Err(IndexError::RebuildIncomplete)));
    let operations = doubles.operations();

    let (outcome, logs) =
        capture_async(operations.change_setting(set("embedding.model", "embeddinggemma"))).await;

    assert!(matches!(
        outcome.unwrap().index_status,
        Some(Err(IndexError::RebuildIncomplete))
    ));
    assert_eq!(logs, []);
}

#[tokio::test]
async fn a_refused_change_returns_the_config_error() {
    let doubles = Doubles::new().config(TestConfig::new().on_change(|_| {
        Err(ConfigError::UnknownName {
            name: "nothing".to_owned(),
        })
    }));

    let error = doubles
        .operations()
        .change_setting(set("nothing", "1"))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Config(ConfigError::UnknownName { .. })
    ));
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn settings_that_cannot_be_read_after_changing_the_model_end_with_the_config_error() {
    let doubles = Doubles::new().config(config_changing_the_model().on_current(|| {
        Err(ConfigError::CannotRead {
            source: "broken".into(),
        })
    }));

    let error = doubles
        .operations()
        .change_setting(set("embedding.model", "embeddinggemma"))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Config(ConfigError::CannotRead { .. })
    ));
    assert_eq!(*doubles.created_embeddings.lock().unwrap(), []);
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn changing_a_setting_while_another_write_is_in_progress_changes_nothing() {
    let doubles = Doubles::new()
        .config(config_changing(
            serde_json::json!(1024),
            serde_json::json!(2048),
        ))
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));

    let error = doubles
        .operations()
        .change_setting(set("get.cli_limit_bytes", "2048"))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.config.calls(), []);
}

#[tokio::test]
async fn resetting_the_model_reads_the_status_against_the_model_it_returns_to() {
    let doubles = Doubles::new().config(config_changing_the_model());

    let outcome = doubles
        .operations()
        .change_setting(ChangeSettingInput::Reset {
            name: "embedding.model".to_owned(),
        })
        .await
        .unwrap();

    assert!(matches!(outcome.index_status, Some(Ok(_))));
    assert_eq!(
        doubles.config.calls(),
        [
            ConfigCall::Reset {
                name: "embedding.model".to_owned(),
            },
            ConfigCall::Current,
        ]
    );
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::Status {
            scope: StatusScope::All,
            compared_model_name: "embeddinggemma".to_owned(),
            check: DimensionCheck::OnlyIfSameName,
        }]
    );
}

fn update_input(collection: &str, progress: &Arc<TestProgress>) -> UpdateInput {
    UpdateInput {
        collection: name(collection),
        overwrite: true,
        progress: progress.clone(),
        optimize: true,
    }
}

#[tokio::test]
async fn updating_a_collection_updates_it_reads_its_status_then_maintains() {
    let lock = Arc::new(TestWriteLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![archives(), notes()]))
        .ingest(directory(), directory_ingest(|_| Ok(report(3, 1))))
        .index(index_seeing_lock(&lock, &seen))
        .write_lock(Arc::clone(&lock));
    let progress = Arc::new(TestProgress::new());

    let outcome = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap();

    assert_eq!(outcome.counts, report(3, 1));
    assert!(outcome.unreadable_sources.is_empty());
    assert!(outcome.status_error.is_none());
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("notes"))]
    );
    assert_eq!(
        doubles.ingests[&directory()].calls(),
        [IngestCall::Update(instruction("notes"))]
    );
    assert_eq!(
        doubles.index.calls(),
        [
            IndexCall::CheckIngestable,
            IndexCall::Status {
                scope: StatusScope::Collections(vec![name("notes")]),
                compared_model_name: "all-minilm".to_owned(),
                check: DimensionCheck::OnlyIfSameName,
            },
            IndexCall::Maintain,
        ]
    );
    assert_eq!(
        progress.events(),
        [
            ProgressEvent::Report(Progress {
                done: 3,
                total: Some(3)
            }),
            ProgressEvent::MaintenanceStarted,
        ]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [LockState {
            acquired: 1,
            held: true
        }]
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

#[tokio::test]
async fn a_collection_whose_source_cannot_be_read_is_skipped_and_still_maintained() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .ingest(
            directory(),
            directory_ingest(|_| {
                Err(UpdateStopped {
                    counts: UpdateReport::default(),
                    error: IngestError::SourceUnreadable {
                        source: "missing".into(),
                    },
                })
            }),
        );
    let progress = Arc::new(TestProgress::new());

    let outcome = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap();

    assert_eq!(outcome.counts, UpdateReport::default());
    assert_eq!(outcome.unreadable_sources, [name("notes")]);
    assert_eq!(outcome.anomalies(), [OutcomeAnomaly::SourcesUnreadable]);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn updating_a_missing_collection_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new();
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::CollectionNotFound { collection } if collection == name("notes")
    ));
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

#[tokio::test]
async fn a_collection_without_update_is_not_updated_and_still_maintained() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![memos()]));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("memos", &progress))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::UpdateNotSupported { collection } if collection == name("memos")
    ));
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn an_update_that_stops_returns_its_counts_reads_no_status_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .ingest(
            directory(),
            directory_ingest(|_| {
                Err(UpdateStopped {
                    counts: report(1, 0),
                    error: IngestError::Index(IndexError::RebuildIncomplete),
                })
            }),
        );
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::UpdateStopped {
            counts,
            context: ErrorContext { collection: Some(collection), key: None },
            source: IngestError::Index(IndexError::RebuildIncomplete),
        } if counts == report(1, 0) && collection == name("notes")
    ));
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[tokio::test]
async fn an_index_that_cannot_ingest_updates_no_collection_and_still_maintains() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .index(TestIndex::new().on_check_ingestable(|| Err(IndexError::RebuildIncomplete)));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Index {
            context: ErrorContext {
                collection: None,
                key: None
            },
            source: IndexError::RebuildIncomplete,
        }
    ));
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[tokio::test]
async fn updating_a_collection_while_another_write_is_in_progress_reads_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .write_lock(Arc::new(
            TestWriteLock::new().on_try_acquire(|| Err(WriteLockError::InProgress)),
        ));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("notes", &progress))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        OperationError::WriteLock(WriteLockError::InProgress)
    ));
    assert_eq!(doubles.collections.calls(), []);
    assert_eq!(doubles.index.calls(), []);
    assert_eq!(progress.events(), []);
}

/// Checks that the warning gives `IndexError::RebuildIncomplete` as its cause.
fn assert_rebuild_incomplete_is_the_cause(log: &LogRecord) {
    assert!(log.fields.contains(&(
        "cause".to_owned(),
        IndexError::RebuildIncomplete.to_string()
    )));
}

#[tokio::test]
async fn an_operation_that_panics_does_not_maintain_and_releases_the_lock() {
    let lock = Arc::new(TestWriteLock::new());
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![notes()]))
        .ingest(
            directory(),
            TestIngest::new().on_update(|_, _| panic!("the update panicked")),
        )
        .write_lock(Arc::clone(&lock));
    let progress = Arc::new(TestProgress::new());
    let operations = Arc::new(doubles.operations());
    let input = update_all_input(&progress, true);

    let joined = tokio::spawn(async move { operations.update_all(input).await }).await;

    assert!(joined.unwrap_err().is_panic());
    assert!(!doubles.index.calls().contains(&IndexCall::Maintain));
    assert!(
        !progress
            .events()
            .contains(&ProgressEvent::MaintenanceStarted)
    );
    assert_eq!(
        lock.state(),
        LockState {
            acquired: 1,
            held: false
        }
    );
}

/// A collection whose type no ingest implementation is given for, as one
/// another version of mdya wrote.
fn unknown() -> CollectionInfo {
    CollectionInfo {
        name: name("unknown"),
        collection_type: CollectionType("future".to_owned()),
        ..notes()
    }
}

#[tokio::test]
async fn updating_all_with_a_collection_of_an_unknown_type_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![notes(), unknown()]));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_all(update_all_input(&progress, true))
        .await
        .unwrap_err();

    assert_unknown_type(&error);
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn deleting_a_document_of_a_collection_of_an_unknown_type_deletes_nothing() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![unknown()]))
        .index(TestIndex::new().on_has_document(|_, _| Ok(true)));
    let confirmation = TestConfirmation::answering(true);

    let error = doubles
        .operations()
        .delete_document(delete_a_md("unknown", &confirmation))
        .await
        .unwrap_err();

    assert_unknown_type(&error);
    assert_eq!(confirmation.asked(), 0);
    assert_eq!(doubles.index.calls(), [IndexCall::Maintain]);
}

/// Checks that `error` is the error of an unknown collection type for the
/// collection `unknown`.
fn assert_unknown_type(error: &OperationError) {
    assert!(matches!(
        error,
        OperationError::UnknownCollectionType {
            collection,
            collection_type,
        } if *collection == name("unknown") && collection_type.0 == "future"
    ));
}

#[tokio::test]
async fn putting_into_a_collection_of_an_unknown_type_reads_nothing_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![unknown()]));
    let content = TestContentReader::returning(b"# A");

    let error = doubles
        .operations()
        .put_document(put_into("unknown", &content))
        .await
        .unwrap_err();

    assert_unknown_type(&error);
    assert_eq!(content.reads(), 0);
    assert_eq!(doubles.ingests[&raw()].calls(), []);
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
}

#[tokio::test]
async fn updating_a_collection_of_an_unknown_type_updates_nothing_and_still_maintains() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![unknown()]));
    let progress = Arc::new(TestProgress::new());

    let error = doubles
        .operations()
        .update_collection(update_input("unknown", &progress))
        .await
        .unwrap_err();

    assert_unknown_type(&error);
    assert_eq!(doubles.ingests[&directory()].calls(), []);
    assert_eq!(
        doubles.index.calls(),
        [IndexCall::CheckIngestable, IndexCall::Maintain]
    );
    assert_eq!(progress.events(), [ProgressEvent::MaintenanceStarted]);
}

#[tokio::test]
async fn changing_the_attributes_of_a_collection_of_an_unknown_type_changes_nothing() {
    let doubles = Doubles::new().collections(TestCollections::with(vec![unknown()]));

    let error = doubles
        .operations()
        .change_collection(ChangeCollectionInput {
            name: name("unknown"),
            change: CollectionChange {
                description: None,
                attributes: Some(TypeAttributes::default()),
            },
        })
        .await
        .unwrap_err();

    assert_unknown_type(&error);
    assert_eq!(
        doubles.collections.calls(),
        [CollectionsCall::Get(name("unknown"))]
    );
    assert_eq!(doubles.index.calls(), []);
}

#[tokio::test]
async fn the_description_of_a_collection_of_an_unknown_type_can_be_changed() {
    let doubles = Doubles::new()
        .collections(TestCollections::with(vec![unknown()]).on_change(|_, _| Ok(unknown())));
    let change = CollectionChange {
        description: Some(None),
        attributes: None,
    };

    doubles
        .operations()
        .change_collection(ChangeCollectionInput {
            name: name("unknown"),
            change: change.clone(),
        })
        .await
        .unwrap();

    assert_eq!(
        doubles.collections.calls(),
        [
            CollectionsCall::Get(name("unknown")),
            CollectionsCall::Change(name("unknown"), change),
        ]
    );
}
