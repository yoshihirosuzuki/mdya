//! A test double for [`Collections`].

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;

use crate::collection::{CollectionChange, CollectionError, CollectionInfo, Collections};
use crate::types::CollectionName;

type CreateFn = dyn Fn(&CollectionInfo) -> Result<(), CollectionError> + Send + Sync;
type NameFn<T> = dyn Fn(&CollectionName) -> Result<T, CollectionError> + Send + Sync;
type ListFn = dyn Fn() -> Result<Vec<CollectionInfo>, CollectionError> + Send + Sync;
type ChangeFn = dyn Fn(&CollectionName, &CollectionChange) -> Result<CollectionInfo, CollectionError>
    + Send
    + Sync;

/// Answers every function from a handler and records every call. The default
/// handlers stand for no collections that accept everything: `create` and
/// `delete` succeed, the list is empty, `get` finds nothing, and `change`
/// fails with `NotFound`. Replace a handler with the `on_*` function of the
/// same name.
pub(crate) struct TestCollections {
    create: Box<CreateFn>,
    delete: Box<NameFn<()>>,
    list: Box<ListFn>,
    get: Box<NameFn<Option<CollectionInfo>>>,
    change: Box<ChangeFn>,
    calls: Mutex<Vec<CollectionsCall>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CollectionsCall {
    Create(CollectionInfo),
    Delete(CollectionName),
    List,
    Get(CollectionName),
    Change(CollectionName, CollectionChange),
}

impl TestCollections {
    pub(crate) fn new() -> Self {
        Self {
            create: Box::new(|_| Ok(())),
            delete: Box::new(|_| Ok(())),
            list: Box::new(|| Ok(Vec::new())),
            get: Box::new(|_| Ok(None)),
            change: Box::new(|_, _| Err(CollectionError::NotFound)),
            calls: Mutex::default(),
        }
    }

    /// A double whose `list` and `get` answer from `collections`.
    pub(crate) fn with(collections: Vec<CollectionInfo>) -> Self {
        let listed = collections.clone();
        Self::new()
            .on_list(move || Ok(listed.clone()))
            .on_get(move |name| {
                Ok(collections
                    .iter()
                    .find(|collection| &collection.name == name)
                    .cloned())
            })
    }

    pub(crate) fn on_create(
        mut self,
        f: impl Fn(&CollectionInfo) -> Result<(), CollectionError> + Send + Sync + 'static,
    ) -> Self {
        self.create = Box::new(f);
        self
    }

    pub(crate) fn on_delete(
        mut self,
        f: impl Fn(&CollectionName) -> Result<(), CollectionError> + Send + Sync + 'static,
    ) -> Self {
        self.delete = Box::new(f);
        self
    }

    pub(crate) fn on_list(
        mut self,
        f: impl Fn() -> Result<Vec<CollectionInfo>, CollectionError> + Send + Sync + 'static,
    ) -> Self {
        self.list = Box::new(f);
        self
    }

    pub(crate) fn on_get(
        mut self,
        f: impl Fn(&CollectionName) -> Result<Option<CollectionInfo>, CollectionError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.get = Box::new(f);
        self
    }

    pub(crate) fn on_change(
        mut self,
        f: impl Fn(&CollectionName, &CollectionChange) -> Result<CollectionInfo, CollectionError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.change = Box::new(f);
        self
    }

    pub(crate) fn calls(&self) -> Vec<CollectionsCall> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: CollectionsCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Default for TestCollections {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collections for TestCollections {
    async fn create_collection(&self, collection: CollectionInfo) -> Result<(), CollectionError> {
        self.record(CollectionsCall::Create(collection.clone()));
        (self.create)(&collection)
    }

    async fn delete_collection(&self, name: &CollectionName) -> Result<(), CollectionError> {
        self.record(CollectionsCall::Delete(name.clone()));
        (self.delete)(name)
    }

    async fn list_collections(&self) -> Result<Vec<CollectionInfo>, CollectionError> {
        self.record(CollectionsCall::List);
        (self.list)()
    }

    async fn get_collection(
        &self,
        name: &CollectionName,
    ) -> Result<Option<CollectionInfo>, CollectionError> {
        self.record(CollectionsCall::Get(name.clone()));
        (self.get)(name)
    }

    async fn change_collection(
        &self,
        name: &CollectionName,
        change: CollectionChange,
    ) -> Result<CollectionInfo, CollectionError> {
        self.record(CollectionsCall::Change(name.clone(), change.clone()));
        (self.change)(name, &change)
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{CollectionType, TypeAttributes};

    use super::*;

    fn notes() -> CollectionInfo {
        CollectionInfo {
            name: CollectionName("notes".to_owned()),
            collection_type: CollectionType("directory".to_owned()),
            attributes: TypeAttributes::default(),
            description: None,
        }
    }

    #[tokio::test]
    async fn the_defaults_stand_for_no_collections() {
        let collections = TestCollections::new();

        assert!(collections.list_collections().await.unwrap().is_empty());
        assert!(
            collections
                .get_collection(&CollectionName("notes".to_owned()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            collections
                .change_collection(
                    &CollectionName("notes".to_owned()),
                    CollectionChange {
                        description: None,
                        attributes: None
                    }
                )
                .await,
            Err(CollectionError::NotFound)
        ));
    }

    #[tokio::test]
    async fn with_answers_list_and_get_from_the_given_collections() {
        let collections = TestCollections::with(vec![notes()]);

        assert_eq!(collections.list_collections().await.unwrap(), vec![notes()]);
        assert_eq!(
            collections
                .get_collection(&CollectionName("notes".to_owned()))
                .await
                .unwrap(),
            Some(notes())
        );
        assert_eq!(
            collections.calls(),
            vec![
                CollectionsCall::List,
                CollectionsCall::Get(CollectionName("notes".to_owned()))
            ]
        );
    }

    #[tokio::test]
    async fn replaced_create_and_delete_answer_and_are_recorded() {
        let collections = TestCollections::new()
            .on_create(|_| Err(CollectionError::AlreadyExists))
            .on_delete(|_| Err(CollectionError::NotFound));

        let created = collections.create_collection(notes()).await;
        let deleted = collections
            .delete_collection(&CollectionName("memo".to_owned()))
            .await;

        assert!(matches!(created, Err(CollectionError::AlreadyExists)));
        assert!(matches!(deleted, Err(CollectionError::NotFound)));
        assert_eq!(
            collections.calls(),
            vec![
                CollectionsCall::Create(notes()),
                CollectionsCall::Delete(CollectionName("memo".to_owned()))
            ]
        );
    }
}
