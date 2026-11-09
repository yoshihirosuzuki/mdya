//! Unit tests of the shared ingest flow, with test doubles for `format`,
//! `index`, and the item contents.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::Level;

use super::ingest_items;
use crate::embedding::Embedding;
use crate::format::test_double::{TestFormat, whole_text};
use crate::format::{FileType, FormatError};
use crate::index::test_double::{IndexCall, TestIndex};
use crate::index::{
    DimensionCheck, DocumentToRegister, Index, IndexError, IndexStatus, PartialDelete, QueryTables,
    RegistrationSink, StatusScope,
};
use crate::ingest::{
    IngestError, IngestItem, IngestItems, ItemContents, UpdateInstruction, UpdateReport,
    UpdateStopped,
};
use crate::test_support::log_receiver::{LogRecord, capture_async};
use crate::types::test_double::TestProgressSink;
use crate::types::{
    BoxError, Chunk, CollectionName, DocumentOutcome, Key, Progress, ProgressSink, Time,
    TypeAttributes,
};

const UNREADABLE: &str = "a document cannot be read and was skipped";
const UNREADABLE_WITHOUT_KEY: &str = "a document without a key cannot be read and was skipped";

fn instruction(parallelism: usize) -> UpdateInstruction {
    UpdateInstruction {
        collection: CollectionName("notes".to_owned()),
        attributes: TypeAttributes::default(),
        overwrite: false,
        parallelism,
    }
}

