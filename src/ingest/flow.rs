//! The ingest flow shared by the implementations that walk a source
//! (`file_ingest` and `archive_ingest`).

use std::any::Any;
use std::collections::HashSet;
use std::error::Error;
use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, FuturesUnordered, StreamExt};
use tokio::task::JoinError;

use super::{
    IngestError, IngestItem, IngestItems, ItemContents, UpdateInstruction, UpdateReport,
    UpdateStopped,
};
use crate::format::{Extracted, FileType, Format, FormatError};
use crate::index::{DocumentToRegister, Index, IndexError, PartialDelete, RegistrationSink};
use crate::types::{BoxError, CollectionName, DocumentOutcome, Key, Progress, ProgressSink};

#[cfg(test)]
mod tests;

/// The ingest flow shared by the implementations that walk a source.
///
/// It has `index` register the documents of the items with a key, passed as
/// one stream, with `progress`. For each item, in order, the stream takes the
/// content from `contents` and has `format` extract the body and chunks (on a
/// blocking thread, since extracting can take long). `index` asks for the next
/// document when it needs one. Up to `instruction.parallelism` documents (at
/// least one) are in process at once, from the content's arrival until the
/// document is passed to `index`, and the next content is asked for only when
/// one more can start. A document whose content cannot be read, whose body
/// cannot be extracted, or whose content `contents` did not hand over before
/// it ended is warned about, counted as failed, and skipped, as is an item
/// without a key. The others are counted when `index` passes their outcomes,
/// that is, once they are written. When `items` asks for it, the documents of
/// the collection other than the items with a key, and other than those under
/// the unreadable directories, are deleted at the end.
///
/// When `index` stops, the documents not yet passed to it are not passed, and
/// its error is returned with the counts once the documents in process end. A
/// panic in `index` is passed on the same way. A panic on this side (in
/// `contents`, `format`, reporting progress, or logging) ends the stream, and
/// is passed on once `index` has returned and the documents in process end. A
/// call to `contents.next` is always awaited until it returns, even when
/// stopping.
pub async fn ingest_items(
    instruction: &UpdateInstruction,
    items: IngestItems,
    contents: &mut dyn ItemContents,
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
    progress: &dyn ProgressSink,
) -> Result<UpdateReport, UpdateStopped> {
    let IngestItems {
        items,
        unreadable_without_key,
        delete_except_unreadable_dirs,
        unreadable_places,
    } = items;
    let total = (items.len() + unreadable_without_key.len()) as u64;
    let mut tally = Tally::start(total, unreadable_places, progress);
    let collection = instruction.collection.0.as_str();
    for (name, cause) in &unreadable_without_key {
        warn_unreadable_without_key(collection, name, cause.as_ref());
        tally.failed();
    }
    let keep: HashSet<Key> = items.iter().map(|item| item.key.clone()).collect();
    let shared = Shared::new(tally);
    let mut documents = Documents::new(instruction, items, contents, format, &shared);
    let registered = documents.register_with(index.as_ref(), progress).await;
    drop(documents);
    let mut tally = shared.finish();
    if let Err(error) = registered {
        return Err(tally.stopped(IngestError::Index(error)));
    }
    if let Some(unreadable_dirs) = delete_except_unreadable_dirs {
        let deleted = index
            .delete_others(&instruction.collection, &keep, &unreadable_dirs)
            .await;
        if let Some(error) = tally.add_deleted(deleted) {
            return Err(tally.stopped(error));
        }
    }
    Ok(tally.counts)
}

/// The counts so far, reported as progress at the start and whenever a
/// document is counted.
struct Tally<'a> {
    counts: UpdateReport,
    done: u64,
    total: u64,
    progress: &'a dyn ProgressSink,
}

impl<'a> Tally<'a> {
    fn start(total: u64, unreadable_places: u64, progress: &'a dyn ProgressSink) -> Self {
        let tally = Self {
            counts: UpdateReport {
                unreadable_places,
                ..UpdateReport::default()
            },
            done: 0,
            total,
            progress,
        };
        tally.report();
        tally
    }

