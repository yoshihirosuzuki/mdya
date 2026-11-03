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

/// The name of the one type-specific attribute: where the archive is.
const PATH_ATTRIBUTE: &str = "path";

/// Checks the names of the type-specific attributes: there is `path` and
/// nothing else, and its value is a string. Whether the location exists is
/// not checked. `file_ingest` checks its attributes by the same rule in its
/// own code, so change the two together. Callable without creating the
/// component.
pub fn check_attribute_names(attributes: &TypeAttributes) -> Result<(), IngestError> {
    let only_a_path_string = attributes.0.len() == 1
        && attributes
            .0
            .get(PATH_ATTRIBUTE)
            .is_some_and(serde_json::Value::is_string);
    if only_a_path_string {
        return Ok(());
    }
    Err(IngestError::AttributesMissingOrExtra)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn attributes(value: Value) -> TypeAttributes {
        TypeAttributes(value.as_object().unwrap().clone())
    }

    #[test]
    fn accepts_a_path_alone_whose_value_is_a_string() {
        for value in [
            json!({ "path": "/home/user/docs.zip" }),
            json!({ "path": "" }),
        ] {
            assert!(
                check_attribute_names(&attributes(value.clone())).is_ok(),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_missing_or_extra_attributes() {
        for value in [
            json!({}),
            json!({ "location": "/home/user/docs.zip" }),
            json!({ "path": "/home/user/docs.zip", "recursive": true }),
        ] {
            assert!(
                matches!(
                    check_attribute_names(&attributes(value.clone())),
                    Err(IngestError::AttributesMissingOrExtra)
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_a_path_whose_value_is_not_a_string() {
        for value in [
            json!({ "path": null }),
            json!({ "path": 1 }),
            json!({ "path": ["/home/user/docs.zip"] }),
        ] {
            assert!(
                matches!(
                    check_attribute_names(&attributes(value.clone())),
                    Err(IngestError::AttributesMissingOrExtra)
                ),
                "{value}"
            );
        }
    }
}