fn time(seconds: u64) -> Time {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn item(key: &str) -> IngestItem {
    IngestItem {
        key: Key(key.to_owned()),
        time: time(1),
        file_type: FileType::for_test_double("markdown"),
    }
}

fn items_for(keys: &[&str]) -> IngestItems {
    IngestItems {
        items: keys.iter().map(|key| item(key)).collect(),
        unreadable_without_key: Vec::new(),
        delete_except_unreadable_dirs: None,
        unreadable_places: 0,
    }
}

fn content_of(key: &str) -> Vec<u8> {
    format!("content of {key}").into_bytes()
}

fn keys(names: &[&str]) -> HashSet<Key> {
    names.iter().map(|name| Key((*name).to_owned())).collect()
}

fn warning(message: &str, fields: &[(&str, &str)]) -> LogRecord {
    LogRecord {
        level: Level::WARN,
        message: message.to_owned(),
        fields: fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

fn registered_keys(index: &TestIndex) -> HashSet<Key> {
    index
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            IndexCall::Register(document) => Some(document.key),
            _ => None,
        })
        .collect()
}

/// Sets a flag when dropped, unless disarmed first: tells whether a future was
/// dropped before it returned.
struct DropWatch(Option<Arc<AtomicBool>>);

impl DropWatch {
    fn new(flag: &Arc<AtomicBool>) -> Self {
        Self(Some(flag.clone()))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for DropWatch {
    fn drop(&mut self) {
        if let Some(flag) = self.0.take() {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Hands over the scripted contents in order, then reports that it is done.
/// Counts the calls, can hold the calls after the first few until the test
/// adds permits, can panic on a call, and records whether a call was dropped
/// before it returned.
struct ScriptedContents {
    script: VecDeque<Result<Vec<u8>, BoxError>>,
    calls: Arc<AtomicUsize>,
    hold_after: Option<(usize, Arc<Semaphore>)>,
    panic_on_call: Option<usize>,
    dropped_before_return: Arc<AtomicBool>,
}

impl ScriptedContents {
    fn new(script: Vec<Result<Vec<u8>, BoxError>>) -> Self {
        Self {
            script: script.into(),
            calls: Arc::new(AtomicUsize::new(0)),
            hold_after: None,
            panic_on_call: None,
            dropped_before_return: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Panics on the call with this index (counting from 0).
    fn panicking_on_call(mut self, call: usize) -> Self {
        self.panic_on_call = Some(call);
        self
    }

    fn of(keys: &[&str]) -> Self {
        Self::new(keys.iter().map(|key| Ok(content_of(key))).collect())
    }

    /// Holds every call after the first `free_calls` until `gate` has a permit.
    fn holding_calls_after(mut self, free_calls: usize, gate: Arc<Semaphore>) -> Self {
        self.hold_after = Some((free_calls, gate));
        self
    }
}

#[async_trait]
impl ItemContents for ScriptedContents {
    async fn next(&mut self) -> Option<Result<Vec<u8>, BoxError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.panic_on_call == Some(call) {
            panic!("next panicked on call {call}");
        }
        let watch = DropWatch::new(&self.dropped_before_return);
        if let Some((free_calls, gate)) = &self.hold_after
            && call >= *free_calls
        {
            gate.acquire().await.unwrap().forget();
        }
        watch.disarm();
        self.script.pop_front()
    }
}

/// An `Index` that takes the documents of the stream, sends each key it
/// receives, and writes a document only when `gate` has a permit, then passes
/// `Added` for it. It can fail or panic for a key, records whether a write was
/// dropped before it returned, and records the parallelism of each call. By
/// default it writes each document before it asks for the next one.
struct GatedIndex {
    gate: Arc<Semaphore>,
    started: UnboundedSender<Key>,
    registered: Mutex<Vec<Key>>,
    fail_on: Option<Key>,
    panic_on: Option<Key>,
    dropped_before_return: Arc<AtomicBool>,
    parallelisms: Mutex<Vec<usize>>,
    writes_after_the_stream_ends: bool,
}

impl GatedIndex {
    fn new(gate: Arc<Semaphore>) -> (Self, UnboundedReceiver<Key>) {
        let (started, receiver) = mpsc::unbounded_channel();
        let index = Self {
            gate,
            started,
            registered: Mutex::new(Vec::new()),
            fail_on: None,
            panic_on: None,
            dropped_before_return: Arc::new(AtomicBool::new(false)),
            parallelisms: Mutex::new(Vec::new()),
            writes_after_the_stream_ends: false,
        };
        (index, receiver)
    }

    fn failing_on(mut self, key: &str) -> Self {
        self.fail_on = Some(Key(key.to_owned()));
        self
    }

    fn panicking_on(mut self, key: &str) -> Self {
        self.panic_on = Some(Key(key.to_owned()));
        self
    }

    /// Takes all the documents of the stream before it writes any, so that it
    /// still has documents to write when the stream ends.
    fn writing_after_the_stream_ends(mut self) -> Self {
        self.writes_after_the_stream_ends = true;
        self
    }

    fn registered(&self) -> Vec<Key> {
        self.registered.lock().unwrap().clone()
    }

    async fn write(
        &self,
        document: DocumentToRegister,
        results: &dyn RegistrationSink,
    ) -> Result<(), IndexError> {
        if self.fail_on.as_ref() == Some(&document.key) {
            return Err(IndexError::CannotWrite {
                source: "the index is broken".into(),
            });
        }
        if self.panic_on.as_ref() == Some(&document.key) {
            panic!("the index panicked for {}", document.key.0);
        }
        let watch = DropWatch::new(&self.dropped_before_return);
        self.gate.acquire().await.unwrap().forget();
        watch.disarm();
        self.registered.lock().unwrap().push(document.key.clone());
        results.registered(&document.collection, &document.key, DocumentOutcome::Added);
        Ok(())
    }
}

#[async_trait]
impl Index for GatedIndex {
    async fn register_stream(
        &self,
        mut documents: BoxStream<'_, DocumentToRegister>,
        parallelism: usize,
        results: &dyn RegistrationSink,
        _progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        self.parallelisms.lock().unwrap().push(parallelism);
        let mut held = Vec::new();
        while let Some(document) = documents.next().await {
            self.started.send(document.key.clone()).unwrap();
            if self.writes_after_the_stream_ends {
                held.push(document);
                continue;
            }
            self.write(document, results).await?;
        }
        for document in held {
            self.write(document, results).await?;
        }
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

/// Hands over `a.md`'s content, then `b.md`'s once `gate` has a permit, and
/// panics when called a third time, before it returns a future, as an
/// implementation written without `async_trait` can.
struct PanickingBeforeTheFuture {
    calls: usize,
    gate: Arc<Semaphore>,
}

impl ItemContents for PanickingBeforeTheFuture {
    fn next<'life0, 'async_trait>(
        &'life0 mut self,
    ) -> Pin<Box<dyn Future<Output = Option<Result<Vec<u8>, BoxError>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        self.calls += 1;
        let gate = self.gate.clone();
        match self.calls {
            1 => Box::pin(async { Some(Ok(content_of("a.md"))) }),
            2 => Box::pin(async move {
                gate.acquire().await.unwrap().forget();
                Some(Ok(content_of("b.md")))
            }),
            _ => panic!("next panicked before it returned a future"),
        }
    }
}

/// Panics on the report of the first finished document, after telling the
/// test through `panicked`.
struct PanickingProgress {
    panicked: UnboundedSender<()>,
}

impl ProgressSink for PanickingProgress {
    fn report(&self, progress: Progress) {
        if progress.done == 1 {
            self.panicked.send(()).unwrap();
            panic!("the progress sink panicked");
        }
    }

    fn add_embedded_chunks(&self, _count: u64) {}
}

type Flow = tokio::task::JoinHandle<Result<UpdateReport, UpdateStopped>>;

/// Runs the flow on its own task, so that the test can drive the gates while
/// it runs.
fn spawn_flow(
    instruction: UpdateInstruction,
    items: IngestItems,
    contents: impl ItemContents + 'static,
    index: Arc<dyn Index>,
) -> Flow {
    let format = Arc::new(TestFormat::new());
    spawn_flow_with(instruction, items, contents, format, index)
}

fn spawn_flow_with(
    instruction: UpdateInstruction,
    items: IngestItems,
    mut contents: impl ItemContents + 'static,
    format: Arc<TestFormat>,
    index: Arc<dyn Index>,
) -> Flow {
    tokio::spawn(async move {
        let progress = TestProgressSink::new();
        ingest_items(&instruction, items, &mut contents, format, index, &progress).await
    })
}

/// Waits for a permit of `gate`, on the blocking thread of an extraction.
fn wait_for(gate: &Semaphore) {
    Handle::current().block_on(gate.acquire()).unwrap().forget();
}

/// A format that holds the extraction of `key`'s content until `gate` has a
/// permit.
fn format_holding(key: &'static str, gate: Arc<Semaphore>) -> Arc<TestFormat> {
    Arc::new(TestFormat::new().on_extract(move |content, _| {
        if content == content_of(key) {
            wait_for(&gate);
        }
        Ok(whole_text(content))
    }))
}

/// The next key that the gated index starts to register. Fails the test if
/// the flow ends first, instead of waiting for ever. A key already sent wins
/// over the flow's end, since the flow may end right after sending it.
async fn next_start(started: &mut UnboundedReceiver<Key>, flow: &mut Flow) -> Key {
    tokio::select! {
        biased;
        key = started.recv() => key.unwrap(),
        result = flow => panic!("the flow ended before the next registration started: {result:?}"),
    }
}

// --- Each document ---

#[tokio::test]
async fn each_document_is_extracted_and_registered_with_its_key_time_and_overwrite() {
    let index = Arc::new(TestIndex::new());
    let format = Arc::new(TestFormat::new());
    let mut instruction = instruction(4);
    instruction.overwrite = true;
    let mut items = items_for(&["a.md", "b.md"]);
    items.items[1].time = time(2);

    let report = ingest_items(
        &instruction,
        items,
        &mut ScriptedContents::of(&["a.md", "b.md"]),
        format.clone(),
        index.clone(),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    let mut registered: Vec<DocumentToRegister> = index
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            IndexCall::Register(document) => Some(document),
            _ => None,
        })
        .collect();
    registered.sort_by(|first, second| first.key.cmp(&second.key));
    let document = |key: &str, seconds: u64| DocumentToRegister {
        collection: CollectionName("notes".to_owned()),
        key: Key(key.to_owned()),
        time: Some(time(seconds)),
        body: format!("content of {key}"),
        chunks: vec![Chunk {
            range: 0..format!("content of {key}").len(),
        }],
        overwrite: true,
    };
    assert_eq!(registered, vec![document("a.md", 1), document("b.md", 2)]);
    let mut extracted = format.extracted();
    extracted.sort_by(|first, second| first.0.cmp(&second.0));
    let markdown = FileType::for_test_double("markdown");
    assert_eq!(
        extracted,
        vec![
            (content_of("a.md"), markdown.clone()),
            (content_of("b.md"), markdown)
        ]
    );
    assert_eq!(report.added, 2);
}

#[tokio::test]
async fn the_progress_sink_is_passed_to_the_index() {
    let index = Arc::new(TestIndex::new().on_register(|_, progress| {
        progress.unwrap().add_embedded_chunks(3);
        Ok(DocumentOutcome::Added)
    }));
    let progress = TestProgressSink::new();

    ingest_items(
        &instruction(4),
        items_for(&["a.md", "b.md"]),
        &mut ScriptedContents::of(&["a.md", "b.md"]),
        Arc::new(TestFormat::new()),
        index,
        &progress,
    )
    .await
    .unwrap();

    assert_eq!(progress.embedded_chunks(), 6);
}

#[tokio::test]
async fn outcomes_from_the_index_are_counted() {
    let index = Arc::new(TestIndex::new().on_register(|document, _| {
        Ok(match document.key.0.as_str() {
            "changed.md" => DocumentOutcome::Changed,
            "unchanged.md" => DocumentOutcome::Unchanged,
            "reingested.md" => DocumentOutcome::Reingested,
            _ => DocumentOutcome::Added,
        })
    }));
    let names = [
        "added-1.md",
        "added-2.md",
        "changed.md",
        "unchanged.md",
        "reingested.md",
    ];

    let report = ingest_items(
        &instruction(4),
        items_for(&names),
        &mut ScriptedContents::of(&names),
        Arc::new(TestFormat::new()),
        index,
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        report,
        UpdateReport {
            added: 2,
            changed: 1,
            unchanged: 1,
            reingested: 1,
            ..UpdateReport::default()
        }
    );
}

// --- Documents that cannot be read ---

#[tokio::test]
async fn a_document_whose_content_cannot_be_read_is_warned_counted_as_failed_and_not_registered() {
    let index = Arc::new(TestIndex::new());
    let mut contents = ScriptedContents::new(vec![
        Ok(content_of("a.md")),
        Err("permission denied".into()),
        Ok(content_of("c.md")),
    ]);

    let (report, warnings) = capture_async(ingest_items(
        &instruction(4),
        items_for(&["a.md", "b.md", "c.md"]),
        &mut contents,
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    ))
    .await;

    let report = report.unwrap();
    assert_eq!((report.added, report.failed), (2, 1));
    assert_eq!(registered_keys(&index), keys(&["a.md", "c.md"]));
    assert_eq!(
        warnings,
        vec![warning(
            UNREADABLE,
            &[
                ("collection", "notes"),
                ("key", "b.md"),
                ("cause", "permission denied")
            ]
        )]
    );
}

#[tokio::test]
async fn a_document_whose_body_cannot_be_extracted_is_warned_counted_as_failed_and_not_registered()
{
    let index = Arc::new(TestIndex::new());
    let format = Arc::new(TestFormat::new().on_extract(|content, _| {
        if content == content_of("b.md") {
            return Err(FormatError::CannotExtract {
                source: "not a PDF".into(),
            });
        }
        Ok(whole_text(content))
    }));

    let (report, warnings) = capture_async(ingest_items(
        &instruction(4),
        items_for(&["a.md", "b.md", "c.md"]),
        &mut ScriptedContents::of(&["a.md", "b.md", "c.md"]),
        format,
        index.clone(),
        &TestProgressSink::new(),
    ))
    .await;

    let report = report.unwrap();
    assert_eq!((report.added, report.failed), (2, 1));
    assert_eq!(registered_keys(&index), keys(&["a.md", "c.md"]));
    assert_eq!(
        warnings,
        vec![warning(
            UNREADABLE,
            &[
                ("collection", "notes"),
                ("key", "b.md"),
                (
                    "cause",
                    "the document's body cannot be extracted: not a PDF"
                )
            ]
        )]
    );
}

#[tokio::test]
async fn an_item_without_a_key_is_warned_by_name_counted_as_failed_and_not_kept() {
    let index = Arc::new(TestIndex::new());
    let mut items = items_for(&["a.md"]);
    items.unreadable_without_key =
        vec![("b\u{fffd}.md".to_owned(), "the name is not UTF-8".into())];
    items.delete_except_unreadable_dirs = Some(Vec::new());

    let (report, warnings) = capture_async(ingest_items(
        &instruction(4),
        items,
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    ))
    .await;

    let report = report.unwrap();
    assert_eq!((report.added, report.failed), (1, 1));
    assert_eq!(
        warnings,
        vec![warning(
            UNREADABLE_WITHOUT_KEY,
            &[
                ("collection", "notes"),
                ("name", "b\u{fffd}.md"),
                ("cause", "the name is not UTF-8")
            ]
        )]
    );
    assert!(index.calls().contains(&IndexCall::DeleteOthers {
        collection: CollectionName("notes".to_owned()),
        keep: keys(&["a.md"]),
        unreadable_dirs: Vec::new(),
    }));
}

#[tokio::test]
async fn when_the_contents_end_early_the_remaining_items_are_warned_and_counted_as_failed() {
    let index = Arc::new(TestIndex::new());

    let (report, warnings) = capture_async(ingest_items(
        &instruction(1),
        items_for(&["a.md", "b.md", "c.md"]),
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    ))
    .await;

    let report = report.unwrap();
    assert_eq!((report.added, report.failed), (1, 2));
    assert_eq!(registered_keys(&index), keys(&["a.md"]));
    let cause = "the ingest implementation handed over no content for it";
    assert_eq!(
        warnings,
        vec![
            warning(
                UNREADABLE,
                &[("collection", "notes"), ("key", "b.md"), ("cause", cause)]
            ),
            warning(
                UNREADABLE,
                &[("collection", "notes"), ("key", "c.md"), ("cause", cause)]
            ),
        ]
    );
}

#[tokio::test]
async fn control_characters_in_a_key_are_kept_in_the_warning() {
    let mut contents = ScriptedContents::new(vec![Err("broken".into())]);

    let (_, warnings) = capture_async(ingest_items(
        &instruction(4),
        items_for(&["a\u{7}.md"]),
        &mut contents,
        Arc::new(TestFormat::new()),
        Arc::new(TestIndex::new()),
        &TestProgressSink::new(),
    ))
    .await;

    assert_eq!(
        warnings,
        vec![warning(
            UNREADABLE,
            &[
                ("collection", "notes"),
                ("key", "a\u{7}.md"),
                ("cause", "broken")
            ]
        )]
    );
}

// --- Progress ---

#[tokio::test]
async fn progress_starts_at_zero_and_counts_every_document() {
    let mut items = items_for(&["a.md", "b.md"]);
    items.unreadable_without_key =
        vec![("c\u{fffd}.md".to_owned(), "the name is not UTF-8".into())];
    let mut contents = ScriptedContents::new(vec![Ok(content_of("a.md")), Err("broken".into())]);
    let progress = TestProgressSink::new();

    ingest_items(
        &instruction(4),
        items,
        &mut contents,
        Arc::new(TestFormat::new()),
        Arc::new(TestIndex::new()),
        &progress,
    )
    .await
    .unwrap();

    let reported = |done| Progress {
        done,
        total: Some(3),
    };
    assert_eq!(
        progress.reports(),
        vec![reported(0), reported(1), reported(2), reported(3)]
    );
}

// --- Deleting the other documents ---

#[tokio::test]
async fn no_deletion_is_requested_when_the_items_do_not_ask_for_it() {
    let index = Arc::new(TestIndex::new());

    ingest_items(
        &instruction(4),
        items_for(&["a.md"]),
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    assert!(
        !index
            .calls()
            .iter()
            .any(|call| matches!(call, IndexCall::DeleteOthers { .. }))
    );
}

#[tokio::test]
async fn deletion_keeps_every_item_with_a_key_and_passes_the_unreadable_dirs() {
    let index = Arc::new(TestIndex::new().on_delete_others(|_, _, _| Ok(3)));
    let mut items = items_for(&["a.md", "b.md"]);
    items.unreadable_without_key =
        vec![("c\u{fffd}.md".to_owned(), "the name is not UTF-8".into())];
    items.delete_except_unreadable_dirs = Some(vec!["unreadable".to_owned()]);
    let mut contents = ScriptedContents::new(vec![Ok(content_of("a.md")), Err("broken".into())]);

    let report = ingest_items(
        &instruction(4),
        items,
        &mut contents,
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.deleted, 3);
    assert!(index.calls().contains(&IndexCall::DeleteOthers {
        collection: CollectionName("notes".to_owned()),
        keep: keys(&["a.md", "b.md"]),
        unreadable_dirs: vec!["unreadable".to_owned()],
    }));
}

#[tokio::test]
async fn a_partial_delete_adds_its_deleted_count_and_returns_the_index_error() {
    let index = Arc::new(TestIndex::new().on_delete_others(|_, _, _| {
        Err(PartialDelete {
            deleted: 2,
            error: IndexError::CannotWrite {
                source: "the disk is full".into(),
            },
        })
    }));
    let mut items = items_for(&["a.md"]);
    items.delete_except_unreadable_dirs = Some(Vec::new());

    let result = ingest_items(
        &instruction(4),
        items,
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        index,
        &TestProgressSink::new(),
    )
    .await;

    let Err(UpdateStopped { counts, error }) = result else {
        panic!("expected UpdateStopped, got {result:?}");
    };
    assert_eq!((counts.added, counts.deleted), (1, 2));
    assert!(matches!(
        error,
        IngestError::Index(IndexError::CannotWrite { .. })
    ));
}

// --- The places that could not be read ---

#[tokio::test]
async fn unreadable_places_are_in_the_report() {
    let mut items = items_for(&["a.md"]);
    items.unreadable_places = 4;

    let report = ingest_items(
        &instruction(4),
        items,
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        Arc::new(TestIndex::new()),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.unreadable_places, 4);
}

#[tokio::test]
async fn unreadable_places_are_in_the_counts_of_an_error() {
    let index = Arc::new(TestIndex::new().on_register(|_, _| {
        Err(IndexError::CannotWrite {
            source: "the disk is full".into(),
        })
    }));
    let mut items = items_for(&["a.md"]);
    items.unreadable_places = 4;

    let result = ingest_items(
        &instruction(4),
        items,
        &mut ScriptedContents::of(&["a.md"]),
        Arc::new(TestFormat::new()),
        index,
        &TestProgressSink::new(),
    )
    .await;

    let Err(UpdateStopped { counts, .. }) = result else {
        panic!("expected UpdateStopped, got {result:?}");
    };
    assert_eq!(counts.unreadable_places, 4);
}

// --- The stream of documents ---

#[tokio::test]
async fn the_documents_go_to_the_index_as_one_stream_with_the_parallelism() {
    let (index, _started) = GatedIndex::new(Arc::new(Semaphore::new(3)));
    let index = Arc::new(index);
    let names = ["a.md", "b.md", "c.md"];

    let report = spawn_flow(
        instruction(2),
        items_for(&names),
        ScriptedContents::of(&names),
        index.clone(),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(report.added, 3);
    assert_eq!(*index.parallelisms.lock().unwrap(), vec![2]);
    assert_eq!(index.registered().len(), 3);
}

#[tokio::test]
async fn documents_are_extracted_up_to_the_parallelism_at_once() {
    let (index, _started) = GatedIndex::new(Arc::new(Semaphore::new(5)));
    let (extracting, mut extraction_started) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let format = Arc::new(TestFormat::new().on_extract(move |content, _| {
        extracting.send(()).unwrap();
        wait_for(&held);
        Ok(whole_text(content))
    }));
    let names = ["a.md", "b.md", "c.md", "d.md", "e.md"];
    let contents = ScriptedContents::of(&names);
    let calls = contents.calls.clone();

    let flow = spawn_flow_with(
        instruction(2),
        items_for(&names),
        contents,
        format,
        Arc::new(index),
    );
    extraction_started.recv().await.unwrap();
    extraction_started.recv().await.unwrap();
    // Time for a third extraction to start, if one were to.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(extraction_started.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gate.add_permits(names.len());
    let report = flow.await.unwrap().unwrap();

    assert_eq!(report.added, 5);
}

#[tokio::test]
async fn the_next_content_is_requested_only_when_a_document_can_start() {
    let gate = Arc::new(Semaphore::new(0));
    let (index, mut started) = GatedIndex::new(gate.clone());
    let contents = ScriptedContents::of(&["a.md", "b.md"]);
    let calls = contents.calls.clone();

    let mut flow = spawn_flow(
        instruction(1),
        items_for(&["a.md", "b.md"]),
        contents,
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    gate.add_permits(1);
    next_start(&mut started, &mut flow).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gate.add_permits(1);
    flow.await.unwrap().unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn after_the_index_fails_no_new_document_starts_and_only_passed_outcomes_are_counted() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(1)));
    let index = Arc::new(index.failing_on("b.md"));
    let names = ["a.md", "b.md", "c.md", "d.md"];
    let contents = ScriptedContents::of(&names);
    let calls = contents.calls.clone();

    let flow = spawn_flow(instruction(1), items_for(&names), contents, index.clone());
    let result = flow.await.unwrap();

    assert_eq!(started.try_recv().unwrap(), Key("a.md".to_owned()));
    assert_eq!(started.try_recv().unwrap(), Key("b.md".to_owned()));
    assert!(started.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(index.registered(), vec![Key("a.md".to_owned())]);
    let Err(UpdateStopped { counts, error }) = result else {
        panic!("expected UpdateStopped, got {result:?}");
    };
    // The index received `b.md` and passed no outcome for it.
    assert_eq!(counts.added, 1);
    assert!(matches!(
        error,
        IngestError::Index(IndexError::CannotWrite { .. })
    ));
}

#[tokio::test]
async fn the_progress_counts_only_the_documents_whose_outcome_the_index_passed() {
    let (index, _started) = GatedIndex::new(Arc::new(Semaphore::new(1)));
    let index: Arc<dyn Index> = Arc::new(index.failing_on("b.md"));
    let progress = Arc::new(TestProgressSink::new());
    let flow_progress = progress.clone();
    let names = ["a.md", "b.md", "c.md", "d.md"];

    let flow = tokio::spawn(async move {
        let format = Arc::new(TestFormat::new());
        let mut contents = ScriptedContents::of(&names);
        ingest_items(
            &instruction(1),
            items_for(&names),
            &mut contents,
            format,
            index,
            &*flow_progress,
        )
        .await
    });
    flow.await.unwrap().unwrap_err();

    let reported = |done| Progress {
        done,
        total: Some(4),
    };
    assert_eq!(progress.reports(), vec![reported(0), reported(1)]);
}

#[tokio::test]
async fn a_document_being_extracted_when_the_index_stops_is_not_passed_to_it() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(2)));
    let index = Arc::new(index.failing_on("a.md"));
    let gate = Arc::new(Semaphore::new(0));
    let format = format_holding("b.md", gate.clone());

    let mut flow = spawn_flow_with(
        instruction(2),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]),
        format,
        index.clone(),
    );
    let first = next_start(&mut started, &mut flow).await;
    assert_eq!(first, Key("a.md".to_owned()));
    assert!(!flow.is_finished());
    gate.add_permits(1);
    let result = flow.await.unwrap();

    assert!(started.try_recv().is_err());
    assert_eq!(index.registered(), Vec::<Key>::new());
    let Err(UpdateStopped { counts, error }) = result else {
        panic!("expected UpdateStopped, got {result:?}");
    };
    assert_eq!(counts, UpdateReport::default());
    assert!(matches!(error, IngestError::Index(_)));
}

#[tokio::test]
async fn a_document_that_cannot_be_extracted_after_the_index_stops_is_counted_as_failed() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(2)));
    let index = index.failing_on("a.md");
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let format = Arc::new(TestFormat::new().on_extract(move |content, _| {
        if content == content_of("b.md") {
            wait_for(&held);
            return Err(FormatError::CannotExtract {
                source: "not a PDF".into(),
            });
        }
        Ok(whole_text(content))
    }));

    let mut flow = spawn_flow_with(
        instruction(2),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]),
        format,
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    gate.add_permits(1);
    let result = flow.await.unwrap();

    let Err(UpdateStopped { counts, .. }) = result else {
        panic!("expected UpdateStopped, got {result:?}");
    };
    assert_eq!((counts.added, counts.failed), (0, 1));
}

