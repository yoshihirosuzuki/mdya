//! A test double for [`Query`].

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;

use crate::query::{ChunkRangeRequest, Query, QueryError, SearchRequest, SearchResult};
use crate::types::{CollectionName, Key};

type SearchFn = dyn Fn(&SearchRequest) -> Result<SearchResult, QueryError> + Send + Sync;
type GetFn =
    dyn Fn(&CollectionName, &Key, ChunkRangeRequest) -> Result<String, QueryError> + Send + Sync;

/// Answers every function from a handler and records every call. By default,
/// a search finds nothing and every document is missing.
pub(crate) struct TestQuery {
    search: Box<SearchFn>,
    get_document: Box<GetFn>,
    calls: Mutex<Vec<QueryCall>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryCall {
    Search(SearchRequest),
    GetDocument(CollectionName, Key, ChunkRangeRequest),
}

impl TestQuery {
    pub(crate) fn new() -> Self {
        Self {
            search: Box::new(|_| {
                Ok(SearchResult {
                    hits: Vec::new(),
                    warnings: Vec::new(),
                })
            }),
            get_document: Box::new(|_, _, _| Err(QueryError::DocumentNotFound)),
            calls: Mutex::default(),
        }
    }

    pub(crate) fn on_search(
        mut self,
        f: impl Fn(&SearchRequest) -> Result<SearchResult, QueryError> + Send + Sync + 'static,
    ) -> Self {
        self.search = Box::new(f);
        self
    }

    pub(crate) fn on_get_document(
        mut self,
        f: impl Fn(&CollectionName, &Key, ChunkRangeRequest) -> Result<String, QueryError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.get_document = Box::new(f);
        self
    }

    pub(crate) fn calls(&self) -> Vec<QueryCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for TestQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Query for TestQuery {
    async fn search(&self, request: SearchRequest) -> Result<SearchResult, QueryError> {
        self.calls
            .lock()
            .unwrap()
            .push(QueryCall::Search(request.clone()));
        (self.search)(&request)
    }

    async fn get_document(
        &self,
        collection: &CollectionName,
        key: &Key,
        range: ChunkRangeRequest,
    ) -> Result<String, QueryError> {
        self.calls.lock().unwrap().push(QueryCall::GetDocument(
            collection.clone(),
            key.clone(),
            range,
        ));
        (self.get_document)(collection, key, range)
    }
}

#[cfg(test)]
mod tests {
    use crate::query::{SearchMode, SearchUnit};

    use super::*;

    fn request() -> SearchRequest {
        SearchRequest {
            query: "release".to_owned(),
            mode: SearchMode::Hybrid,
            collections: Vec::new(),
            limit: 20,
            unit: SearchUnit::Document,
        }
    }

    #[tokio::test]
    async fn by_default_nothing_is_found() {
        let query = TestQuery::new();
        let range = ChunkRangeRequest {
            start: None,
            end: None,
        };

        let result = query.search(request()).await.unwrap();
        let document = query
            .get_document(
                &CollectionName("notes".to_owned()),
                &Key("a.md".to_owned()),
                range,
            )
            .await;

        assert!(result.hits.is_empty());
        assert!(matches!(document, Err(QueryError::DocumentNotFound)));
        assert_eq!(query.calls().len(), 2);
    }

    #[tokio::test]
    async fn a_replaced_handler_answers_and_the_request_is_recorded() {
        let query = TestQuery::new().on_get_document(|_, key, _| Ok(format!("text of {}", key.0)));
        let range = ChunkRangeRequest {
            start: Some(1),
            end: None,
        };

        let text = query
            .get_document(
                &CollectionName("notes".to_owned()),
                &Key("a.md".to_owned()),
                range,
            )
            .await
            .unwrap();

        assert_eq!(text, "text of a.md");
        assert_eq!(
            query.calls(),
            vec![QueryCall::GetDocument(
                CollectionName("notes".to_owned()),
                Key("a.md".to_owned()),
                range
            )]
        );
    }

    #[tokio::test]
    async fn a_replaced_search_answers_and_the_request_is_recorded() {
        let query = TestQuery::new().on_search(|_| Err(QueryError::ZeroLimit));

        let result = query.search(request()).await;

        assert!(matches!(result, Err(QueryError::ZeroLimit)));
        assert_eq!(query.calls(), vec![QueryCall::Search(request())]);
    }
}