    fn count(&mut self, outcome: DocumentOutcome) {
        let count = match outcome {
            DocumentOutcome::Added => &mut self.counts.added,
            DocumentOutcome::Changed => &mut self.counts.changed,
            DocumentOutcome::Unchanged => &mut self.counts.unchanged,
            DocumentOutcome::Reingested => &mut self.counts.reingested,
        };
        *count += 1;
        self.finish_one();
    }

    fn failed(&mut self) {
        self.counts.failed += 1;
        self.finish_one();
    }

    fn finish_one(&mut self) {
        self.done += 1;
        self.report();
    }

    fn report(&self) {
        self.progress.report(Progress {
            done: self.done,
            total: Some(self.total),
        });
    }

    /// Adds the documents deleted, also those deleted before a failure.
    /// Returns the error that stopped the deletion, if any.
    fn add_deleted(&mut self, deleted: Result<u64, PartialDelete>) -> Option<IngestError> {
        match deleted {
            Ok(deleted) => {
                self.counts.deleted += deleted;
                None
            }
            Err(PartialDelete { deleted, error }) => {
                self.counts.deleted += deleted;
                Some(IngestError::Index(error))
            }
        }
    }

    fn stopped(&self, error: IngestError) -> UpdateStopped {
        UpdateStopped {
            counts: self.counts,
            error,
        }
    }
}

type Panic = Box<dyn Any + Send>;

/// The result of `contents.next`, or the panic it raised.
type NextContent = Result<Option<Result<Vec<u8>, BoxError>>, Panic>;

/// How preparing one document in process ended.
enum Extraction {
    Document(DocumentToRegister),
    /// Its body could not be extracted.
    Unreadable {
        key: Key,
        cause: FormatError,
    },
}

/// What the stream of documents waits for next.
enum Event {
    Content(IngestItem, NextContent),
    Extraction(Result<Extraction, Panic>),
}

#[derive(Debug, thiserror::Error)]
#[error("the ingest implementation handed over no content for it")]
struct NoContent;

/// What the stream of documents and `index` both reach: `index` passes the
/// outcomes through a shared reference while it holds the stream.
struct Shared<'p> {
    tally: Mutex<Tally<'p>>,
    /// The first panic, which has to reach the caller and wins over an error.
    panic: Mutex<Option<Panic>>,
}

impl<'p> Shared<'p> {
    fn new(tally: Tally<'p>) -> Self {
        Self {
            tally: Mutex::new(tally),
            panic: Mutex::new(None),
        }
    }

    fn tally(&self) -> MutexGuard<'_, Tally<'p>> {
        lock(&self.tally)
    }

    fn note_panic(&self, panic: Panic) {
        lock(&self.panic).get_or_insert(panic);
    }

    fn panicked(&self) -> bool {
        lock(&self.panic).is_some()
    }

    /// The counts, unless a panic has to reach the caller instead.
    fn finish(self) -> Tally<'p> {
        let panic = self.panic.into_inner();
        if let Some(panic) = panic.unwrap_or_else(PoisonError::into_inner) {
            resume_unwind(panic);
        }
        let tally = self.tally.into_inner();
        tally.unwrap_or_else(PoisonError::into_inner)
    }
}

impl RegistrationSink for Shared<'_> {
    /// A panic while counting is one on this side, so it ends the stream
    /// rather than unwinding through `index`, which writes what it has
    /// finished before it returns.
    fn registered(&self, _collection: &CollectionName, _key: &Key, outcome: DocumentOutcome) {
        let counted = catch_unwind(AssertUnwindSafe(|| self.tally().count(outcome)));
        if let Err(panic) = counted {
            self.note_panic(panic);
        }
    }
}

/// A panic while counting poisons the lock. The flow carries on, to pass that
/// panic to the caller once what it started has ended.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The documents of the items with a key, prepared as `index` asks for them.
/// Everything under way is kept here rather than in the stream's future, so
/// that it can still be waited for after `index` has dropped the stream.
struct Documents<'a, 'p> {
    instruction: &'a UpdateInstruction,
    format: Arc<dyn Format>,
    shared: &'a Shared<'p>,
    contents: BoxStream<'a, NextContent>,
    waiting: std::vec::IntoIter<IngestItem>,
    /// The item whose content has been asked for and has not arrived.
    asked: Option<IngestItem>,
    in_process: FuturesUnordered<BoxFuture<'a, Result<Extraction, Panic>>>,
    parallelism: usize,
    index_returned: bool,
}

