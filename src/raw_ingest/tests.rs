//! Unit tests of `raw_ingest`, with test doubles for `format` and `index`.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::io;
use std::mem::discriminant;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};

use super::*;
use crate::embedding::{Embedding, EmbeddingError};
use crate::format::test_double::{TestFormat, whole_text};
use crate::format::{Extracted, FileType, FormatError};
use crate::index::test_double::{IndexCall, TestIndex};
use crate::index::{
    DimensionCheck, DocumentToRegister, IndexError, IndexStatus, PartialDelete, QueryTables,
    RegistrationSink, StatusScope,
};
use crate::ingest::{PutInstruction, UpdateInstruction, UpdateReport};
use crate::types::test_double::{TestProgressSink, test_dir_lookup};
use crate::types::{Chunk, CollectionName, DirLookup, DocumentOutcome, Key, ProgressSink};

const TIMEOUT: Duration = Duration::from_secs(30);

fn attributes(value: Value) -> TypeAttributes {
    TypeAttributes(value.as_object().unwrap().clone())
}

fn component() -> Arc<dyn Ingest> {
    create(Arc::new(TestFormat::new()), Arc::new(TestIndex::new()))
}

/// Knows Markdown by its MIME type, and extracts the content as one chunk.
fn markdown() -> TestFormat {
    TestFormat::new().with_media_type("text/markdown", "markdown")
}

fn put_instruction(key: &str) -> PutInstruction {
    PutInstruction {
        collection: CollectionName("notes".to_owned()),
        attributes: TypeAttributes::default(),
        key: Key(key.to_owned()),
        media_type: "text/markdown".to_owned(),
        content: b"# A".to_vec(),
        overwrite: false,
    }
}

fn registered(index: &TestIndex) -> Vec<DocumentToRegister> {
    index
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            IndexCall::Register(document) => Some(document),
            _ => None,
        })
        .collect()
}

async fn panic_message(future: impl Future) -> String {
    let panic = AssertUnwindSafe(future).catch_unwind().await.err().unwrap();
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    panic.downcast_ref::<String>().unwrap().clone()
}

// The names of the type-specific attributes, checked without the component.

#[test]
fn accepts_no_attributes() {
    assert!(check_attribute_names(&TypeAttributes::default()).is_ok());
}

#[test]
fn rejects_any_attribute() {
    let attributes = TypeAttributes(
        json!({ "path": "/home/user/docs" })
            .as_object()
            .unwrap()
            .clone(),
    );

    assert!(matches!(
        check_attribute_names(&attributes),
        Err(IngestError::AttributesMissingOrExtra)
    ));
}

// What the component says it can do, and the operation it does not offer.

#[test]
fn it_takes_document_content_but_cannot_be_updated() {
    let ingest = component();

    assert!(!ingest.supports_update());
    assert!(ingest.supports_put());
}

#[tokio::test]
async fn an_update_is_refused_without_touching_the_index_or_the_progress() {
    let index = Arc::new(TestIndex::new());
    let ingest = create(Arc::new(TestFormat::new()), index.clone());
    let progress = TestProgressSink::new();
    let instruction = UpdateInstruction {
        collection: CollectionName("notes".to_owned()),
        attributes: TypeAttributes::default(),
        overwrite: false,
        parallelism: 1,
    };

    let stopped = ingest.update(instruction, &progress).await.unwrap_err();

    assert!(matches!(stopped.error, IngestError::UpdateNotSupported));
    assert_eq!(stopped.counts, UpdateReport::default());
    assert!(index.calls().is_empty());
    assert!(progress.reports().is_empty());
    assert_eq!(progress.embedded_chunks(), 0);
}

// Checking the type-specific attributes.

/// Neither directory is known, so a check that needed one could not succeed.
fn unknown_directories() -> DirLookup {
    test_dir_lookup(None, Err(io::ErrorKind::NotFound))
}

#[tokio::test]
async fn no_attributes_are_accepted_and_returned_as_they_are() {
    let checked = component()
        .check_attributes(TypeAttributes::default(), &unknown_directories())
        .await
        .unwrap();

    assert_eq!(checked, TypeAttributes::default());
}

#[tokio::test]
async fn any_attribute_is_refused_as_an_extra_one() {
    let result = component()
        .check_attributes(
            attributes(json!({ "path": "/home/user/docs" })),
            &unknown_directories(),
        )
        .await;

    assert!(matches!(result, Err(IngestError::AttributesMissingOrExtra)));
}