// --- Not dropping what was started ---

#[tokio::test]
async fn the_pending_next_is_awaited_when_the_index_fails() {
    // Open, so that a document started after the stop would register and show
    // in `started`, instead of the test waiting for ever.
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(3)));
    let index = index.failing_on("a.md");
    let contents_gate = Arc::new(Semaphore::new(0));
    let contents = ScriptedContents::of(&["a.md", "b.md", "c.md"])
        .holding_calls_after(1, contents_gate.clone());
    let dropped = contents.dropped_before_return.clone();
    let format = Arc::new(TestFormat::new());

    let mut flow = spawn_flow_with(
        instruction(2),
        items_for(&["a.md", "b.md", "c.md"]),
        contents,
        format.clone(),
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    assert!(!flow.is_finished());
    contents_gate.add_permits(1);
    let result = flow.await.unwrap();

    assert!(!dropped.load(Ordering::SeqCst));
    assert!(started.try_recv().is_err());
    // The content that arrived after the stop is not processed.
    let markdown = FileType::for_test_double("markdown");
    assert_eq!(format.extracted(), vec![(content_of("a.md"), markdown)]);
    assert!(matches!(
        result,
        Err(UpdateStopped {
            error: IngestError::Index(_),
            ..
        })
    ));
}

#[tokio::test]
async fn the_pending_next_is_awaited_when_the_index_panics() {
    // Open, as in the test above.
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(3)));
    let index = index.panicking_on("a.md");
    let contents_gate = Arc::new(Semaphore::new(0));
    let contents = ScriptedContents::of(&["a.md", "b.md", "c.md"])
        .holding_calls_after(1, contents_gate.clone());
    let dropped = contents.dropped_before_return.clone();

    let mut flow = spawn_flow(
        instruction(2),
        items_for(&["a.md", "b.md", "c.md"]),
        contents,
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    assert!(!flow.is_finished());
    contents_gate.add_permits(1);
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(started.try_recv().is_err());
}

