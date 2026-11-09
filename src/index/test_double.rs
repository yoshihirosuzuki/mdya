//! A test double for [`Index`], for the unit tests of the components that use
//! it.

// Parts of the double are for tests that the components using it have not
// written yet.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use super::{
    DimensionCheck, DimensionResult, DocumentToRegister, Index, IndexError, IndexStatus,
    PartialDelete, QueryTables, RegistrationSink, StatusScope,
};
use crate::embedding::Embedding;
use crate::types::{CollectionName, DocumentOutcome, Key, ProgressSink};

type RegisterFn = dyn Fn(&DocumentToRegister, Option<&dyn ProgressSink>) -> Result<DocumentOutcome, IndexError>
    + Send
    + Sync;
type DeleteOthersFn =
    dyn Fn(&CollectionName, &HashSet<Key>, &[String]) -> Result<u64, PartialDelete> + Send + Sync;
type DocumentFn<T> = dyn Fn(&CollectionName, &Key) -> Result<T, IndexError> + Send + Sync;
type DocumentCountsFn = dyn Fn() -> Result<BTreeMap<CollectionName, u64>, IndexError> + Send + Sync;
type StatusFn = dyn Fn(&StatusScope, &dyn Embedding, DimensionCheck) -> Result<IndexStatus, IndexError>
    + Send
    + Sync;
type RebuildFn = dyn Fn(bool, usize, &dyn ProgressSink) -> Result<u64, IndexError> + Send + Sync;
type TablesFn = dyn Fn() -> Result<Option<QueryTables>, IndexError> + Send + Sync;
type UnitFn = dyn Fn() -> Result<(), IndexError> + Send + Sync;

/// Answers every function from a handler and records every call. The default
/// handlers stand for an empty index that accepts everything: each document
/// of `register_stream` is `Added`, the deletes delete nothing, `has_document`
/// is false, the counts and the status are empty, `tables_for_query` is
/// `None`, and `maintain` and `check_ingestable` succeed. Replace a handler
/// with the `on_*` function of the same name. `on_register` answers for each
/// document of the stream, and `register_stream` records each as one
/// `IndexCall::Register`, one at a time, stopping at the first error.
pub(crate) struct TestIndex {
    register: Box<RegisterFn>,
    delete_others: Box<DeleteOthersFn>,
    delete_document: Box<DocumentFn<()>>,
    has_document: Box<DocumentFn<bool>>,
    document_counts: Box<DocumentCountsFn>,
    status: Box<StatusFn>,
    rebuild_embeddings: Box<RebuildFn>,
    tables_for_query: Box<TablesFn>,
    maintain: Box<UnitFn>,
    check_ingestable: Box<UnitFn>,
    calls: Mutex<Vec<IndexCall>>,
}

/// A call the double received, with its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IndexCall {
    Register(DocumentToRegister),
    DeleteOthers {
        collection: CollectionName,
        keep: HashSet<Key>,
        unreadable_dirs: Vec<String>,
    },
    DeleteDocument(CollectionName, Key),
    HasDocument(CollectionName, Key),
    DocumentCounts,
    Status {
        scope: StatusScope,
        compared_model_name: String,
        check: DimensionCheck,
    },
    RebuildEmbeddings {
        include_configured_model: bool,
        parallelism: usize,
    },
    TablesForQuery,
    Maintain,
    CheckIngestable,
}