// Ingesting a document from its content.

#[tokio::test]
async fn the_body_and_chunks_from_format_are_registered_with_no_time() {
    let format = Arc::new(markdown().on_extract(|_, _| {
        Ok(Extracted {
            body: "first second".to_owned(),
            chunks: vec![Chunk { range: 0..5 }, Chunk { range: 6..12 }],
        })
    }));
    let index = Arc::new(TestIndex::new());
    let ingest = create(format.clone(), index.clone());

    let outcome = ingest.put(put_instruction("a.md")).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(
        format.extracted(),
        vec![(b"# A".to_vec(), FileType::for_test_double("markdown"))]
    );
    assert_eq!(
        registered(&index),
        vec![DocumentToRegister {
            collection: CollectionName("notes".to_owned()),
            key: Key("a.md".to_owned()),
            time: None,
            body: "first second".to_owned(),
            chunks: vec![Chunk { range: 0..5 }, Chunk { range: 6..12 }],
            overwrite: false,
        }]
    );
}

#[tokio::test]
async fn the_file_type_is_the_one_format_finds_for_the_media_type() {
    let format = Arc::new(
        TestFormat::new()
            .with_media_type("text/markdown", "markdown")
            .with_media_type("pdf", "pdf"),
    );
    let ingest = create(format.clone(), Arc::new(TestIndex::new()));
    let mut instruction = put_instruction("a.pdf");
    instruction.media_type = "pdf".to_owned();

    ingest.put(instruction).await.unwrap();

    let (_, file_type) = format.extracted().pop().unwrap();
    assert_eq!(file_type, FileType::for_test_double("pdf"));
}

#[tokio::test]
async fn the_outcome_the_index_passes_is_returned() {
    let outcomes = [
        DocumentOutcome::Added,
        DocumentOutcome::Changed,
        DocumentOutcome::Unchanged,
        DocumentOutcome::Reingested,
    ];
    for outcome in outcomes {
        let index = TestIndex::new().on_register(move |_, _| Ok(outcome));
        let ingest = create(Arc::new(markdown()), Arc::new(index));

        let returned = ingest.put(put_instruction("a.md")).await.unwrap();

        assert_eq!(returned, outcome);
    }
}

#[tokio::test]
async fn whether_to_overwrite_is_passed_on_to_the_index() {
    for overwrite in [false, true] {
        let index = Arc::new(TestIndex::new());
        let ingest = create(Arc::new(markdown()), index.clone());
        let mut instruction = put_instruction("a.md");
        instruction.overwrite = overwrite;

        ingest.put(instruction).await.unwrap();

        assert_eq!(registered(&index)[0].overwrite, overwrite);
    }
}

#[tokio::test]
async fn any_key_is_taken_as_it_is_even_an_empty_one() {
    for key in ["", " ", "../a b/./c.md", "メモ"] {
        let index = Arc::new(TestIndex::new());
        let ingest = create(Arc::new(markdown()), index.clone());

        ingest.put(put_instruction(key)).await.unwrap();

        assert_eq!(registered(&index)[0].key, Key(key.to_owned()));
    }
}

#[tokio::test]
async fn the_attributes_in_the_instruction_are_not_used() {
    let index = Arc::new(TestIndex::new());
    let ingest = create(Arc::new(markdown()), index.clone());
    let mut instruction = put_instruction("a.md");
    instruction.attributes = attributes(json!({ "path": "/home/user/docs" }));

    let outcome = ingest.put(instruction).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(registered(&index).len(), 1);
}

#[tokio::test]
async fn a_media_type_format_does_not_know_is_refused_before_anything_is_read() {
    let format = Arc::new(markdown());
    let index = Arc::new(TestIndex::new());
    let ingest = create(format.clone(), index.clone());
    let mut instruction = put_instruction("a.txt");
    instruction.media_type = "text/plain".to_owned();

    let result = ingest.put(instruction).await;

    assert!(matches!(result, Err(IngestError::UnsupportedFileType)));
    assert!(format.extracted().is_empty());
    assert!(index.calls().is_empty());
}

#[tokio::test]
async fn a_body_that_cannot_be_extracted_is_an_error_and_nothing_is_registered() {
    let format = markdown().on_extract(|_, _| {
        Err(FormatError::CannotExtract {
            source: "not UTF-8".into(),
        })
    });
    let index = Arc::new(TestIndex::new());
    let ingest = create(Arc::new(format), index.clone());

    let result = ingest.put(put_instruction("a.md")).await;

    let Err(IngestError::CannotExtract(FormatError::CannotExtract { source })) = result else {
        panic!("{result:?}");
    };
    assert_eq!(source.to_string(), "not UTF-8");
    assert!(index.calls().is_empty());
}