#[tokio::test]
async fn a_panic_passing_an_outcome_ends_the_stream_and_what_was_started_is_awaited() {
    let gate = Arc::new(Semaphore::new(0));
    let (index, mut started) = GatedIndex::new(gate.clone());
    let index = Arc::new(index);
    let index_dropped = index.dropped_before_return.clone();
    let flow_index: Arc<dyn Index> = index.clone();
    let contents_gate = Arc::new(Semaphore::new(0));
    let mut contents = ScriptedContents::of(&["a.md", "b.md", "c.md"])
        .holding_calls_after(2, contents_gate.clone());
    let contents_dropped = contents.dropped_before_return.clone();
    let (panicked, mut panic_seen) = mpsc::unbounded_channel();

    let mut flow = tokio::spawn(async move {
        let progress = PanickingProgress { panicked };
        ingest_items(
            &instruction(3),
            items_for(&["a.md", "b.md", "c.md"]),
            &mut contents,
            Arc::new(TestFormat::new()),
            flow_index,
            &progress,
        )
        .await
    });
    next_start(&mut started, &mut flow).await;
    gate.add_permits(1);
    panic_seen.recv().await.unwrap();
    assert!(!flow.is_finished());
    contents_gate.add_permits(1);
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert!(!index_dropped.load(Ordering::SeqCst));
    assert!(!contents_dropped.load(Ordering::SeqCst));
    assert_eq!(index.registered().len(), 1);
    assert!(started.try_recv().is_err());
}

