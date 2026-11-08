//! Reads tar files, plain or compressed with gzip.

// The driver is the only user of these, and it is not written yet.
#![allow(dead_code)]

use std::fs::File;

use super::transfer::{Contents, Listing};
use crate::types::BoxError;

/// Starts the first pass over a tar file. `gzip` says whether it is compressed
/// with gzip.
pub(super) fn tar_listing(file: File, gzip: bool) -> Box<dyn Listing> {
    let _ = (file, gzip);
    todo!()
}

/// Starts the second pass over a tar file. `gzip` says whether it is compressed
/// with gzip. No item's content may grow beyond `max_content`.
pub(super) fn tar_contents(
    file: File,
    gzip: bool,
    max_content: u64,
) -> Result<Box<dyn Contents>, BoxError> {
    let _ = (file, gzip, max_content);
    todo!()
}
