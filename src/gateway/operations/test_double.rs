//! A test double for [`Operations`], for the tests of `cli` and `mcp`, and
//! test doubles for the traits that `cli` hands to the operations.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]
#![expect(
    clippy::result_large_err,
    reason = "the handlers return the operations' error, as the trait does"
)]

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use futures::future::BoxFuture;

use crate::collection::CollectionInfo;
use crate::types::{Progress, ProgressSink};

use super::{
    ChangeCollectionInput, ChangeSettingInput, CollectionList, Confirmation, ContentReader,
    CreateCollectionInput, DeleteCollectionInput, DeleteCollectionOutcome, DeleteDocumentInput,
    DeleteDocumentOutcome, DocumentText, GatewayProgressSink, GetDocumentInput, OperationError,
    Operations, OptimizeInput, PutInput, PutOutcome, RebuildInput, RebuildOutcome, SearchInput,
    SearchOutput, SettingChangeOutcome, SettingsOutput, StatusOutput, UpdateAllInput, UpdateInput,
    UpdateOutcome,
};

/// Answers an operation. Asynchronous, so that a handler can call what the
/// input holds (the confirmation, the content reader) during the operation,
/// as the real operations do.
type Handler<I, O> =
    Box<dyn for<'a> Fn(&'a I) -> BoxFuture<'a, Result<O, OperationError>> + Send + Sync>;

/// Answers each operation from a handler set with the `on_*` function of the
/// same name, and keeps the inputs it received. An operation without a
/// handler panics, so that a test sets a handler for every operation it
/// expects. The operations that hold a confirmation or a content reader also
/// take an asynchronous handler, set with `on_*_async`.
#[derive(Default)]
pub(crate) struct TestOperations {
    create_collection: Option<Handler<CreateCollectionInput, CollectionInfo>>,
    delete_collection: Option<Handler<DeleteCollectionInput, DeleteCollectionOutcome>>,
    list_collections: Option<Handler<(), CollectionList>>,
    change_collection: Option<Handler<ChangeCollectionInput, CollectionInfo>>,
    update_collections: Option<Handler<UpdateInput, UpdateOutcome>>,
    update_all: Option<Handler<UpdateAllInput, UpdateOutcome>>,
    put_document: Option<Handler<PutInput, PutOutcome>>,
    delete_document: Option<Handler<DeleteDocumentInput, DeleteDocumentOutcome>>,
    search: Option<Handler<SearchInput, SearchOutput>>,
    get_document: Option<Handler<GetDocumentInput, DocumentText>>,
    status: Option<Handler<(), StatusOutput>>,
    list_settings: Option<Handler<(), SettingsOutput>>,
    change_setting: Option<Handler<ChangeSettingInput, SettingChangeOutcome>>,
    rebuild_embeddings: Option<Handler<RebuildInput, RebuildOutcome>>,
    optimize: Option<Handler<OptimizeInput, ()>>,
    received: Mutex<Received>,
}

/// The inputs the double received, in order, for each operation. An
/// operation without input counts its calls.
#[derive(Default)]
pub(crate) struct Received {
    pub(crate) create_collection: Vec<CreateCollectionInput>,
    pub(crate) delete_collection: Vec<DeleteCollectionInput>,
    pub(crate) list_collections: Vec<()>,
    pub(crate) change_collection: Vec<ChangeCollectionInput>,
    pub(crate) update_collections: Vec<UpdateInput>,
    pub(crate) update_all: Vec<UpdateAllInput>,
    pub(crate) put_document: Vec<PutInput>,
    pub(crate) delete_document: Vec<DeleteDocumentInput>,
    pub(crate) search: Vec<SearchInput>,
    pub(crate) get_document: Vec<GetDocumentInput>,
    pub(crate) status: Vec<()>,
    pub(crate) list_settings: Vec<()>,
    pub(crate) change_setting: Vec<ChangeSettingInput>,
    pub(crate) rebuild_embeddings: Vec<RebuildInput>,
    pub(crate) optimize: Vec<OptimizeInput>,
}

macro_rules! setter {
    ($setter:ident, $field:ident, $input:ty, $output:ty) => {
        pub(crate) fn $setter(
            mut self,
            f: impl Fn(&$input) -> Result<$output, OperationError> + Send + Sync + 'static,
        ) -> Self {
            self.$field = Some(Box::new(move |input| {
                Box::pin(std::future::ready(f(input)))
            }));
            self
        }
    };
}