#[tokio::test]
async fn a_panic_counting_an_unreadable_document_ends_the_stream() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(2)));
    let flow_index: Arc<dyn Index> = Arc::new(index);
    let mut contents = ScriptedContents::new(vec![Err("broken".into()), Ok(content_of("b.md"))]);
    let (panicked, _panic_seen) = mpsc::unbounded_channel();

    let flow = tokio::spawn(async move {
        let progress = PanickingProgress { panicked };
        ingest_items(
            &instruction(1),
            items_for(&["a.md", "b.md"]),
            &mut contents,
            Arc::new(TestFormat::new()),
            flow_index,
            &progress,
        )
        .await
    });
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert!(started.try_recv().is_err());
}

#[tokio::test]
async fn a_panic_in_extract_reaches_the_caller_after_the_index_has_returned() {
    let gate = Arc::new(Semaphore::new(0));
    let (index, mut started) = GatedIndex::new(gate.clone());
    let index = Arc::new(index.writing_after_the_stream_ends());
    let dropped = index.dropped_before_return.clone();
    let format = Arc::new(TestFormat::new().on_extract(|content, _| {
        if content == content_of("b.md") {
            panic!("extract panicked");
        }
        Ok(whole_text(content))
    }));

    let mut flow = spawn_flow_with(
        instruction(1),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]),
        format,
        index.clone(),
    );
    next_start(&mut started, &mut flow).await;
    // Time for the panic to end the stream. The index has `a.md` to write.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!flow.is_finished());
    gate.add_permits(1);
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert_eq!(index.registered(), vec![Key("a.md".to_owned())]);
    assert!(!dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_panic_in_next_reaches_the_caller_after_the_index_has_returned() {
    let gate = Arc::new(Semaphore::new(0));
    let (index, mut started) = GatedIndex::new(gate.clone());
    let index = Arc::new(index.writing_after_the_stream_ends());
    let dropped = index.dropped_before_return.clone();

    let mut flow = spawn_flow(
        instruction(1),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]).panicking_on_call(1),
        index.clone(),
    );
    next_start(&mut started, &mut flow).await;
    // Time for the panic to end the stream. The index has `a.md` to write.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!flow.is_finished());
    gate.add_permits(1);
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert_eq!(index.registered(), vec![Key("a.md".to_owned())]);
    assert!(!dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_panic_while_calling_next_reaches_the_caller_after_the_index_has_returned() {
    let gate = Arc::new(Semaphore::new(0));
    let (index, mut started) = GatedIndex::new(gate.clone());
    let index = Arc::new(index.writing_after_the_stream_ends());
    let dropped = index.dropped_before_return.clone();
    let contents_gate = Arc::new(Semaphore::new(0));
    let contents = PanickingBeforeTheFuture {
        calls: 0,
        gate: contents_gate.clone(),
    };

    let mut flow = spawn_flow(
        instruction(1),
        items_for(&["a.md", "b.md", "c.md"]),
        contents,
        index.clone(),
    );
    next_start(&mut started, &mut flow).await;
    contents_gate.add_permits(1);
    next_start(&mut started, &mut flow).await;
    gate.add_permits(2);
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
    assert!(!dropped.load(Ordering::SeqCst));
    assert_eq!(index.registered().len(), 2);
}

#[tokio::test]
async fn the_first_panic_is_the_one_passed_on() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(0)));
    let index = index.panicking_on("a.md").writing_after_the_stream_ends();

    let mut flow = spawn_flow(
        instruction(1),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]).panicking_on_call(1),
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    let panic = flow.await.unwrap_err().into_panic();

    let message = panic.downcast_ref::<String>().unwrap();
    assert_eq!(message, "next panicked on call 1");
}

