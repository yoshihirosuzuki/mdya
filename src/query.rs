//! The `query` component: searches the index and reads documents back.

use std::sync::Arc;

use async_trait::async_trait;

use crate::embedding::{Embedding, EmbeddingError};
use crate::index::{Index, IndexError};
use crate::types::{BoxError, CollectionName, Key};

/// Creates the component.
pub fn create(embedding: Arc<dyn Embedding>, index: Arc<dyn Index>) -> Arc<dyn Query> {
    let _ = (embedding, index);
    todo!()
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

/// Checks a search request without reading anything. Callable without
/// creating the component.
pub fn check_search_request(request: &SearchRequest) -> Result<(), QueryError> {
    let _ = request;
    todo!()
}

/// Checks a chunk range without reading anything. Callable without creating
/// the component.
pub fn check_chunk_range(range: &ChunkRangeRequest) -> Result<(), QueryError> {
    let _ = range;
    todo!()
}
