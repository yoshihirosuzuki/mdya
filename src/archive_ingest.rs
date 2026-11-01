//! The `archive_ingest` component: ingests an `archive` collection.

use std::sync::Arc;

use crate::format::Format;
use crate::index::Index;
use crate::ingest::{Ingest, IngestError};
use crate::types::TypeAttributes;

/// Creates the component.
pub fn create(format: Arc<dyn Format>, index: Arc<dyn Index>) -> Arc<dyn Ingest> {
    let _ = (format, index);
    todo!()
}

/// Checks the names of the type-specific attributes. Callable without creating
/// the component.
pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError> {
    let _ = attributes;
    todo!()
}