#[tokio::test]
async fn a_panic_of_the_index_comes_before_one_while_waiting_for_what_started() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(0)));
    let index = index.panicking_on("a.md");
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let format = Arc::new(TestFormat::new().on_extract(move |content, _| {
        if content == content_of("b.md") {
            wait_for(&held);
            panic!("extract panicked");
        }
        Ok(whole_text(content))
    }));

    let mut flow = spawn_flow_with(
        instruction(2),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]),
        format,
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    gate.add_permits(1);
    let panic = flow.await.unwrap_err().into_panic();

    let message = panic.downcast_ref::<String>().unwrap();
    assert_eq!(message, "the index panicked for a.md");
}

#[tokio::test]
async fn a_panic_wins_over_an_index_error() {
    let (index, mut started) = GatedIndex::new(Arc::new(Semaphore::new(0)));
    let index = index.failing_on("a.md").writing_after_the_stream_ends();

    let mut flow = spawn_flow(
        instruction(1),
        items_for(&["a.md", "b.md"]),
        ScriptedContents::of(&["a.md", "b.md"]).panicking_on_call(1),
        Arc::new(index),
    );
    next_start(&mut started, &mut flow).await;
    let result = flow.await;

    assert!(result.unwrap_err().is_panic());
}

// --- How the flow runs ---