impl TestIndex {
    pub(crate) fn new() -> Self {
        Self {
            register: Box::new(|_, _| Ok(DocumentOutcome::Added)),
            delete_others: Box::new(|_, _, _| Ok(0)),
            delete_document: Box::new(|_, _| Ok(())),
            has_document: Box::new(|_, _| Ok(false)),
            document_counts: Box::new(|| Ok(BTreeMap::new())),
            status: Box::new(|_, compare_with, _| Ok(empty_status(compare_with))),
            rebuild_embeddings: Box::new(|_, _, _| Ok(0)),
            tables_for_query: Box::new(|| Ok(None)),
            maintain: Box::new(|| Ok(())),
            check_ingestable: Box::new(|| Ok(())),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn on_register(
        mut self,
        handler: impl Fn(
            &DocumentToRegister,
            Option<&dyn ProgressSink>,
        ) -> Result<DocumentOutcome, IndexError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.register = Box::new(handler);
        self
    }

    pub(crate) fn on_delete_others(
        mut self,
        handler: impl Fn(&CollectionName, &HashSet<Key>, &[String]) -> Result<u64, PartialDelete>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.delete_others = Box::new(handler);
        self
    }

    pub(crate) fn on_delete_document(
        mut self,
        handler: impl Fn(&CollectionName, &Key) -> Result<(), IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.delete_document = Box::new(handler);
        self
    }

    pub(crate) fn on_has_document(
        mut self,
        handler: impl Fn(&CollectionName, &Key) -> Result<bool, IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.has_document = Box::new(handler);
        self
    }

    pub(crate) fn on_document_counts(
        mut self,
        handler: impl Fn() -> Result<BTreeMap<CollectionName, u64>, IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.document_counts = Box::new(handler);
        self
    }

    pub(crate) fn on_status(
        mut self,
        handler: impl Fn(
            &StatusScope,
            &dyn Embedding,
            DimensionCheck,
        ) -> Result<IndexStatus, IndexError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.status = Box::new(handler);
        self
    }

    pub(crate) fn on_rebuild_embeddings(
        mut self,
        handler: impl Fn(bool, usize, &dyn ProgressSink) -> Result<u64, IndexError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.rebuild_embeddings = Box::new(handler);
        self
    }

    pub(crate) fn on_tables_for_query(
        mut self,
        handler: impl Fn() -> Result<Option<QueryTables>, IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.tables_for_query = Box::new(handler);
        self
    }

    pub(crate) fn on_maintain(
        mut self,
        handler: impl Fn() -> Result<(), IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.maintain = Box::new(handler);
        self
    }

    pub(crate) fn on_check_ingestable(
        mut self,
        handler: impl Fn() -> Result<(), IndexError> + Send + Sync + 'static,
    ) -> Self {
        self.check_ingestable = Box::new(handler);
        self
    }

    /// Every call so far, in order.
    pub(crate) fn calls(&self) -> Vec<IndexCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: IndexCall) {
        self.calls.lock().unwrap().push(call);
    }
}

/// The status of an empty index, compared with `compare_with`'s model, without
/// checking its dimension.
pub(crate) fn empty_status(compare_with: &dyn Embedding) -> IndexStatus {
    IndexStatus {
        compared_model_name: compare_with.model_name().to_owned(),
        compared_dimension: DimensionResult::NotChecked,
        models: Vec::new(),
        collections: BTreeMap::new(),
        document_count: 0,
        chunk_count: 0,
        chunks_without_vector: 0,
    }
}

#[async_trait]
impl Index for TestIndex {
    async fn register_stream(
        &self,
        mut documents: BoxStream<'_, DocumentToRegister>,
        _parallelism: usize,
        results: &dyn RegistrationSink,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        while let Some(document) = documents.next().await {
            let result = (self.register)(&document, progress);
            let (collection, key) = (document.collection.clone(), document.key.clone());
            self.record(IndexCall::Register(document));
            results.registered(&collection, &key, result?);
        }
        Ok(())
    }

