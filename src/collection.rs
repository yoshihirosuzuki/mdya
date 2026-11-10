//! The `collection` component: keeps the collections the user has made.

mod store;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use lancedb::Table;

use self::store::{Hooks, RealHooks, Store};
use crate::index::{Index, IndexError};
use crate::types::{BoxError, CollectionName, CollectionType, TypeAttributes};

/// Creates the component over the LanceDB in the data directory. Nothing is
/// opened until a function first needs it.
pub fn create(data_dir: PathBuf, index: Arc<dyn Index>) -> Arc<dyn Collections> {
    Arc::new(LanceDbCollections::new(
        data_dir,
        index,
        Arc::new(RealHooks),
    ))
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

struct LanceDbCollections {
    store: Store,
    index: Arc<dyn Index>,
    /// Held from reading whether a collection exists until the write that
    /// depends on it is done, so that concurrent calls in this process cannot
    /// act on what another call is about to change. A call waits for the one
    /// holding it, and a call that panics releases it. Across processes,
    /// `gateway` runs one write operation at a time.
    writing: tokio::sync::Mutex<()>,
}

impl LanceDbCollections {
    fn new(data_dir: PathBuf, index: Arc<dyn Index>, hooks: Arc<dyn Hooks>) -> Self {
        Self {
            store: Store::new(data_dir, hooks),
            index,
            writing: tokio::sync::Mutex::new(()),
        }
    }

    /// The table and the collection's row, for a function that needs the
    /// collection to exist. Creates nothing.
    async fn existing(
        &self,
        name: &CollectionName,
    ) -> Result<(Table, CollectionInfo), CollectionError> {
        let table = self.store.table_to_read().await?;
        let table = table.ok_or(CollectionError::NotFound)?;
        let collection = self.store.read_one(&table, name).await?;
        Ok((table, collection.ok_or(CollectionError::NotFound)?))
    }
}

#[async_trait]
impl Collections for LanceDbCollections {
    async fn create_collection(&self, collection: CollectionInfo) -> Result<(), CollectionError> {
        check_collection_name(&collection.name)?;
        let _writing = self.writing.lock().await;
        let table = self.store.table_to_write().await?;
        if self
            .store
            .read_one(&table, &collection.name)
            .await?
            .is_some()
        {
            return Err(CollectionError::AlreadyExists);
        }
        self.store.add(&table, &collection).await
    }

    async fn delete_collection(&self, name: &CollectionName) -> Result<(), CollectionError> {
        let _writing = self.writing.lock().await;
        let (table, _) = self.existing(name).await?;
        // The documents go first: an operation that stops partway leaves the
        // collection, and deleting it again goes on with what is left. Keeping
        // nothing deletes every document of the collection.
        let deleted = self.index.delete_others(name, &HashSet::new(), &[]).await;
        deleted.map_err(|partial| CollectionError::Index(partial.error))?;
        self.store.delete(&table, name).await
    }

    async fn list_collections(&self) -> Result<Vec<CollectionInfo>, CollectionError> {
        let Some(table) = self.store.table_to_read().await? else {
            return Ok(Vec::new());
        };
        let mut collections = self.store.read_all(&table).await?;
        collections.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(collections)
    }

    async fn get_collection(
        &self,
        name: &CollectionName,
    ) -> Result<Option<CollectionInfo>, CollectionError> {
        let Some(table) = self.store.table_to_read().await? else {
            return Ok(None);
        };
        self.store.read_one(&table, name).await
    }

    async fn change_collection(
        &self,
        name: &CollectionName,
        change: CollectionChange,
    ) -> Result<CollectionInfo, CollectionError> {
        let _writing = self.writing.lock().await;
        let (table, current) = self.existing(name).await?;
        let changed = CollectionInfo {
            attributes: change.attributes.unwrap_or(current.attributes),
            description: change.description.unwrap_or(current.description),
            ..current
        };
        self.store.replace(&table, &changed).await?;
        Ok(changed)
    }
}