impl<'a, 'p> Documents<'a, 'p> {
    fn new(
        instruction: &'a UpdateInstruction,
        items: Vec<IngestItem>,
        contents: &'a mut dyn ItemContents,
        format: Arc<dyn Format>,
        shared: &'a Shared<'p>,
    ) -> Self {
        // A call to `next` in progress lives inside the stream, so that it
        // can be polled until it returns, also after `index` has returned.
        // The call itself is inside `catch_unwind` too, since an
        // implementation may panic before it returns its future.
        let contents = stream::unfold(contents, |contents| async move {
            let next = AssertUnwindSafe(async { contents.next().await })
                .catch_unwind()
                .await;
            Some((next, contents))
        });
        Self {
            instruction,
            format,
            shared,
            contents: contents.boxed(),
            waiting: items.into_iter(),
            asked: None,
            in_process: FuturesUnordered::new(),
            parallelism: instruction.parallelism.max(1),
            index_returned: false,
        }
    }

    /// Has `index` register the documents as one stream. Returns its result
    /// after the documents in process and a pending `next` have ended. A panic
    /// of `index` is kept in `shared`, for the caller to pass on.
    async fn register_with(
        &mut self,
        index: &dyn Index,
        progress: &dyn ProgressSink,
    ) -> Result<(), IndexError> {
        let parallelism = self.parallelism;
        let shared = self.shared;
        let documents = stream::unfold(&mut *self, |documents| async move {
            let document = documents.next_document().await?;
            Some((document, documents))
        });
        let documents = documents.boxed();
        // Inside `catch_unwind` too, since an implementation may panic before
        // it returns its future.
        let registered = AssertUnwindSafe(async move {
            index
                .register_stream(documents, parallelism, shared, Some(progress))
                .await
        })
        .catch_unwind()
        .await;
        // Before waiting, since what is waited for can panic too, and the
        // first panic is the one to pass on.
        let registered = registered.unwrap_or_else(|panic| {
            shared.note_panic(panic);
            Ok(())
        });
        self.wait_for_what_started().await;
        registered
    }

    /// The next document to pass to `index`. `None` ends the stream: after
    /// the last item, or at once after a panic on this side.
    async fn next_document(&mut self) -> Option<DocumentToRegister> {
        while !self.shared.panicked() {
            if self.asked.is_none() && self.in_process.len() < self.parallelism {
                self.asked = self.waiting.next();
            }
            if self.asked.is_none() && self.in_process.is_empty() {
                return None;
            }
            let event = self.next_event().await;
            let document = self.handle(event);
            if document.is_some() {
                return document;
            }
        }
        None
    }

    /// Waits for the documents in process and a pending `next`, passing on
    /// nothing, so that neither is dropped before it ends.
    async fn wait_for_what_started(&mut self) {
        self.index_returned = true;
        while self.asked.is_some() || !self.in_process.is_empty() {
            let event = self.next_event().await;
            self.handle(event);
        }
    }

    async fn next_event(&mut self) -> Event {
        poll_fn(|context| {
            if self.asked.is_some()
                && let Poll::Ready(Some(next)) = self.contents.poll_next_unpin(context)
                && let Some(item) = self.asked.take()
            {
                return Poll::Ready(Event::Content(item, next));
            }
            match self.in_process.poll_next_unpin(context) {
                Poll::Ready(Some(extraction)) => Poll::Ready(Event::Extraction(extraction)),
                _ => Poll::Pending,
            }
        })
        .await
    }

    /// Handles what the stream waited for. Returns the document to pass to
    /// `index`, if any. Reporting progress and logging can panic too. Such a
    /// panic stops the flow like the others, so that the documents in process
    /// and a pending `next` are not dropped.
    fn handle(&mut self, event: Event) -> Option<DocumentToRegister> {
        let handled = catch_unwind(AssertUnwindSafe(|| match event {
            Event::Content(item, next) => {
                self.take_content(item, next);
                None
            }
            Event::Extraction(extraction) => self.finish_extraction(extraction),
        }));
        handled.unwrap_or_else(|panic| {
            self.shared.note_panic(panic);
            None
        })
    }

