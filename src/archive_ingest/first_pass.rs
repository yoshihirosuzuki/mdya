//! The first pass: reads the list of entries and works out what to ingest.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use super::Limits;
use super::kind::ArchiveKind;
use super::names::{EntryRole, role_of};
use super::readers::Readers;
use super::second_pass::{ExpectedEntry, OpenArchive, SecondPass};
use super::stamp::Stamp;
use super::transfer::{Entry, ListError, ListSize, ListSizeExceeded, Position};
use crate::format::{FileType, Format};
use crate::ingest::{IngestItem, IngestItems};
use crate::types::{BoxError, Key};

/// About what a document takes in memory besides its name, in bytes.
const MEMORY_PER_DOCUMENT: u64 = 128;

/// What the first pass needs. It is moved to a blocking thread.
pub(super) struct FirstPass {
    pub(super) path: PathBuf,
    pub(super) kind: ArchiveKind,
    pub(super) readers: Arc<dyn Readers>,
    pub(super) format: Arc<dyn Format>,
    pub(super) limits: Limits,
}

/// What the first pass found out, and how to go on to the second.
pub(super) struct Listed {
    pub(super) items: IngestItems,
    pub(super) second_pass: SecondPass,
}

#[derive(Debug, thiserror::Error)]
#[error("the list of entries of the archive is over its size limit")]
struct ListTooLarge;

impl FirstPass {
    /// Reads the list. An error means that the source cannot be read.
    pub(super) fn run(self) -> Result<Listed, BoxError> {
        let file = File::open(&self.path)?;
        let stamp = Stamp::of(&file)?;
        let duplicate = self.readers.clone_file(&file)?;
        let mut listing = self.readers.listing(self.kind, duplicate);
        let mut size = ListSize::new(self.limits.list);
        let mut found = Found::default();
        while let Some(entry) = listing.next(&mut size).map_err(cause_of)? {
            found.take(entry, self.format.as_ref(), &mut size)?;
        }
        // A list read while the archive was being rewritten may be missing
        // entries, and the documents not in the list would be deleted.
        stamp.confirm(&file)?;
        Ok(found.into_listed(OpenArchive {
            file,
            stamp,
            kind: self.kind,
            readers: self.readers,
            max_content: self.limits.content,
        }))
    }
}

fn cause_of(error: ListError) -> BoxError {
    match error {
        ListError::SourceUnreadable(cause) => cause,
        ListError::ListSizeExceeded => Box::new(ListTooLarge),
    }
}

struct Document {
    position: Position,
    name: Vec<u8>,
    key: Key,
    file_type: FileType,
}

/// The entries that become documents, in the order of the list.
#[derive(Default)]
struct Found {
    documents: Vec<Document>,
    without_key: Vec<(String, BoxError)>,
}

impl Found {
    /// Takes in an entry, whose name the reader has already counted in `size`.
    fn take(
        &mut self,
        entry: Entry,
        format: &dyn Format,
        size: &mut ListSize,
    ) -> Result<(), ListTooLarge> {
        match role_of(&entry.name, entry.regular, format) {
            EntryRole::NotADocument => {
                size.sub(entry.name.len() as u64);
                return Ok(());
            }
            EntryRole::WithoutKey { name, cause } => self.without_key.push((name, cause)),
            EntryRole::Document { key, file_type } => self.documents.push(Document {
                position: entry.position,
                name: entry.name,
                key,
                file_type,
            }),
        }
        size.add(MEMORY_PER_DOCUMENT)
            .map_err(|ListSizeExceeded| ListTooLarge)
    }

    fn into_listed(mut self, archive: OpenArchive) -> Listed {
        self.keep_the_last_of_each_key();
        let time = archive.stamp.modified();
        let (items, expected): (Vec<IngestItem>, VecDeque<ExpectedEntry>) = self
            .documents
            .into_iter()
            .map(|document| {
                let item = IngestItem {
                    key: document.key,
                    time,
                    file_type: document.file_type,
                };
                let expected = ExpectedEntry {
                    position: document.position,
                    name: document.name,
                };
                (item, expected)
            })
            .unzip();
        Listed {
            items: IngestItems {
                items,
                unreadable_without_key: self.without_key,
                delete_except_unreadable_dirs: Some(Vec::new()),
                unreadable_places: 0,
            },
            second_pass: SecondPass::new(archive, expected),
        }
    }

    /// Of the documents with the same key, keeps the last in the archive: the
    /// one that would be left after unpacking it.
    fn keep_the_last_of_each_key(&mut self) {
        let mut is_last = is_last_of_its_key(&self.documents).into_iter();
        self.documents.retain(|_| is_last.next().unwrap_or(true));
    }
}

fn is_last_of_its_key(documents: &[Document]) -> Vec<bool> {
    let mut seen = HashSet::with_capacity(documents.len());
    let mut is_last: Vec<bool> = documents
        .iter()
        .rev()
        .map(|document| seen.insert(&document.key))
        .collect();
    is_last.reverse();
    is_last
}