    async fn delete_others(
        &self,
        collection: &CollectionName,
        keep: &HashSet<Key>,
        unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete> {
        self.record(IndexCall::DeleteOthers {
            collection: collection.clone(),
            keep: keep.clone(),
            unreadable_dirs: unreadable_dirs.to_vec(),
        });
        (self.delete_others)(collection, keep, unreadable_dirs)
    }

    async fn delete_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<(), IndexError> {
        self.record(IndexCall::DeleteDocument(collection.clone(), key.clone()));
        (self.delete_document)(collection, key)
    }

    async fn has_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<bool, IndexError> {
        self.record(IndexCall::HasDocument(collection.clone(), key.clone()));
        (self.has_document)(collection, key)
    }

    async fn document_counts(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        self.record(IndexCall::DocumentCounts);
        (self.document_counts)()
    }

    async fn status(
        &self,
        scope: StatusScope,
        compare_with: &dyn Embedding,
        check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError> {
        self.record(IndexCall::Status {
            scope: scope.clone(),
            compared_model_name: compare_with.model_name().to_owned(),
            check,
        });
        (self.status)(&scope, compare_with, check)
    }

    async fn rebuild_embeddings(
        &self,
        include_configured_model: bool,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        self.record(IndexCall::RebuildEmbeddings {
            include_configured_model,
            parallelism,
        });
        (self.rebuild_embeddings)(include_configured_model, parallelism, progress)
    }

    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError> {
        self.record(IndexCall::TablesForQuery);
        (self.tables_for_query)()
    }

    async fn maintain(&self) -> Result<(), IndexError> {
        self.record(IndexCall::Maintain);
        (self.maintain)()
    }

    async fn check_ingestable(&self) -> Result<(), IndexError> {
        self.record(IndexCall::CheckIngestable);
        (self.check_ingestable)()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::embedding::test_double::TestEmbedding;
    use crate::types::Chunk;

    fn document(key: &str) -> DocumentToRegister {
        DocumentToRegister {
            collection: CollectionName("notes".to_owned()),
            key: Key(key.to_owned()),
            time: None,
            body: "body".to_owned(),
            chunks: vec![Chunk { range: 0..4 }],
            overwrite: false,
        }
    }

    /// Records the keys and outcomes passed to it, in order.
    #[derive(Default)]
    struct Outcomes(Mutex<Vec<(String, DocumentOutcome)>>);

    impl RegistrationSink for Outcomes {
        fn registered(&self, _collection: &CollectionName, key: &Key, outcome: DocumentOutcome) {
            self.0.lock().unwrap().push((key.0.clone(), outcome));
        }
    }

    async fn register_one(
        index: &Arc<dyn Index>,
        document: DocumentToRegister,
    ) -> Result<DocumentOutcome, IndexError> {
        let outcomes = Outcomes::default();
        let documents = futures::stream::iter([document]).boxed();
        index.register_stream(documents, 1, &outcomes, None).await?;
        let outcome = outcomes.0.lock().unwrap()[0].1;
        Ok(outcome)
    }

    #[tokio::test]
    async fn answers_as_an_empty_index_by_default_and_records_the_calls() {
        let double = Arc::new(TestIndex::new());
        let index: Arc<dyn Index> = double.clone();
        let embedding = TestEmbedding::new("test-model", 4);
        let notes = CollectionName("notes".to_owned());

        let outcome = register_one(&index, document("a.md")).await.unwrap();
        let deleted = index
            .delete_others(&notes, &HashSet::new(), &[])
            .await
            .unwrap();
        let status = index
            .status(StatusScope::All, &embedding, DimensionCheck::Never)
            .await
            .unwrap();

        assert_eq!(outcome, DocumentOutcome::Added);
        assert_eq!(deleted, 0);
        assert_eq!(status.compared_model_name, "test-model");
        assert!(index.tables_for_query().await.unwrap().is_none());
        assert_eq!(
            double.calls(),
            vec![
                IndexCall::Register(document("a.md")),
                IndexCall::DeleteOthers {
                    collection: notes,
                    keep: HashSet::new(),
                    unreadable_dirs: Vec::new(),
                },
                IndexCall::Status {
                    scope: StatusScope::All,
                    compared_model_name: "test-model".to_owned(),
                    check: DimensionCheck::Never,
                },
                IndexCall::TablesForQuery,
            ]
        );
    }

    #[tokio::test]
    async fn answers_from_the_replaced_handlers() {
        let index: Arc<dyn Index> = Arc::new(
            TestIndex::new()
                .on_register(|document, _| match document.key.0.as_str() {
                    "same.md" => Ok(DocumentOutcome::Unchanged),
                    _ => Err(IndexError::CannotWrite {
                        source: "disk full".into(),
                    }),
                })
                .on_delete_others(|_, _, _| {
                    Err(PartialDelete {
                        deleted: 2,
                        error: IndexError::CannotRead {
                            source: "broken".into(),
                        },
                    })
                }),
        );
        let notes = CollectionName("notes".to_owned());

        assert_eq!(
            register_one(&index, document("same.md")).await.unwrap(),
            DocumentOutcome::Unchanged
        );
        assert!(matches!(
            register_one(&index, document("other.md")).await,
            Err(IndexError::CannotWrite { .. })
        ));
        let stopped = index
            .delete_others(&notes, &HashSet::new(), &[])
            .await
            .unwrap_err();
        assert_eq!(stopped.deleted, 2);
    }

    #[tokio::test]
    async fn registers_a_stream_one_document_at_a_time_until_one_fails() {
        let double =
            Arc::new(
                TestIndex::new().on_register(|document, _| match document.key.0.as_str() {
                    "same.md" => Ok(DocumentOutcome::Unchanged),
                    "bad.md" => Err(IndexError::CannotWrite {
                        source: "disk full".into(),
                    }),
                    _ => Ok(DocumentOutcome::Added),
                }),
            );
        let index: Arc<dyn Index> = double.clone();
        let keys = ["a.md", "same.md", "bad.md", "c.md"];
        let documents = futures::stream::iter(keys.map(document)).boxed();
        let outcomes = Outcomes::default();

        let stopped = index.register_stream(documents, 4, &outcomes, None).await;

        assert!(matches!(stopped, Err(IndexError::CannotWrite { .. })));
        assert_eq!(
            *outcomes.0.lock().unwrap(),
            [
                ("a.md".to_owned(), DocumentOutcome::Added),
                ("same.md".to_owned(), DocumentOutcome::Unchanged),
            ]
        );
        let registered =
            ["a.md", "same.md", "bad.md"].map(|key| IndexCall::Register(document(key)));
        assert_eq!(double.calls(), registered);
    }
}
