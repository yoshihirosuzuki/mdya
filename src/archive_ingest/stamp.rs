//! What tells that an open archive has not been rewritten.

use std::fs::File;
use std::io;

use crate::types::{BoxError, Time};

/// The modification time and the length of an open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    modified: Time,
    length: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("the archive changed while it was being read")]
struct ArchiveChanged;

impl Stamp {
    pub(super) fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            modified: metadata.modified()?,
            length: metadata.len(),
        })
    }

    pub(super) fn modified(self) -> Time {
        self.modified
    }

    /// Checks that the open file still has this stamp.
    pub(super) fn confirm(self, file: &File) -> Result<(), BoxError> {
        if Self::of(file)? == self {
            return Ok(());
        }
        Err(Box::new(ArchiveChanged))
    }
}