macro_rules! async_setter {
    ($setter:ident, $field:ident, $input:ty, $output:ty) => {
        pub(crate) fn $setter(
            mut self,
            f: impl for<'a> Fn(&'a $input) -> BoxFuture<'a, Result<$output, OperationError>>
            + Send
            + Sync
            + 'static,
        ) -> Self {
            self.$field = Some(Box::new(f));
            self
        }
    };
}

impl TestOperations {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    setter!(
        on_create_collection,
        create_collection,
        CreateCollectionInput,
        CollectionInfo
    );
    setter!(
        on_delete_collection,
        delete_collection,
        DeleteCollectionInput,
        DeleteCollectionOutcome
    );
    setter!(on_list_collections, list_collections, (), CollectionList);
    setter!(
        on_change_collection,
        change_collection,
        ChangeCollectionInput,
        CollectionInfo
    );
    setter!(
        on_update_collections,
        update_collections,
        UpdateInput,
        UpdateOutcome
    );
    setter!(on_update_all, update_all, UpdateAllInput, UpdateOutcome);
    setter!(on_put_document, put_document, PutInput, PutOutcome);
    setter!(
        on_delete_document,
        delete_document,
        DeleteDocumentInput,
        DeleteDocumentOutcome
    );
    setter!(on_search, search, SearchInput, SearchOutput);
    setter!(
        on_get_document,
        get_document,
        GetDocumentInput,
        DocumentText
    );
    setter!(on_status, status, (), StatusOutput);
    setter!(on_list_settings, list_settings, (), SettingsOutput);
    setter!(
        on_change_setting,
        change_setting,
        ChangeSettingInput,
        SettingChangeOutcome
    );
    setter!(
        on_rebuild_embeddings,
        rebuild_embeddings,
        RebuildInput,
        RebuildOutcome
    );
    setter!(on_optimize, optimize, OptimizeInput, ());
    async_setter!(
        on_delete_collection_async,
        delete_collection,
        DeleteCollectionInput,
        DeleteCollectionOutcome
    );
    async_setter!(on_put_document_async, put_document, PutInput, PutOutcome);
    async_setter!(
        on_delete_document_async,
        delete_document,
        DeleteDocumentInput,
        DeleteDocumentOutcome
    );

    /// The inputs received so far. An input is kept only after its handler
    /// returns, so it is not here while an asynchronous handler is waiting,
    /// nor when the operation's future is dropped before that. Drop the guard
    /// before calling the double again: an operation waits for it.
    pub(crate) fn received(&self) -> MutexGuard<'_, Received> {
        self.received.lock().unwrap()
    }

    /// Answers `input` with the handler, then keeps `input` with `keep`.
    async fn answer<I, O>(
        &self,
        handler: &Option<Handler<I, O>>,
        operation: &str,
        input: I,
        keep: impl FnOnce(&mut Received, I),
    ) -> Result<O, OperationError> {
        let Some(handler) = handler else {
            panic!("TestOperations has no handler for {operation}; set one with on_{operation}");
        };
        let answer = handler(&input).await;
        keep(&mut self.received.lock().unwrap(), input);
        answer
    }
}

#[async_trait]
impl Operations for TestOperations {
    async fn create_collection(
        &self,
        input: CreateCollectionInput,
    ) -> Result<CollectionInfo, OperationError> {
        self.answer(
            &self.create_collection,
            "create_collection",
            input,
            |r, i| r.create_collection.push(i),
        )
        .await
    }

    async fn delete_collection(
        &self,
        input: DeleteCollectionInput,
    ) -> Result<DeleteCollectionOutcome, OperationError> {
        self.answer(
            &self.delete_collection,
            "delete_collection",
            input,
            |r, i| r.delete_collection.push(i),
        )
        .await
    }

    async fn list_collections(&self) -> Result<CollectionList, OperationError> {
        self.answer(&self.list_collections, "list_collections", (), |r, i| {
            r.list_collections.push(i)
        })
        .await
    }

    async fn change_collection(
        &self,
        input: ChangeCollectionInput,
    ) -> Result<CollectionInfo, OperationError> {
        self.answer(
            &self.change_collection,
            "change_collection",
            input,
            |r, i| r.change_collection.push(i),
        )
        .await
    }

    async fn update_collections(
        &self,
        input: UpdateInput,
    ) -> Result<UpdateOutcome, OperationError> {
        self.answer(
            &self.update_collections,
            "update_collections",
            input,
            |r, i| r.update_collections.push(i),
        )
        .await
    }

    async fn update_all(&self, input: UpdateAllInput) -> Result<UpdateOutcome, OperationError> {
        self.answer(&self.update_all, "update_all", input, |r, i| {
            r.update_all.push(i)
        })
        .await
    }

