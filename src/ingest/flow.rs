//! The ingest flow shared by the implementations that walk a source
//! (`file_ingest` and `archive_ingest`).

use std::any::Any;
use std::collections::HashSet;
use std::error::Error;
use std::future::{Future, poll_fn};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;

use futures::FutureExt;
use futures::stream::{self, FuturesUnordered, Stream, StreamExt};
use tokio::task::JoinError;

use super::{
    IngestError, IngestItem, IngestItems, ItemContents, UpdateInstruction, UpdateReport,
    UpdateStopped,
};
use crate::format::{Extracted, FileType, Format, FormatError};
use crate::index::{DocumentToRegister, Index, IndexError, PartialDelete};
use crate::types::{BoxError, DocumentOutcome, Key, Progress, ProgressSink};

#[cfg(test)]
mod tests;

/// The ingest flow shared by the implementations that walk a source.
///
/// For each item with a key, in order, it takes the content from `contents`,
/// has `format` extract the body and chunks (on a blocking thread, since
/// extracting can take long), and has `index` register the document, with
/// `progress`. Up to `instruction.parallelism` documents (at least one) are in
/// process at once, and the next content is asked for only when one more can
/// start. A document whose content cannot be read, whose body cannot be
/// extracted, or whose content `contents` did not hand over before it ended is
/// warned about, counted as failed, and skipped, as is an item without a key.
/// When `items` asks for it, the documents of the collection other than the
/// items with a key, and other than those under the unreadable directories,
/// are deleted at the end.
///
/// When `index` fails, no further document starts, and the error is returned
/// with the counts once the documents already in process end. A panic, in the
/// flow or in anything it calls, stops the flow the same way and is passed on
/// once those documents end. A call to `contents.next` is always awaited until
/// it returns, even when stopping.
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
    let documents = Documents {
        instruction,
        format,
        index: index.as_ref(),
        progress,
    };
    if let Some(error) = documents.process(items, contents, &mut tally).await {
        return Err(tally.stopped(error));
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

/// How one document in process ended.
enum Finished {
    Registered(DocumentOutcome),
    /// Its body could not be extracted.
    Unreadable {
        key: Key,
        cause: FormatError,
    },
    IndexFailed(IndexError),
}

/// Why the flow stops before the end. A panic wins over an error, since it
/// has to reach the caller.
enum Stop {
    Error(IngestError),
    Panic(Panic),
}

/// What the flow waits for next.
enum Event {
    Content(IngestItem, NextContent),
    Finished(Result<Finished, Panic>),
}

#[derive(Debug, thiserror::Error)]
#[error("the ingest implementation handed over no content for it")]
struct NoContent;

/// What processing the documents needs.
struct Documents<'a> {
    instruction: &'a UpdateInstruction,
    format: Arc<dyn Format>,
    index: &'a dyn Index,
    progress: &'a dyn ProgressSink,
}

