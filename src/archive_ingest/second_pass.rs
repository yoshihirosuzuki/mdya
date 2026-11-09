//! The second pass: hands over the documents' content, one at a time and only
//! when it is asked for.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::sync::Arc;

use async_trait::async_trait;

use super::blocking::run_blocking;
use super::kind::ArchiveKind;
use super::readers::Readers;
use super::stamp::Stamp;
use super::transfer::{Contents, Position, ReadResult};
use crate::ingest::ItemContents;
use crate::types::BoxError;

/// The archive that the first pass opened. It stays open, and is never opened
/// again from its location, so that the content comes from the archive the
/// list was read from, even if another archive is put at the location.
pub(super) struct OpenArchive {
    pub(super) file: File,
    pub(super) stamp: Stamp,
    pub(super) kind: ArchiveKind,
    pub(super) readers: Arc<dyn Readers>,
    pub(super) max_content: u64,
}

impl OpenArchive {
    fn start_reading(&self) -> Result<Box<dyn Contents>, BoxError> {
        self.stamp.confirm(&self.file)?;
        let duplicate = self.readers.clone_file(&self.file)?;
        self.readers
            .contents(self.kind, duplicate, self.max_content)
    }
}

/// An entry that the second pass is to read, as the first pass saw it.
pub(super) struct ExpectedEntry {
    pub(super) position: Position,
    pub(super) name: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
#[error("the entry at this position is not the one in the list of entries")]
struct NotTheListedEntry;

/// One cause, given to every document that it applies to.
#[derive(Clone, Debug)]
struct SharedCause(Arc<dyn Error + Send + Sync>);

impl From<BoxError> for SharedCause {
    fn from(cause: BoxError) -> Self {
        Self(Arc::from(cause))
    }
}

impl fmt::Display for SharedCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for SharedCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

/// The state of the second pass, which moves to a blocking thread for each
/// document.
pub(super) struct SecondPass {
    archive: OpenArchive,
    expected: VecDeque<ExpectedEntry>,
    /// `None` until the first document is asked for. `Some(Err(_))` once
    /// nothing more can be read: every document left gets that cause.
    reader: Option<Result<Box<dyn Contents>, SharedCause>>,
}

impl SecondPass {
    pub(super) fn new(archive: OpenArchive, expected: VecDeque<ExpectedEntry>) -> Self {
        Self {
            archive,
            expected,
            reader: None,
        }
    }

    fn next_content(&mut self) -> Option<Result<Vec<u8>, BoxError>> {
        let expected = self.expected.pop_front()?;
        Some(self.content_of(&expected))
    }

    fn content_of(&mut self, expected: &ExpectedEntry) -> Result<Vec<u8>, BoxError> {
        let reader = self
            .reader
            .get_or_insert_with(|| self.archive.start_reading().map_err(SharedCause::from));
        let result = match reader {
            Ok(reader) => reader.read(expected.position),
            Err(cause) => return Err(Box::new(cause.clone())),
        };
        // Checked after reading, so that a change made during the read is
        // caught before the content is handed over.
        if let Err(cause) = self.archive.stamp.confirm(&self.archive.file) {
            return Err(self.give_up(cause));
        }
        match result {
            ReadResult::RestUnreadable(cause) => Err(self.give_up(cause)),
            ReadResult::Entry {
                name,
                regular,
                content,
            } if regular && name == expected.name => content,
            ReadResult::Entry { .. } => Err(Box::new(NotTheListedEntry)),
        }
    }

    fn give_up(&mut self, cause: BoxError) -> BoxError {
        let cause = SharedCause::from(cause);
        self.reader = Some(Err(cause.clone()));
        Box::new(cause)
    }
}

/// Hands the second pass's results to the shared ingest flow. Each document is
/// read in a blocking task of its own, which the call waits for.
pub(super) struct ArchiveContents {
    /// `None` while a task has it, and from then on if that task panicked.
    pass: Option<SecondPass>,
}

impl ArchiveContents {
    pub(super) fn new(pass: SecondPass) -> Self {
        Self { pass: Some(pass) }
    }
}

#[async_trait]
impl ItemContents for ArchiveContents {
    async fn next(&mut self) -> Option<Result<Vec<u8>, BoxError>> {
        let mut pass = self.pass.take()?;
        let (pass, content) = run_blocking(move || {
            let content = pass.next_content();
            (pass, content)
        })
        .await;
        self.pass = Some(pass);
        content
    }
}
