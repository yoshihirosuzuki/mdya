//! Reads zip files.

// The driver is the only user of these, and it is not written yet.
#![allow(dead_code)]

use std::fs::File;

use super::transfer::{Contents, Listing};
use crate::types::BoxError;

/// Starts the first pass over a zip file.
pub(super) fn zip_listing(file: File) -> Box<dyn Listing> {
    let _ = file;
    todo!()
}

/// Starts the second pass over a zip file, after checking it again. No item's
/// content may grow beyond `max_content` when it is inflated.
pub(super) fn zip_contents(file: File, max_content: u64) -> Result<Box<dyn Contents>, BoxError> {
    let _ = (file, max_content);
    todo!()
}
