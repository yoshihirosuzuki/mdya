//! Values that two or more components pass to each other. A type that only
//! one component's functions use lives in that component's module instead, so
//! that components do not depend on each other just for types.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(test)]
pub(crate) mod test_double;

/// A chunk: the part of a document's body that it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The range in the document's body, in UTF-8 byte offsets. Both ends are
    /// on character boundaries. The chunks of a document are in body order,
    /// together cover the whole body without gaps, and may overlap.
    pub range: std::ops::Range<usize>,
}

/// A collection name as the user gave it. It may not follow the naming rules;
/// `collection` checks them when a collection is created.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CollectionName(pub String);

/// A collection type (`directory`, `archive`, `raw`, ...). Only `gateway`
/// knows which values exist and which `ingest` implementation each one maps
/// to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CollectionType(pub String);

/// The type-specific attributes of a collection, as a JSON object. Only the
/// `ingest` implementation for the collection type knows what they mean.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TypeAttributes(pub serde_json::Map<String, serde_json::Value>);

/// An embedding model. Two models are the same when both the name and the
/// dimension are.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmbeddingModel {
    pub name: String,
    pub dimension: usize,
}

pub type EmbeddingVector = Vec<f32>;

/// The embedding vector of a chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkEmbedding {
    pub vector: EmbeddingVector,
    /// Whether the vector covers the whole chunk (the model may have truncated
    /// it).
    pub covers_whole_chunk: bool,
}

/// The cause of an error, kept as the `source` of the error that wraps it.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The functions that find the home directory and the working directory, for
/// [`crate::path::make_path_absolute`]. The real functions are meant to call
/// `std::env::home_dir` and `std::env::current_dir`; in tests, they give fixed
/// answers.
#[derive(Clone)]
pub struct DirLookup {
    pub home_dir: Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>,
    pub current_dir: Arc<dyn Fn() -> std::io::Result<PathBuf> + Send + Sync>,
}

/// A key: the name that tells documents apart within a collection. When it is
/// a path, the separator is `/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key(pub String);

/// The time of a document version. Collection types that do not report a time
/// (`raw`) have none, so functions take it as an `Option`.
pub type Time = std::time::SystemTime;

/// What happened to a document when it was registered in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentOutcome {
    /// Newly added.
    Added,
    /// Changed.
    Changed,
    /// Skipped because it had not changed.
    Unchanged,
    /// Registered again although it had not changed.
    Reingested,
}

/// Progress: how many items are done, and the total when it is known. What the
/// items are (documents or chunks) depends on the function that reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
}

/// Receives progress. Both functions return immediately and may be called
/// from several tasks at once. A reported `Progress` may be dropped in favour
/// of a later one.
pub trait ProgressSink: Send + Sync {
    fn report(&self, progress: Progress);
    /// Adds the number of chunks whose embedding vectors are done. The counts
    /// are summed, never dropped.
    fn add_embedded_chunks(&self, count: u64);
}
