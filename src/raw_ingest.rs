//! The `raw_ingest` component: ingests a `raw` collection, whose documents the
//! user passes in directly.

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

/// Checks the names of the type-specific attributes: a `raw` collection has
/// none, so any attribute is an extra one. Callable without creating the
/// component.
pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError> {
    if attributes.0.is_empty() {
        return Ok(());
    }
    Err(IngestError::AttributesMissingOrExtra)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_no_attributes() {
        assert!(check_attribute_names(&TypeAttributes::default()).is_ok());
    }

    #[test]
    fn rejects_any_attribute() {
        let attributes = TypeAttributes(
            json!({ "path": "/home/user/docs" })
                .as_object()
                .unwrap()
                .clone(),
        );

        assert!(matches!(
            check_attribute_names(&attributes),
            Err(IngestError::AttributesMissingOrExtra)
        ));
    }
}