#[tokio::test]
async fn an_error_from_the_index_is_returned_wrapped() {
    let errors: [fn() -> IndexError; 4] = [
        || {
            IndexError::Embedding(EmbeddingError::ModelUnavailable {
                source: "no model".into(),
            })
        },
        || IndexError::CannotRead {
            source: "broken".into(),
        },
        || IndexError::CannotWrite {
            source: "disk full".into(),
        },
        || IndexError::RebuildIncomplete,
    ];
    for error in errors {
        let index = TestIndex::new().on_register(move |_, _| Err(error()));
        let ingest = create(Arc::new(markdown()), Arc::new(index));

        let result = ingest.put(put_instruction("a.md")).await;

        let Err(IngestError::Index(wrapped)) = result else {
            panic!("{result:?}");
        };
        assert_eq!(discriminant(&wrapped), discriminant(&error()));
    }
}

/// An `Index` that records how `register_stream` is called, and passes
/// `Added` for each document of the stream, or nothing while it is made to
/// pass nothing. The test double of `index` keeps neither the parallelism nor
/// a way to return without passing an outcome.
struct RecordingIndex {
    passes_outcomes: AtomicBool,
    calls: Mutex<Vec<StreamCall>>,
}

#[derive(Debug, PartialEq)]
struct StreamCall {
    keys: Vec<Key>,
    parallelism: usize,
    with_progress: bool,
}

