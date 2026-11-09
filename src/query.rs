//! The `query` component: searches the index and reads documents back.

use std::sync::Arc;

use async_trait::async_trait;

use lancedb::index::scalar::FtsIndexBuilder;

use crate::embedding::{Embedding, EmbeddingError};
use crate::index::{Index, IndexError, fts_index_params};
use crate::types::{BoxError, CollectionName, Key};

mod document;
mod search;
mod snippet;
#[cfg(test)]
mod test_tables;

use document::StoredDocument;
use snippet::Tokenizers;

/// Creates the component.
pub fn create(embedding: Arc<dyn Embedding>, index: Arc<dyn Index>) -> Arc<dyn Query> {
    Arc::new(IndexedQuery::new(embedding, index, fts_index_params))
}

struct IndexedQuery {
    embedding: Arc<dyn Embedding>,
    index: Arc<dyn Index>,
    tokenizers: Tokenizers,
}

impl IndexedQuery {
    fn new(
        embedding: Arc<dyn Embedding>,
        index: Arc<dyn Index>,
        fts_settings: fn() -> FtsIndexBuilder,
    ) -> Self {
        Self {
            embedding,
            index,
            tokenizers: Tokenizers::new(fts_settings),
        }
    }
}

/// Runs `work` where it does not hold up other asynchronous work, and waits
/// for it. A panic in `work` reaches the caller as the same panic. The error
/// is returned only when the task was cancelled before it started.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Ok(value),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(error),
    }
}

#[async_trait]
impl Query for IndexedQuery {
    async fn search(&self, request: SearchRequest) -> Result<SearchResult, QueryError> {
        search::run(
            self.embedding.as_ref(),
            self.index.as_ref(),
            &self.tokenizers,
            request,
        )
        .await
    }

    async fn get_document(
        &self,
        collection: &CollectionName,
        key: &Key,
        range: ChunkRangeRequest,
    ) -> Result<String, QueryError> {
        check_chunk_range(&range)?;
        let tables = self.index.tables_for_query().await;
        let Some(tables) = tables.map_err(QueryError::Index)? else {
            return Err(QueryError::DocumentNotFound);
        };
        let document = StoredDocument::read(&tables.document, collection, key).await;
        let document = document.map_err(|source| QueryError::CannotRead { source })?;
        document.ok_or(QueryError::DocumentNotFound)?.text(range)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    FullText,
    Vector,
    Hybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchUnit {
    Document,
    Chunk,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    pub mode: SearchMode,
    /// Empty means all collections.
    pub collections: Vec<CollectionName>,
    pub limit: usize,
    pub unit: SearchUnit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    /// In the order the search returns them.
    pub hits: Vec<Hit>,
    /// Warnings to show with the result (for example, a different embedding
    /// model).
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub collection: CollectionName,
    pub key: Key,
    pub score: f64,
    pub snippet: String,
    /// The number of the chunk the snippet comes from.
    pub chunk_index: usize,
    /// The number of chunks in the document.
    pub chunk_count: usize,
}

/// The chunks of a document to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRangeRequest {
    pub start: Option<usize>,
    pub end: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("the query is empty")]
    EmptyQuery,
    #[error("the limit is zero")]
    ZeroLimit,
    #[error("the chunk range is not valid")]
    InvalidChunkRange,
    #[error("the document does not exist")]
    DocumentNotFound,
    #[error("the chunk does not exist")]
    ChunkNotFound,
    #[error("the embedding model failed")]
    Embedding(#[source] EmbeddingError),
    /// `index` failed while handing over the tables to read.
    #[error("the index cannot be read")]
    Index(#[source] IndexError),
    #[error("the index cannot be read")]
    CannotRead { source: BoxError },
}

#[async_trait]
pub trait Query: Send + Sync {
    async fn search(&self, request: SearchRequest) -> Result<SearchResult, QueryError>;
    /// The document's text, or the text of the requested chunks.
    async fn get_document(
        &self,
        collection: &CollectionName,
        key: &Key,
        range: ChunkRangeRequest,
    ) -> Result<String, QueryError>;
}

/// Checks a search request without reading anything: the query has something
/// other than white space (as `char::is_whitespace` sees it, so the
/// ideographic space counts as white space too), and the limit is 1 or more.
/// When both are wrong, the empty query is reported. Callable without creating
/// the component.
pub fn check_search_request(request: &SearchRequest) -> Result<(), QueryError> {
    if request.query.trim().is_empty() {
        return Err(QueryError::EmptyQuery);
    }
    if request.limit == 0 {
        return Err(QueryError::ZeroLimit);
    }
    Ok(())
}

/// Checks a chunk range without reading anything: an end needs a start, and is
/// not before it. Callable without creating the component.
pub fn check_chunk_range(range: &ChunkRangeRequest) -> Result<(), QueryError> {
    match (range.start, range.end) {
        (None, Some(_)) => Err(QueryError::InvalidChunkRange),
        (Some(start), Some(end)) if end < start => Err(QueryError::InvalidChunkRange),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &str, limit: usize) -> SearchRequest {
        SearchRequest {
            query: query.to_owned(),
            mode: SearchMode::Hybrid,
            collections: Vec::new(),
            limit,
            unit: SearchUnit::Document,
        }
    }

    fn range(start: Option<usize>, end: Option<usize>) -> ChunkRangeRequest {
        ChunkRangeRequest { start, end }
    }

    #[test]
    fn accepts_a_query_with_something_other_than_spaces_and_a_limit_of_1_or_more() {
        for (query, limit) in [("rust", 1), (" rust ", 20), ("検索", usize::MAX)] {
            assert!(
                check_search_request(&request(query, limit)).is_ok(),
                "{query:?} {limit}"
            );
        }
    }

    #[test]
    fn rejects_a_query_that_is_empty_or_only_spaces() {
        for query in ["", " ", "\t\n", "\u{3000}"] {
            assert!(
                matches!(
                    check_search_request(&request(query, 20)),
                    Err(QueryError::EmptyQuery)
                ),
                "{query:?}"
            );
        }
    }

    #[test]
    fn rejects_a_limit_of_0() {
        assert!(matches!(
            check_search_request(&request("rust", 0)),
            Err(QueryError::ZeroLimit)
        ));
    }

    #[test]
    fn reports_the_empty_query_first_when_the_limit_is_also_0() {
        assert!(matches!(
            check_search_request(&request(" ", 0)),
            Err(QueryError::EmptyQuery)
        ));
    }

    #[test]
    fn accepts_no_range_a_start_alone_or_an_end_not_before_the_start() {
        for (start, end) in [
            (None, None),
            (Some(0), None),
            (Some(3), Some(3)),
            (Some(3), Some(5)),
        ] {
            assert!(
                check_chunk_range(&range(start, end)).is_ok(),
                "{start:?} {end:?}"
            );
        }
    }

    #[test]
    fn rejects_an_end_without_a_start() {
        assert!(matches!(
            check_chunk_range(&range(None, Some(3))),
            Err(QueryError::InvalidChunkRange)
        ));
    }

    #[test]
    fn rejects_an_end_before_the_start() {
        assert!(matches!(
            check_chunk_range(&range(Some(5), Some(4))),
            Err(QueryError::InvalidChunkRange)
        ));
    }
}