    fn stopping(&self) -> bool {
        self.index_returned || self.shared.panicked()
    }

    /// Handles the content handed over for `item`. Once stopping, what arrives
    /// is not processed.
    fn take_content(&mut self, item: IngestItem, next: NextContent) {
        match next {
            Err(panic) => self.shared.note_panic(panic),
            Ok(_) if self.stopping() => {}
            Ok(None) => self.fail_with_the_rest(item),
            Ok(Some(Err(cause))) => self.fail(&item.key, cause.as_ref()),
            Ok(Some(Ok(content))) => self.in_process.push(self.extraction(item, content)),
        }
    }

    fn fail_with_the_rest(&mut self, item: IngestItem) {
        let mut items = vec![item];
        items.extend(self.waiting.by_ref());
        for item in items {
            self.fail(&item.key, &NoContent);
        }
    }

    fn fail(&self, key: &Key, cause: &(dyn Error + 'static)) {
        warn_unreadable(self.instruction.collection.0.as_str(), key, cause);
        self.shared.tally().failed();
    }

    /// Extracts the body and chunks of one document.
    fn extraction(
        &self,
        item: IngestItem,
        content: Vec<u8>,
    ) -> BoxFuture<'a, Result<Extraction, Panic>> {
        let format = self.format.clone();
        let instruction = self.instruction;
        let extraction = async move {
            match extract(format, content, item.file_type).await {
                Ok(extracted) => Extraction::Document(DocumentToRegister {
                    collection: instruction.collection.clone(),
                    key: item.key,
                    time: Some(item.time),
                    body: extracted.body,
                    chunks: extracted.chunks,
                    overwrite: instruction.overwrite,
                }),
                Err(cause) => Extraction::Unreadable {
                    key: item.key,
                    cause,
                },
            }
        };
        AssertUnwindSafe(extraction).catch_unwind().boxed()
    }

    /// Returns the document to pass to `index`, if any. Once stopping, a
    /// document that is ready is not passed.
    fn finish_extraction(
        &self,
        extraction: Result<Extraction, Panic>,
    ) -> Option<DocumentToRegister> {
        match extraction {
            Err(panic) => {
                self.shared.note_panic(panic);
                None
            }
            Ok(Extraction::Unreadable { key, cause }) => {
                self.fail(&key, &cause);
                None
            }
            Ok(Extraction::Document(_)) if self.stopping() => None,
            Ok(Extraction::Document(document)) => Some(document),
        }
    }
}

/// Extracts on a blocking thread and waits for it, so that a long extraction
/// does not hold up the other async work, and the thread's panic reaches the
/// caller as a panic.
async fn extract(
    format: Arc<dyn Format>,
    content: Vec<u8>,
    file_type: FileType,
) -> Result<Extracted, FormatError> {
    let task = tokio::task::spawn_blocking(move || format.extract(&content, &file_type));
    match task.await {
        Ok(extracted) => extracted,
        Err(error) => resume_panic(error),
    }
}

#[expect(
    clippy::panic,
    reason = "a blocking task that is awaited ends without a panic only when the runtime shuts down, which the flow cannot carry on from"
)]
fn resume_panic(error: JoinError) -> ! {
    match error.try_into_panic() {
        Ok(panic) => resume_unwind(panic),
        Err(error) => panic!(
            "the extraction was cancelled, which happens only when the runtime shuts down: {error}"
        ),
    }
}

fn warn_unreadable(collection: &str, key: &Key, cause: &(dyn Error + 'static)) {
    tracing::warn!(
        collection,
        key = key.0.as_str(),
        cause = causes(cause).as_str(),
        "a document cannot be read and was skipped"
    );
}

fn warn_unreadable_without_key(collection: &str, name: &str, cause: &(dyn Error + 'static)) {
    tracing::warn!(
        collection,
        name,
        cause = causes(cause).as_str(),
        "a document without a key cannot be read and was skipped"
    );
}

/// The error and its sources, joined with `: `.
fn causes(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}