#[tokio::test]
async fn a_parallelism_of_zero_is_taken_as_one() {
    let index = Arc::new(TestIndex::new());

    let report = ingest_items(
        &instruction(0),
        items_for(&["a.md", "b.md"]),
        &mut ScriptedContents::of(&["a.md", "b.md"]),
        Arc::new(TestFormat::new()),
        index.clone(),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.added, 2);
    assert_eq!(registered_keys(&index), keys(&["a.md", "b.md"]));
}

#[tokio::test]
async fn extract_runs_off_the_thread_that_polls_the_flow() {
    let threads: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = threads.clone();
    let format = Arc::new(TestFormat::new().on_extract(move |content, _| {
        recorded.lock().unwrap().push(std::thread::current().id());
        Ok(whole_text(content))
    }));

    ingest_items(
        &instruction(4),
        items_for(&["a.md"]),
        &mut ScriptedContents::of(&["a.md"]),
        format,
        Arc::new(TestIndex::new()),
        &TestProgressSink::new(),
    )
    .await
    .unwrap();

    let threads = threads.lock().unwrap();
    assert_eq!(threads.len(), 1);
    assert_ne!(threads[0], std::thread::current().id());
}

#[test]
fn the_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let instruction = instruction(1);
    let mut contents = ScriptedContents::of(&[]);
    let progress = TestProgressSink::new();

    let future = ingest_items(
        &instruction,
        items_for(&[]),
        &mut contents,
        Arc::new(TestFormat::new()),
        Arc::new(TestIndex::new()),
        &progress,
    );

    assert_send(&future);
}
