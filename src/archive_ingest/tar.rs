//! Reads tar files, plain or compressed with gzip.

mod gzip;
mod header;
mod items;
mod numbers;
mod records;
mod source;
mod sparse;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::fs::File;
use std::io;

use self::items::{ContentFailure, Item, Items};
use super::transfer::{
    Contents, Entry, ListError, ListSize, ListSizeExceeded, Listing, Position, ReadResult,
};
use crate::types::BoxError;

/// The most bytes of names the second pass holds for one item. It has no
/// list to count against; this is the list's own limit, which every name
/// passed on the first pass.
const MAX_NAME_BYTES_ON_SECOND_PASS: u64 = 256 * 1024 * 1024;

/// Why a walk through the tar cannot go on.
#[derive(Debug)]
enum Stop {
    Unreadable(BoxError),
    ListTooLarge,
}

impl Stop {
    fn unreadable(cause: impl Into<BoxError>) -> Self {
        Self::Unreadable(cause.into())
    }

    fn into_cause(self) -> BoxError {
        match self {
            Self::Unreadable(cause) => cause,
            Self::ListTooLarge => "an item's names are larger than any list may hold".into(),
        }
    }
}

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Self {
        Self::Unreadable(Box::new(error))
    }
}

impl From<ListSizeExceeded> for Stop {
    fn from(_: ListSizeExceeded) -> Self {
        Self::ListTooLarge
    }
}

impl From<Stop> for ListError {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Unreadable(cause) => Self::SourceUnreadable(cause),
            Stop::ListTooLarge => Self::ListSizeExceeded,
        }
    }
}

/// Starts the first pass over a tar file. `gzip` says whether it is compressed
/// with gzip.
pub(super) fn tar_listing(file: File, gzip: bool) -> Box<dyn Listing> {
    Box::new(TarListing {
        unopened: Some((file, gzip)),
        items: None,
    })
}

struct TarListing {
    /// Opened on the first call to `next`, which can report a failure.
    unopened: Option<(File, bool)>,
    /// `None` once the list has been read to its end.
    items: Option<Items>,
}

impl Listing for TarListing {
    fn next(&mut self, size: &mut ListSize) -> Result<Option<Entry>, ListError> {
        if let Some((file, gzip)) = self.unopened.take() {
            self.items = Some(Items::new(source::open(file, gzip).map_err(Stop::from)?));
        }
        let Some(items) = &mut self.items else {
            return Ok(None);
        };
        let Some(item) = items.next_item(size)? else {
            self.items = None;
            return Ok(None);
        };
        let regular = item.is_regular();
        Ok(Some(Entry {
            name: item.name,
            regular,
            position: Position(item.at),
        }))
    }
}

/// Starts the second pass over a tar file. `gzip` says whether it is compressed
/// with gzip. No item's content may grow beyond `max_content`.
pub(super) fn tar_contents(
    file: File,
    gzip: bool,
    max_content: u64,
) -> Result<Box<dyn Contents>, BoxError> {
    let items = Items::new(source::open(file, gzip)?);
    Ok(Box::new(TarContents { items, max_content }))
}

struct TarContents {
    items: Items,
    max_content: u64,
}

impl TarContents {
    /// Walks forward to the item whose header is at `position`.
    fn find(&mut self, position: Position) -> Result<Item, BoxError> {
        loop {
            let mut names = ListSize::new(MAX_NAME_BYTES_ON_SECOND_PASS);
            let item = self
                .items
                .next_item(&mut names)
                .map_err(Stop::into_cause)?
                .ok_or("the tar ends before the position of an item it listed")?;
            match item.at.cmp(&position.0) {
                Ordering::Less => {}
                Ordering::Equal => return Ok(item),
                Ordering::Greater => return Err("the tar has no item at a listed position".into()),
            }
        }
    }

    fn read_at(&mut self, position: Position) -> Result<ReadResult, BoxError> {
        let item = self.find(position)?;
        let regular = item.is_regular();
        let content = match self.items.read_content(item.layout, self.max_content) {
            Ok(content) => Ok(content),
            Err(ContentFailure::Document(cause)) => Err(cause),
            Err(ContentFailure::Rest(cause)) => return Err(cause),
        };
        Ok(ReadResult::Entry {
            name: item.name,
            regular,
            content,
        })
    }
}

impl Contents for TarContents {
    fn read(&mut self, position: Position) -> ReadResult {
        self.read_at(position)
            .unwrap_or_else(ReadResult::RestUnreadable)
    }
}
