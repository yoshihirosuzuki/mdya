//! Reads zip files. The central directory, the end records, and the local file
//! headers are read here, following PKWARE's APPNOTE.TXT; only the deflate data
//! and the CRC-32 are left to flate2.

// The driver is the only user of these, and it is not written yet.
#![allow(dead_code)]

use std::fs::File;

use self::content::read_content;
use self::directory::{Located, Walk};
use self::header::read_header;
use self::source::Source;
use super::transfer::{Contents, Entry, ListError, ListSize, Listing, Position, ReadResult};
use crate::types::BoxError;

mod content;
mod directory;
mod end_record;
mod header;
mod source;

#[cfg(test)]
mod test_zip;
#[cfg(test)]
mod tests;

/// Starts the first pass over a zip file.
pub(super) fn zip_listing(file: File) -> Box<dyn Listing> {
    Box::new(ZipListing {
        source: Source::new(file),
        stage: Stage::NotStarted,
    })
}

/// Starts the second pass over a zip file, after checking it again. No item's
/// content may grow beyond `max_content` when it is inflated.
pub(super) fn zip_contents(file: File, max_content: u64) -> Result<Box<dyn Contents>, BoxError> {
    let mut source = Source::new(file);
    let mut walk = Walk::start(&mut source)?;
    while walk.next_header(&mut source)?.is_some() {}
    let directory_end = walk.directory_end();
    let located = walk.finish(&mut source)?;
    Ok(Box::new(ZipContents {
        source,
        directory_end,
        located,
        max_content,
    }))
}

struct ZipListing {
    source: Source,
    stage: Stage,
}

enum Stage {
    NotStarted,
    Walking(Walk),
    Finished,
}

impl ZipListing {
    fn finish(&mut self) -> Result<Option<Entry>, ListError> {
        if let Stage::Walking(walk) = std::mem::replace(&mut self.stage, Stage::Finished) {
            walk.finish(&mut self.source)
                .map_err(ListError::SourceUnreadable)?;
        }
        Ok(None)
    }
}

impl Listing for ZipListing {
    fn next(&mut self, size: &mut ListSize) -> Result<Option<Entry>, ListError> {
        if matches!(self.stage, Stage::NotStarted) {
            let walk = Walk::start(&mut self.source).map_err(ListError::SourceUnreadable)?;
            self.stage = Stage::Walking(walk);
        }
        let Stage::Walking(walk) = &mut self.stage else {
            return Ok(None);
        };
        let header = walk
            .next_header(&mut self.source)
            .map_err(ListError::SourceUnreadable)?;
        let Some(header) = header else {
            return self.finish();
        };
        size.add(header.name_length())
            .map_err(|_| ListError::ListSizeExceeded)?;
        let name = header
            .read_name(&mut self.source)
            .map_err(|error| ListError::SourceUnreadable(error.into()))?;
        let regular = header.is_regular(&name);
        Ok(Some(Entry {
            name,
            regular,
            position: Position(walk.last_index()),
        }))
    }
}

struct ZipContents {
    source: Source,
    directory_end: u64,
    /// Indexed by position, which is an entry's index in the directory.
    located: Vec<Located>,
    max_content: u64,
}

impl ZipContents {
    fn read_located(&mut self, located: Located) -> Result<ReadResult, BoxError> {
        let header = read_header(&mut self.source, located.header, self.directory_end)?;
        let name = header.read_name(&mut self.source)?;
        let regular = header.is_regular(&name);
        let content = read_content(
            &mut self.source,
            &header,
            located.data_start,
            self.max_content,
        );
        Ok(ReadResult::Entry {
            name,
            regular,
            content,
        })
    }
}

impl Contents for ZipContents {
    fn read(&mut self, position: Position) -> ReadResult {
        let located = usize::try_from(position.0)
            .ok()
            .and_then(|index| self.located.get(index))
            .copied();
        let Some(located) = located else {
            return ReadResult::RestUnreadable("the position is past the last entry".into());
        };
        match self.read_located(located) {
            Ok(result) => result,
            Err(error) => ReadResult::RestUnreadable(error),
        }
    }
}