impl Documents<'_> {
    /// Processes the items with a key. Returns the error that stopped it, if
    /// any, after the documents in process have finished.
    async fn process(
        &self,
        items: Vec<IngestItem>,
        contents: &mut dyn ItemContents,
        tally: &mut Tally<'_>,
    ) -> Option<IngestError> {
        // A call to `next` in progress lives inside the stream, so that it
        // can be polled until it returns across the loop's turns. The call
        // itself is inside `catch_unwind` too, since an implementation may
        // panic before it returns its future.
        let mut contents = pin!(stream::unfold(contents, |contents| async move {
            let next = AssertUnwindSafe(async { contents.next().await })
                .catch_unwind()
                .await;
            Some((next, contents))
        }));
        let parallelism = self.instruction.parallelism.max(1);
        let mut waiting = items.into_iter();
        let mut in_process = FuturesUnordered::new();
        let mut asked: Option<IngestItem> = None;
        let mut stop: Option<Stop> = None;
        loop {
            if stop.is_none() && asked.is_none() && in_process.len() < parallelism {
                asked = waiting.next();
            }
            if asked.is_none() && in_process.is_empty() {
                break;
            }
            let event = poll_fn(|context| {
                if asked.is_some()
                    && let Poll::Ready(Some(next)) = contents.as_mut().poll_next(context)
                    && let Some(item) = asked.take()
                {
                    return Poll::Ready(Event::Content(item, next));
                }
                match in_process.poll_next_unpin(context) {
                    Poll::Ready(Some(finished)) => Poll::Ready(Event::Finished(finished)),
                    _ => Poll::Pending,
                }
            })
            .await;
            // Reporting progress and logging can panic too. Such a panic
            // stops the flow like the others, so that the documents in
            // process and a pending `next` are not dropped.
            let handled = catch_unwind(AssertUnwindSafe(|| {
                self.handle(event, &mut waiting, &mut stop, tally)
            }));
            match handled {
                Ok(Some(document)) => in_process.push(document),
                Ok(None) => {}
                Err(panic) => stop_with_panic(&mut stop, panic),
            }
        }
        match stop {
            None => None,
            Some(Stop::Error(error)) => Some(error),
            Some(Stop::Panic(panic)) => resume_unwind(panic),
        }
    }

    /// Handles what the flow waited for. Returns the document to process, if
    /// any.
    fn handle(
        &self,
        event: Event,
        waiting: &mut std::vec::IntoIter<IngestItem>,
        stop: &mut Option<Stop>,
        tally: &mut Tally<'_>,
    ) -> Option<impl Future<Output = Result<Finished, Panic>> + Send + '_> {
        match event {
            Event::Content(item, next) => self.take_content(item, next, waiting, stop, tally),
            Event::Finished(finished) => {
                self.finish(finished, stop, tally);
                None
            }
        }
    }

    /// Handles the content handed over for `item`. Returns the document to
    /// process, if any. Once stopped, what arrives is not processed.
    fn take_content(
        &self,
        item: IngestItem,
        next: NextContent,
        waiting: &mut std::vec::IntoIter<IngestItem>,
        stop: &mut Option<Stop>,
        tally: &mut Tally<'_>,
    ) -> Option<impl Future<Output = Result<Finished, Panic>> + Send + '_> {
        let collection = self.instruction.collection.0.as_str();
        match next {
            Err(panic) => stop_with_panic(stop, panic),
            Ok(_) if stop.is_some() => {}
            Ok(None) => {
                for item in std::iter::once(item).chain(waiting) {
                    warn_unreadable(collection, &item.key, &NoContent);
                    tally.failed();
                }
            }
            Ok(Some(Err(cause))) => {
                warn_unreadable(collection, &item.key, cause.as_ref());
                tally.failed();
            }
            Ok(Some(Ok(content))) => return Some(self.document(item, content)),
        }
        None
    }

    /// Extracts and registers one document.
    fn document(
        &self,
        item: IngestItem,
        content: Vec<u8>,
    ) -> impl Future<Output = Result<Finished, Panic>> + Send + '_ {
        let format = self.format.clone();
        let document = async move {
            let extracted = match extract(format, content, item.file_type).await {
                Ok(extracted) => extracted,
                Err(cause) => {
                    return Finished::Unreadable {
                        key: item.key,
                        cause,
                    };
                }
            };
            let document = DocumentToRegister {
                collection: self.instruction.collection.clone(),
                key: item.key,
                time: Some(item.time),
                body: extracted.body,
                chunks: extracted.chunks,
                overwrite: self.instruction.overwrite,
            };
            match self.index.register(document, Some(self.progress)).await {
                Ok(outcome) => Finished::Registered(outcome),
                Err(error) => Finished::IndexFailed(error),
            }
        };
        AssertUnwindSafe(document).catch_unwind()
    }

    /// Counts a document that finished, or notes why the flow stops.
    fn finish(
        &self,
        finished: Result<Finished, Panic>,
        stop: &mut Option<Stop>,
        tally: &mut Tally<'_>,
    ) {
        match finished {
            Err(panic) => stop_with_panic(stop, panic),
            Ok(Finished::Registered(outcome)) => tally.count(outcome),
            Ok(Finished::Unreadable { key, cause }) => {
                warn_unreadable(self.instruction.collection.0.as_str(), &key, &cause);
                tally.failed();
            }
            Ok(Finished::IndexFailed(error)) => {
                if stop.is_none() {
                    *stop = Some(Stop::Error(IngestError::Index(error)));
                }
            }
        }
    }
}

fn stop_with_panic(stop: &mut Option<Stop>, panic: Panic) {
    if !matches!(stop, Some(Stop::Panic(_))) {
        *stop = Some(Stop::Panic(panic));
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