impl RecordingIndex {
    fn new(passes_outcomes: bool) -> Self {
        Self {
            passes_outcomes: AtomicBool::new(passes_outcomes),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn start_passing_outcomes(&self) {
        self.passes_outcomes.store(true, Ordering::SeqCst);
    }

    fn calls(&self) -> Vec<StreamCall> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

#[async_trait]
impl Index for RecordingIndex {
    async fn register_stream(
        &self,
        documents: BoxStream<'_, DocumentToRegister>,
        parallelism: usize,
        results: &dyn RegistrationSink,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        let documents: Vec<DocumentToRegister> = documents.collect().await;
        let passes_outcomes = self.passes_outcomes.load(Ordering::SeqCst);
        for document in documents.iter().filter(|_| passes_outcomes) {
            results.registered(&document.collection, &document.key, DocumentOutcome::Added);
        }
        self.calls.lock().unwrap().push(StreamCall {
            keys: documents.into_iter().map(|document| document.key).collect(),
            parallelism,
            with_progress: progress.is_some(),
        });
        Ok(())
    }

    async fn delete_others(
        &self,
        _collection: &CollectionName,
        _keep: &HashSet<Key>,
        _unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete> {
        unimplemented!()
    }

    async fn delete_document(
        &self,
        _collection: &CollectionName,
        _key: &Key,
    ) -> Result<(), IndexError> {
        unimplemented!()
    }

    async fn has_document(
        &self,
        _collection: &CollectionName,
        _key: &Key,
    ) -> Result<bool, IndexError> {
        unimplemented!()
    }

    async fn document_counts(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        unimplemented!()
    }

    async fn status(
        &self,
        _scope: StatusScope,
        _compare_with: &dyn Embedding,
        _check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError> {
        unimplemented!()
    }

    async fn rebuild_embeddings(
        &self,
        _include_configured_model: bool,
        _parallelism: usize,
        _progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        unimplemented!()
    }

    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError> {
        unimplemented!()
    }

    async fn maintain(&self) -> Result<(), IndexError> {
        unimplemented!()
    }

    async fn check_ingestable(&self) -> Result<(), IndexError> {
        unimplemented!()
    }
}

#[tokio::test]
async fn the_document_goes_to_the_index_alone_with_a_parallelism_of_one_and_no_progress() {
    let index = Arc::new(RecordingIndex::new(true));
    let ingest = create(Arc::new(markdown()), index.clone());

    ingest.put(put_instruction("a.md")).await.unwrap();

    assert_eq!(
        index.calls(),
        vec![StreamCall {
            keys: vec![Key("a.md".to_owned())],
            parallelism: 1,
            with_progress: false,
        }]
    );
}

#[tokio::test]
async fn an_index_that_succeeds_without_passing_the_outcome_is_a_bug_and_the_component_still_works()
{
    let index = Arc::new(RecordingIndex::new(false));
    let ingest = create(Arc::new(markdown()), index.clone());

    let message = panic_message(ingest.put(put_instruction("a.md"))).await;

    assert!(
        message.contains("passed no outcome"),
        "unexpected panic message: {message}"
    );
    index.start_passing_outcomes();
    let outcome = ingest.put(put_instruction("a.md")).await.unwrap();
    assert_eq!(outcome, DocumentOutcome::Added);
}

// Panics, other tasks, and calls at the same time.

#[tokio::test]
async fn a_panic_while_extracting_reaches_the_caller_and_the_component_still_works() {
    let panicked = Arc::new(AtomicBool::new(false));
    let first_call = panicked.clone();
    let format = markdown().on_extract(move |content, _| {
        if !first_call.swap(true, Ordering::SeqCst) {
            panic!("the extraction panicked");
        }
        Ok(whole_text(content))
    });
    let index = Arc::new(TestIndex::new());
    let ingest = create(Arc::new(format), index.clone());

    let message = panic_message(ingest.put(put_instruction("a.md"))).await;

    assert_eq!(message, "the extraction panicked");
    assert!(index.calls().is_empty());
    let outcome = ingest.put(put_instruction("a.md")).await.unwrap();
    assert_eq!(outcome, DocumentOutcome::Added);
}

#[tokio::test]
async fn a_panic_in_the_index_reaches_the_caller_and_the_component_still_works() {
    let panicked = Arc::new(AtomicBool::new(false));
    let first_call = panicked.clone();
    let index = TestIndex::new().on_register(move |_, _| {
        if !first_call.swap(true, Ordering::SeqCst) {
            panic!("the index panicked");
        }
        Ok(DocumentOutcome::Added)
    });
    let ingest = create(Arc::new(markdown()), Arc::new(index));

    let message = panic_message(ingest.put(put_instruction("a.md"))).await;

    assert_eq!(message, "the index panicked");
    let outcome = ingest.put(put_instruction("a.md")).await.unwrap();
    assert_eq!(outcome, DocumentOutcome::Added);
}

/// A signal that an extraction waits for, with a limit on the wait: the wait
/// blocks its thread, so the runtime's own timers would not help.
#[derive(Clone, Default)]
struct Signal(Arc<(Mutex<bool>, Condvar)>);

impl Signal {
    fn send(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }

    fn wait(&self) {
        let (sent, _) = self
            .0
            .1
            .wait_timeout_while(self.0.0.lock().unwrap(), TIMEOUT, |sent| !*sent)
            .unwrap();
        assert!(
            *sent,
            "nothing sent the signal: the extraction blocked the runtime"
        );
    }
}

#[tokio::test]
async fn extracting_does_not_stop_other_tasks() {
    let signal = Signal::default();
    let waiting = signal.clone();
    let format = markdown().on_extract(move |content, _| {
        waiting.wait();
        Ok(whole_text(content))
    });
    let ingest = create(Arc::new(format), Arc::new(TestIndex::new()));

    tokio::spawn(async move { signal.send() });

    ingest.put(put_instruction("a.md")).await.unwrap();
}

/// Lets `count` callers through only together.
struct Rendezvous(Arc<(Mutex<usize>, Condvar)>, usize);

impl Rendezvous {
    fn new(count: usize) -> Self {
        Self(Arc::new((Mutex::new(0), Condvar::new())), count)
    }

    fn arrive(&self) {
        let mut arrived = self.0.0.lock().unwrap();
        *arrived += 1;
        self.0.1.notify_all();
        let (arrived, _) = self
            .0
            .1
            .wait_timeout_while(arrived, TIMEOUT, |arrived| *arrived < self.1)
            .unwrap();
        assert!(*arrived >= self.1, "the documents were not put at once");
    }
}

#[tokio::test]
async fn two_documents_can_be_put_at_once() {
    let rendezvous = Rendezvous::new(2);
    let format = markdown().on_extract(move |content, _| {
        rendezvous.arrive();
        Ok(whole_text(content))
    });
    let index = Arc::new(TestIndex::new());
    let ingest = create(Arc::new(format), index.clone());

    let (a, b) = tokio::join!(
        ingest.put(put_instruction("a.md")),
        ingest.put(put_instruction("b.md"))
    );

    assert_eq!(a.unwrap(), DocumentOutcome::Added);
    assert_eq!(b.unwrap(), DocumentOutcome::Added);
    assert_eq!(registered(&index).len(), 2);
}
