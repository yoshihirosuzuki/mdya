//! The `collection` component: keeps the collections the user has made.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::index::{Index, IndexError};
use crate::types::{BoxError, CollectionName, CollectionType, TypeAttributes};

/// Creates the component over the LanceDB in the data directory. Nothing is
/// opened until a function first needs it.
pub fn create(data_dir: PathBuf, index: Arc<dyn Index>) -> Arc<dyn Collections> {
    let _ = (data_dir, index);
    todo!()
}

/// A collection, as `list_collections` and `get_collection` return it and
/// `create_collection` takes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionInfo {
    pub name: CollectionName,
    pub collection_type: CollectionType,
    pub attributes: TypeAttributes,
    pub description: Option<String>,
}

/// What `change_collection` changes.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionChange {
    /// `None` keeps the description, `Some(None)` removes it, and
    /// `Some(Some(s))` sets it to `s`.
    pub description: Option<Option<String>>,
    /// `None` keeps the attributes.
    pub attributes: Option<TypeAttributes>,
}

#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    #[error("the collection name does not follow the naming rules")]
    NameInvalid,
    #[error("a collection with that name already exists")]
    AlreadyExists,
    #[error("the collection does not exist")]
    NotFound,
    #[error("the collection information cannot be read")]
    CannotRead { source: BoxError },
    #[error("the collection information cannot be written")]
    CannotWrite { source: BoxError },
    /// `index` failed while deleting the collection's documents.
    #[error("the collection's documents could not be deleted")]
    Index(#[source] IndexError),
}

#[async_trait]
pub trait Collections: Send + Sync {
    async fn create_collection(&self, collection: CollectionInfo) -> Result<(), CollectionError>;
    /// Deletes the collection's documents from the index, then the collection.
    async fn delete_collection(&self, name: &CollectionName) -> Result<(), CollectionError>;
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>, CollectionError>;
    async fn get_collection(
        &self,
        name: &CollectionName,
    ) -> Result<Option<CollectionInfo>, CollectionError>;
    async fn change_collection(
        &self,
        name: &CollectionName,
        change: CollectionChange,
    ) -> Result<CollectionInfo, CollectionError>;
}

/// The longest collection name, in characters.
const MAX_NAME_LENGTH: usize = 64;

/// Checks a collection name against the naming rules: 1 to 64 characters,
/// each an ASCII letter or digit, `-`, or `_`. Callable without creating the
/// component.
pub fn check_collection_name(name: &CollectionName) -> Result<(), CollectionError> {
    let name = name.0.as_str();
    // Counting bytes counts characters here: any character that is not ASCII
    // fails the second check anyway.
    let has_allowed_length = (1..=MAX_NAME_LENGTH).contains(&name.len());
    let has_only_allowed_characters = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if has_allowed_length && has_only_allowed_characters {
        return Ok(());
    }
    Err(CollectionError::NameInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str) -> Result<(), CollectionError> {
        check_collection_name(&CollectionName(name.to_owned()))
    }

    #[test]
    fn accepts_ascii_letters_digits_hyphens_and_underscores() {
        for name in ["a", "notes", "Work-Notes_2", "_", "-", "0"] {
            assert!(check(name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn accepts_a_name_of_64_characters() {
        assert!(check(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn rejects_an_empty_name() {
        assert!(matches!(check(""), Err(CollectionError::NameInvalid)));
    }

    #[test]
    fn rejects_a_name_of_65_characters() {
        assert!(matches!(
            check(&"a".repeat(65)),
            Err(CollectionError::NameInvalid)
        ));
    }

    #[test]
    fn rejects_other_characters() {
        for name in ["my notes", "a.b", "a/b", "a\\b", "メモ", "é", "a\n"] {
            assert!(
                matches!(check(name), Err(CollectionError::NameInvalid)),
                "{name:?}"
            );
        }
    }
}