    async fn put_document(&self, input: PutInput) -> Result<PutOutcome, OperationError> {
        self.answer(&self.put_document, "put_document", input, |r, i| {
            r.put_document.push(i)
        })
        .await
    }

    async fn delete_document(
        &self,
        input: DeleteDocumentInput,
    ) -> Result<DeleteDocumentOutcome, OperationError> {
        self.answer(&self.delete_document, "delete_document", input, |r, i| {
            r.delete_document.push(i)
        })
        .await
    }

    async fn search(&self, input: SearchInput) -> Result<SearchOutput, OperationError> {
        self.answer(&self.search, "search", input, |r, i| r.search.push(i))
            .await
    }

    async fn get_document(&self, input: GetDocumentInput) -> Result<DocumentText, OperationError> {
        self.answer(&self.get_document, "get_document", input, |r, i| {
            r.get_document.push(i)
        })
        .await
    }

    async fn status(&self) -> Result<StatusOutput, OperationError> {
        self.answer(&self.status, "status", (), |r, i| r.status.push(i))
            .await
    }

    async fn list_settings(&self) -> Result<SettingsOutput, OperationError> {
        self.answer(&self.list_settings, "list_settings", (), |r, i| {
            r.list_settings.push(i)
        })
        .await
    }

    async fn change_setting(
        &self,
        input: ChangeSettingInput,
    ) -> Result<SettingChangeOutcome, OperationError> {
        self.answer(&self.change_setting, "change_setting", input, |r, i| {
            r.change_setting.push(i)
        })
        .await
    }

    async fn rebuild_embeddings(
        &self,
        input: RebuildInput,
    ) -> Result<RebuildOutcome, OperationError> {
        self.answer(
            &self.rebuild_embeddings,
            "rebuild_embeddings",
            input,
            |r, i| r.rebuild_embeddings.push(i),
        )
        .await
    }

    async fn optimize(&self, input: OptimizeInput) -> Result<(), OperationError> {
        self.answer(&self.optimize, "optimize", input, |r, i| r.optimize.push(i))
            .await
    }
}

/// A confirmation that gives a fixed answer and counts how often it was asked.
/// Clones share the count, so that a test can keep one and put another in
/// the input.
#[derive(Clone)]
pub(crate) struct TestConfirmation {
    answer: bool,
    asked: Arc<Mutex<u32>>,
}

impl TestConfirmation {
    pub(crate) fn answering(answer: bool) -> Self {
        Self {
            answer,
            asked: Arc::default(),
        }
    }

    pub(crate) fn asked(&self) -> u32 {
        *self.asked.lock().unwrap()
    }
}

#[async_trait]
impl Confirmation for TestConfirmation {
    async fn confirm(&self) -> bool {
        *self.asked.lock().unwrap() += 1;
        self.answer
    }
}

/// A content reader that returns fixed bytes, or fails with the given kind,
/// and counts how often it was read. Clones share the count.
#[derive(Clone)]
pub(crate) struct TestContentReader {
    content: Result<Vec<u8>, std::io::ErrorKind>,
    reads: Arc<Mutex<u32>>,
}

impl TestContentReader {
    pub(crate) fn returning(content: &[u8]) -> Self {
        Self {
            content: Ok(content.to_vec()),
            reads: Arc::default(),
        }
    }

    pub(crate) fn failing(kind: std::io::ErrorKind) -> Self {
        Self {
            content: Err(kind),
            reads: Arc::default(),
        }
    }

    pub(crate) fn reads(&self) -> u32 {
        *self.reads.lock().unwrap()
    }
}

#[async_trait]
impl ContentReader for TestContentReader {
    async fn read(&self) -> Result<Vec<u8>, std::io::Error> {
        *self.reads.lock().unwrap() += 1;
        match &self.content {
            Ok(content) => Ok(content.clone()),
            Err(kind) => Err(std::io::Error::from(*kind)),
        }
    }
}

/// A progress sink that records everything it receives, in order.
#[derive(Default)]
pub(crate) struct TestProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgressEvent {
    Report(Progress),
    EmbeddedChunks(u64),
    MaintenanceStarted,
}

impl TestProgress {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl ProgressSink for TestProgress {
    fn report(&self, progress: Progress) {
        self.events
            .lock()
            .unwrap()
            .push(ProgressEvent::Report(progress));
    }

