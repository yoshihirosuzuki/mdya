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

/// Checks a collection name against the naming rules. Callable without
/// creating the component.
pub fn check_collection_name(name: &CollectionName) -> Result<(), CollectionError> {
    let _ = name;
    todo!()
}