    fn add_embedded_chunks(&self, count: u64) {
        self.events
            .lock()
            .unwrap()
            .push(ProgressEvent::EmbeddedChunks(count));
    }
}

impl GatewayProgressSink for TestProgress {
    fn maintenance_started(&self) {
        self.events
            .lock()
            .unwrap()
            .push(ProgressEvent::MaintenanceStarted);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::types::CollectionName;

    use super::super::DocumentText;
    use super::*;

    fn get_input(collection: &str) -> GetDocumentInput {
        GetDocumentInput {
            collection: CollectionName(collection.to_owned()),
            key: crate::types::Key("a.md".to_owned()),
            range: crate::query::ChunkRangeRequest {
                start: None,
                end: None,
            },
            size_limit: Some(10),
        }
    }

    #[tokio::test]
    async fn an_operation_answers_from_its_handler_and_keeps_the_input() {
        let operations = TestOperations::new().on_get_document(|input| {
            Ok(DocumentText {
                collection: input.collection.0.clone(),
                key: input.key.0.clone(),
                text: "body".to_owned(),
            })
        });

        let answer = operations.get_document(get_input("notes")).await.unwrap();

        assert_eq!(answer.text, "body");
        let received = operations.received();
        assert_eq!(received.get_document.len(), 1);
        assert_eq!(received.get_document[0].collection.0, "notes");
        assert_eq!(received.get_document[0].size_limit, Some(10));
    }

    #[tokio::test]
    async fn an_async_handler_can_ask_for_confirmation_during_the_operation() {
        let operations = TestOperations::new().on_delete_collection_async(|input| {
            Box::pin(async move {
                if input.confirmation.confirm().await {
                    Ok(DeleteCollectionOutcome::Deleted {
                        collection: input.name.clone(),
                    })
                } else {
                    Ok(DeleteCollectionOutcome::Declined)
                }
            })
        });
        let refusing = TestConfirmation::answering(false);
        let agreeing = TestConfirmation::answering(true);
        let input = |confirmation: &TestConfirmation| DeleteCollectionInput {
            name: CollectionName("notes".to_owned()),
            confirmation: Box::new(confirmation.clone()),
            optimize: true,
        };

        let declined = operations
            .delete_collection(input(&refusing))
            .await
            .unwrap();
        let deleted = operations
            .delete_collection(input(&agreeing))
            .await
            .unwrap();

        assert!(matches!(declined, DeleteCollectionOutcome::Declined));
        assert!(matches!(deleted, DeleteCollectionOutcome::Deleted { .. }));
        assert_eq!((refusing.asked(), agreeing.asked()), (1, 1));
        assert_eq!(operations.received().delete_collection.len(), 2);
    }

    #[tokio::test]
    async fn an_operation_without_input_counts_its_calls() {
        let operations = TestOperations::new().on_status(|()| {
            Err(OperationError::CollectionNotFound {
                collection: CollectionName("x".to_owned()),
            })
        });

        let _ = operations.status().await;
        let _ = operations.status().await;

        assert_eq!(operations.received().status.len(), 2);
    }

    #[tokio::test]
    #[should_panic(expected = "no handler for get_document")]
    async fn an_operation_without_a_handler_panics() {
        let operations = TestOperations::new();

        let _ = operations.get_document(get_input("notes")).await;
    }

    #[tokio::test]
    async fn the_confirmation_answers_and_counts() {
        let confirmation = TestConfirmation::answering(false);

        let answer = confirmation.confirm().await;

        assert!(!answer);
        assert_eq!(confirmation.asked(), 1);
    }

    #[tokio::test]
    async fn the_content_reader_returns_its_bytes_or_its_error() {
        let ok = TestContentReader::returning(b"text");
        let failing = TestContentReader::failing(std::io::ErrorKind::NotFound);

        assert_eq!(ok.read().await.unwrap(), b"text");
        assert_eq!(
            failing.read().await.unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[tokio::test]
    async fn a_kept_clone_of_the_content_reader_counts_the_reads_of_another() {
        let kept = TestContentReader::returning(b"text");
        let handed_over: Box<dyn ContentReader> = Box::new(kept.clone());

        handed_over.read().await.unwrap();

        assert_eq!(kept.reads(), 1);
    }

    #[test]
    fn the_progress_sink_records_events_in_order() {
        let progress = Arc::new(TestProgress::new());
        let as_gateway: Arc<dyn GatewayProgressSink> = progress.clone();

        as_gateway.report(Progress {
            done: 1,
            total: Some(2),
        });
        as_gateway.add_embedded_chunks(3);
        as_gateway.maintenance_started();

        assert_eq!(
            progress.events(),
            vec![
                ProgressEvent::Report(Progress {
                    done: 1,
                    total: Some(2)
                }),
                ProgressEvent::EmbeddedChunks(3),
                ProgressEvent::MaintenanceStarted,
            ]
        );
    }
}
