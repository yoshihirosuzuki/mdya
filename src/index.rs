//! The `index` component: keeps the documents and chunks that search reads,
//! with their embedding vectors and full-text search indexes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::future::Future;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream::{BoxStream, FuturesUnordered};
use futures::{StreamExt, TryStreamExt};
use lancedb::DistanceType;
use lancedb::arrow::arrow_array::builder::StringBuilder;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::{
    Float32Type, TimestampMicrosecondType, UInt32Type, UInt64Type,
};
use lancedb::arrow::arrow_array::{
    Array, ArrayRef, ArrowPrimitiveType, BooleanArray, FixedSizeListArray, Float32Array, ListArray,
    PrimitiveArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray, StructArray,
    TimestampMicrosecondArray, UInt32Array, UInt64Array,
};
use lancedb::arrow::arrow_buffer::{NullBuffer, OffsetBuffer};
use lancedb::arrow::arrow_schema::{
    ArrowError, DataType, Field, FieldRef, Fields, Schema, SchemaRef, TimeUnit,
};
use lancedb::arrow::arrow_select::take::take_record_batch;
use lancedb::arrow::{
    SendableRecordBatchStream, SendableRecordBatchStreamExt, SimpleRecordBatchStream,
};
use lancedb::expr::{DfExpr, col, lit};
use lancedb::index::scalar::{BTreeIndexBuilder, FtsIndexBuilder};
use lancedb::index::vector::IvfPqIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::table::{
    ColumnAlteration, CompactionOptions, Duration as TimeDelta, NewColumnTransform, OptimizeAction,
    OptimizeOptions,
};
use sha2::{Digest, Sha256};

use crate::embedding::{Embedding, EmbeddingError};
use crate::types::{
    BoxError, Chunk, ChunkEmbedding, CollectionName, DocumentOutcome, EmbeddingModel, Key,
    Progress, ProgressSink, Time,
};

#[cfg(test)]
pub(crate) mod test_double;
#[cfg(test)]
mod tests;

/// Creates the component over the LanceDB in the data directory. Nothing is
/// opened until a function first needs the index.
pub fn create(data_dir: PathBuf, embedding: Arc<dyn Embedding>) -> Arc<dyn Index> {
    Arc::new(LanceDbIndex::new(data_dir, embedding, Arc::new(RealHooks)))
}

/// A document to register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentToRegister {
    pub collection: CollectionName,
    pub key: Key,
    /// `None` for collection types that do not report a time (`raw`).
    pub time: Option<Time>,
    pub body: String,
    pub chunks: Vec<Chunk>,
    /// Whether to register the document again even when it has not changed.
    pub overwrite: bool,
}

/// Which part of the index `status` looks at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusScope {
    All,
    Collections(Vec<CollectionName>),
}

/// Whether `status` checks the dimension of the embedding model it compares
/// with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimensionCheck {
    Always,
    /// Only when the index has chunks made with a model of the same name.
    OnlyIfSameName,
    Never,
}

/// The state of the index, as `status` returns it.
#[derive(Debug)]
pub struct IndexStatus {
    pub compared_model_name: String,
    pub compared_dimension: DimensionResult,
    /// In the order the status operation shows them.
    pub models: Vec<ModelStatus>,
    /// The same as `models`, for the chunks of each collection in the scope
    /// that has chunks.
    pub collections: BTreeMap<CollectionName, Vec<ModelStatus>>,
    pub document_count: u64,
    pub chunk_count: u64,
    /// Chunks counted as having no vector: those whose vector is null, and,
    /// while a table lacks the vector column it reads or the two tables read
    /// vectors of different models (partway through switching the vector
    /// columns), every chunk.
    pub chunks_without_vector: u64,
}

#[derive(Debug)]
pub enum DimensionResult {
    Known(usize),
    /// The dimension could not be checked.
    Unavailable(EmbeddingError),
    NotChecked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelStatus {
    pub model: EmbeddingModel,
    /// Whether this is the model compared with. `Some(false)` when the names
    /// differ; `None` when the names are the same but the dimension was not
    /// checked or could not be.
    pub same_as_compared: Option<bool>,
    pub chunk_count: u64,
    /// Of those, the chunks whose embedding vector does not cover the whole
    /// chunk.
    pub truncated_chunk_count: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The document does not exist. Only `delete_document` returns it.
    #[error("the document does not exist")]
    DocumentNotFound,
    /// The embedding model cannot be used. Only `register`,
    /// `rebuild_embeddings`, and `check_ingestable` return it.
    #[error("the embedding model cannot be used")]
    Embedding(#[source] EmbeddingError),
    /// The configured embedding model differs from the one the index's vector
    /// columns were made with; rebuild the embedding vectors first. Only
    /// `register` and `check_ingestable` return it. `configured_dimension` is
    /// `None` when the configured model's dimension could not be checked.
    #[error("the configured embedding model differs from the one the index was made with")]
    ModelMismatch {
        configured_name: String,
        configured_dimension: Option<usize>,
        indexed: EmbeddingModel,
    },
    /// A rebuild of the embedding vectors stopped partway through switching
    /// the vector columns, and a vector column is of another model than the
    /// configured one; rebuild first. Only `register` and `check_ingestable`
    /// return it.
    #[error("a rebuild of the embedding vectors has not finished")]
    RebuildIncomplete,
    #[error("the index cannot be read")]
    CannotRead { source: BoxError },
    #[error("the index cannot be written")]
    CannotWrite { source: BoxError },
}

/// What gets the outcome of each document `register_stream` has written.
pub trait RegistrationSink: Send + Sync {
    fn registered(&self, collection: &CollectionName, key: &Key, outcome: DocumentOutcome);
}

#[async_trait]
pub trait Index: Send + Sync {
    /// Registers the documents of `documents`: adds, replaces, updates only
    /// the time of, or skips each, and passes which to `results` once the
    /// document is written. It asks `documents` for the next document only
    /// when it needs one, and makes the embedding vectors of at most
    /// `parallelism` documents at once (one when it is 0).
    ///
    /// The documents are read and written a bulk at a time. When making a
    /// document's embedding vectors fails, it starts no other document, and
    /// first writes the documents whose vectors are made and passes their
    /// outcomes. A document it has not finished is neither written nor
    /// passed. It returns the first error that made it stop. The caller passes no document twice, in one stream
    /// or in two calls at once.
    ///
    /// By default, the documents are registered one at a time with
    /// `register`, which the test doubles of other components rely on; the
    /// component itself reads and writes them a bulk at a time.
    async fn register_stream(
        &self,
        mut documents: BoxStream<'_, DocumentToRegister>,
        _parallelism: usize,
        results: &dyn RegistrationSink,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        while let Some(document) = documents.next().await {
            let (collection, key) = (document.collection.clone(), document.key.clone());
            let outcome = self.register(document, progress).await?;
            results.registered(&collection, &key, outcome);
        }
        Ok(())
    }
    /// Registers one document, as `register_stream` does a stream of it
    /// alone, and says what it did.
    async fn register(
        &self,
        document: DocumentToRegister,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<DocumentOutcome, IndexError>;
    /// Deletes the documents of the collection whose key is neither in `keep`
    /// nor under one of `unreadable_dirs` (key prefixes), and returns how many
    /// it deleted. A key is under a directory when it begins with the
    /// directory and `/`, so a directory in `unreadable_dirs` has no trailing
    /// `/`.
    async fn delete_others(
        &self,
        collection: &CollectionName,
        keep: &HashSet<Key>,
        unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete>;
    async fn delete_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<(), IndexError>;
    async fn has_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<bool, IndexError>;
    /// The number of documents in each collection that has any.
    async fn document_counts(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError>;
    /// The state of the index, compared with the embedding model of
    /// `compare_with`.
    async fn status(
        &self,
        scope: StatusScope,
        compare_with: &dyn Embedding,
        check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError>;
    /// Rebuilds the embedding vectors with the configured model, and returns
    /// the number of chunk rows once the vector columns are rebuilt or
    /// switched over; 0 when it did neither, or when there is no chunk row.
    async fn rebuild_embeddings(
        &self,
        include_configured_model: bool,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError>;
    /// The tables `query` reads, pinned to the latest version. `None` until
    /// both tables and their full-text search indexes exist.
    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError>;
    /// Compacts the tables, cleans up old versions, and indexes new rows.
    async fn maintain(&self) -> Result<(), IndexError>;
    /// Whether documents can be registered now: the configured embedding
    /// model is the one the vector columns were made with (by name alone when
    /// its dimension cannot be checked), and no rebuild stopped partway
    /// through switching the columns to another model. A switch whose columns
    /// are all of the configured model is finished, and what a stopped
    /// rebuild left before switching is dropped. While no vector column has a
    /// value, it makes the vector columns for the configured model instead of
    /// returning an error. Making them needs that model's dimension.
    async fn check_ingestable(&self) -> Result<(), IndexError>;
}

/// The error of `delete_others`, with the number of documents deleted before
/// it stopped.
#[derive(Debug, thiserror::Error)]
#[error("stopped after deleting {deleted} documents")]
pub struct PartialDelete {
    pub deleted: u64,
    #[source]
    pub error: IndexError,
}

/// The tables `query` reads, both pinned to the version that was the latest
/// when they were taken.
pub struct QueryTables {
    pub document: lancedb::Table,
    pub chunk: lancedb::Table,
}

/// The environment variable that tells LanceDB where to read the full-text
/// search dictionary configuration from.
const LANGUAGE_MODEL_HOME: &str = "LANCE_LANGUAGE_MODEL_HOME";

/// The directory in the data directory that `LANGUAGE_MODEL_HOME` points to.
const LANGUAGE_MODELS_DIR: &str = "index_language_models";

/// The lindera configuration for the `lindera/ipadic` tokenizer: the IPADIC
/// dictionary built into the binary (so nothing is downloaded at run time),
/// NFKC normalization before tokenizing, and dropping the parts of speech that
/// Lucene's Japanese analyzer drops by default. The tags are the non-comment
/// lines of Lucene 10.6.0's `stoptags.txt`
/// (<https://raw.githubusercontent.com/apache/lucene/releases/lucene/10.6.0/lucene/analysis/kuromoji/src/resources/org/apache/lucene/analysis/ja/stoptags.txt>),
/// with its `-` separators written as lindera's `,`.
pub(crate) const DICTIONARY_CONFIG: &str = r#"segmenter:
  mode: normal
  dictionary: "embedded://ipadic"
character_filters:
  - kind: unicode_normalize
    args:
      kind: nfkc
token_filters:
  - kind: japanese_stop_tags
    args:
      tags:
        - "接続詞"
        - "助詞"
        - "助詞,格助詞"
        - "助詞,格助詞,一般"
        - "助詞,格助詞,引用"
        - "助詞,格助詞,連語"
        - "助詞,接続助詞"
        - "助詞,係助詞"
        - "助詞,副助詞"
        - "助詞,間投助詞"
        - "助詞,並立助詞"
        - "助詞,終助詞"
        - "助詞,副助詞／並立助詞／終助詞"
        - "助詞,連体化"
        - "助詞,副詞化"
        - "助詞,特殊"
        - "助動詞"
        - "記号"
        - "記号,一般"
        - "記号,読点"
        - "記号,句点"
        - "記号,空白"
        - "記号,括弧開"
        - "記号,括弧閉"
        - "その他,間投"
        - "フィラー"
        - "非言語音"
"#;

/// The full-text search index settings, used both to build the index and to
/// tokenize text the same way when reading. Every option is set explicitly
/// rather than left to lance's defaults, which LanceDB's documents describe
/// inconsistently. The `lindera/ipadic` tokenizer reads its configuration from
/// the file that [`place_fts_dictionary_config`] places. Callable without
/// creating the component.
pub fn fts_index_params() -> FtsIndexBuilder {
    #[expect(clippy::expect_used)]
    let params = FtsIndexBuilder::default()
        .language("English")
        .expect("lance accepts \"English\" as a language name");
    params
        .base_tokenizer("lindera/ipadic".to_owned())
        .lower_case(true)
        .ascii_folding(true)
        .stem(true)
        .remove_stop_words(true)
        .max_token_length(Some(255))
        .with_position(false)
}

/// The environment variables to set in the process for the data directory, as
/// name and value pairs. Empty when the data directory's path is not Unicode:
/// LanceDB reads the variable with `std::env::var`, which ignores a value that
/// is not Unicode, so setting it would have no effect. Callable without
/// creating the component.
pub fn process_environment(data_dir: &Path) -> Vec<(OsString, OsString)> {
    if data_dir.to_str().is_none() {
        return Vec::new();
    }
    vec![(
        OsString::from(LANGUAGE_MODEL_HOME),
        data_dir.join(LANGUAGE_MODELS_DIR).into_os_string(),
    )]
}

/// Places the full-text search dictionary configuration file in the directory
/// that [`process_environment`] points LanceDB to, unless the file is already
/// there. Callable without creating the component.
pub async fn place_fts_dictionary_config(data_dir: &Path) -> Result<(), IndexError> {
    let dir = data_dir
        .join(LANGUAGE_MODELS_DIR)
        .join("lindera")
        .join("ipadic");
    // Once the task has started, it cannot be cancelled: if the caller stops
    // waiting, the task still runs to the end, placing the file or trying to
    // remove its temporary file.
    let placed = run_blocking(move || {
        let path = dir.join("config.yml");
        if path.try_exists().map_err(cannot_write)? {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).map_err(cannot_write)?;
        write_unless_present(&dir, &path, DICTIONARY_CONFIG)
    });
    placed
        .await
        .unwrap_or_else(|error| Err(write_error(Box::new(error))))
}

/// Runs `work`, which blocks, on tokio's blocking threads, so that it does not
/// hold up other asynchronous work, and waits for it to end. A panic in `work`
/// reaches the caller as the same panic, not as an error. The error is
/// returned only when the task was cancelled before it started, as can happen
/// when the runtime shuts down. Once the task has started, it cannot be
/// cancelled: a caller that stops waiting leaves it running to the end, and a
/// panic in it then reaches no one, so callers wait for it.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Ok(value),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(error),
    }
}

/// Writes `content` to `path`, a file in `dir`, unless a file is already
/// there. The content goes to a temporary file in `dir` first, which is moved
/// to `path` only if nothing is there, so that an existing file is never
/// replaced and a reader never sees half a file. When the temporary file is not
/// moved, including when writing it fails, dropping it tries to remove it; a
/// failure to remove it is ignored, because the configuration is placed or not
/// either way. Where tempfile has to fall back to a hard link, the temporary
/// name can also stay behind as a second name of the placed file.
fn write_unless_present(dir: &Path, path: &Path, content: &str) -> Result<(), IndexError> {
    let mut temporary = tempfile::Builder::new()
        .prefix(".config.yml.")
        .tempfile_in(dir)
        .map_err(cannot_write)?;
    temporary
        .write_all(content.as_bytes())
        .map_err(cannot_write)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(cannot_write(error.error)),
    }
}

fn cannot_write(error: std::io::Error) -> IndexError {
    IndexError::CannotWrite {
        source: Box::new(error),
    }
}

// ---------------------------------------------------------------- hooks

/// The kinds of long computation that the component splits into slices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Computation {
    /// Hashing a document's body.
    Hash,
    /// Comparing a document's chunk rows with its body.
    Compare,
    /// Building the rows to write for a document.
    Build,
}

/// A call into LanceDB, or a step that prepares what LanceDB reads, as the
/// hooks see it before it is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LanceCall {
    /// Opening the connection to the LanceDB in the data directory.
    Connect,
    OpenTable(TableName),
    CreateTable(TableName),
    /// Creating one of the table's indexes.
    CreateIndex(TableName),
    /// Placing the full-text search dictionary configuration file, which
    /// LanceDB reads.
    PlaceDictionary,
    /// Reading the full-text search dictionary configuration.
    ReadDictionary,
    /// Reading a document's rows, or the rows of many documents.
    ReadRows(TableName),
    /// Upserting, updating some columns of, or deleting the rows of
    /// documents.
    WriteRows(TableName),
    /// Adding the rows of documents.
    AddRows(TableName),
    /// One step of maintaining a table, or, before a rebuild fills the
    /// document table, adding the chunk table's new rows to its indexes.
    Optimize(TableName, MaintenanceStep),
    /// Adding a table's new vector columns, with their values or all null.
    AddColumns(TableName),
    /// Dropping a table's vector columns, those it reads or new ones.
    DropColumns(TableName),
    /// Giving a table's new vector columns the names of those it reads.
    RenameColumns(TableName),
    /// A call on the table where a rebuild keeps the chunk vectors it made.
    RebuildTable(RebuildTableCall),
}

/// The calls on the rebuild table ([`REBUILD_TABLE`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebuildTableCall {
    Open,
    Create,
    /// Reading the vectors it keeps, or how many.
    Read,
    /// Adding vectors to keep.
    Add,
    Drop,
}

/// Where rebuilding one document's embedding vectors is, as the hooks see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentRebuild {
    Started,
    /// The vectors of the document's chunks are made.
    Finished,
}

/// How many days of old table versions maintenance keeps.
const KEEP_VERSIONS_DAYS: i64 = 7;

/// The steps of maintaining a table, in the order they are taken. They are
/// what LanceDB's `OptimizeAction::All` does, with the time to keep old
/// versions given rather than left to LanceDB, so that a newer LanceDB does
/// not change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaintenanceStep {
    /// Merging small files.
    Compact,
    /// Deleting the versions older than `older_than`; the latest version is
    /// kept however old it is.
    Prune { older_than: TimeDelta },
    /// Adding new rows to the indexes, and training an index that has enough
    /// rows.
    Index,
}

impl MaintenanceStep {
    fn all() -> [Self; 3] {
        [
            Self::Compact,
            Self::Prune {
                older_than: TimeDelta::days(KEEP_VERSIONS_DAYS),
            },
            Self::Index,
        ]
    }

    /// Whether the step leaves the table at its version: only cleaning up
    /// old versions does, as it commits nothing.
    fn keeps_the_version(self) -> bool {
        matches!(self, Self::Prune { .. })
    }

    fn action(self) -> OptimizeAction {
        match self {
            Self::Compact => OptimizeAction::Compact {
                options: CompactionOptions::default(),
                remap_options: None,
            },
            Self::Prune { older_than } => OptimizeAction::Prune {
                older_than: Some(older_than),
                delete_unverified: None,
                error_if_tagged_old_versions: None,
            },
            Self::Index => OptimizeAction::Index(OptimizeOptions::default()),
        }
    }
}

/// Points in the component's work where the unit tests observe it and make
/// the calls of [`LanceCall`] fail. The component always goes through them;
/// outside tests, [`RealHooks`] lets every call go ahead and only lets other
/// async work run between slices.
#[async_trait]
trait Hooks: Send + Sync {
    /// Called just before each call of [`LanceCall`]. An error from here is
    /// taken as that call's error.
    async fn before(&self, call: LanceCall) -> Result<(), BoxError>;
    /// Reports that one slice of a long computation is done.
    fn computed(&self, computation: Computation, len: usize);
    /// Lets other async work run between two slices of a long computation.
    async fn yield_now(&self);
    /// Reports where rebuilding a document's embedding vectors is.
    fn document_rebuild(&self, stage: DocumentRebuild);
}

struct RealHooks;

#[async_trait]
impl Hooks for RealHooks {
    async fn before(&self, _call: LanceCall) -> Result<(), BoxError> {
        Ok(())
    }

    fn computed(&self, _computation: Computation, _len: usize) {}

    fn document_rebuild(&self, _stage: DocumentRebuild) {}

    async fn yield_now(&self) {
        tokio::task::yield_now().await;
    }
}

/// How many bytes a long computation goes through at a time. Hashing the
/// largest bodies at once would keep the caller's other async work waiting for
/// a long while; between slices, the component lets that work run.
const SLICE: usize = 1 << 20;

/// Splits a long computation into slices of at most [`SLICE`] bytes: reports
/// each slice to the hooks and lets other async work run between two slices.
/// The last slice is reported when the pace is dropped.
struct Pace<'a> {
    hooks: &'a dyn Hooks,
    computation: Computation,
    in_slice: usize,
}

impl<'a> Pace<'a> {
    fn new(hooks: &'a dyn Hooks, computation: Computation) -> Self {
        Self {
            hooks,
            computation,
            in_slice: 0,
        }
    }

    /// Called before going through `len` more bytes, at most [`SLICE`]. Ends
    /// the current slice first when they would not fit in it.
    async fn before(&mut self, len: usize) {
        if self.in_slice > 0 && self.in_slice + len > SLICE {
            self.hooks.computed(self.computation, self.in_slice);
            self.in_slice = 0;
            self.hooks.yield_now().await;
        }
        self.in_slice += len;
    }
}

impl Drop for Pace<'_> {
    fn drop(&mut self) {
        if self.in_slice > 0 {
            self.hooks.computed(self.computation, self.in_slice);
        }
    }
}

/// `text` in pieces of at most [`SLICE`] bytes, split between characters.
fn pieces(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(SLICE);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (piece, after) = rest.split_at(end);
        rest = after;
        Some(piece)
    })
}

/// Whether two texts are the same, compared a slice at a time.
async fn texts_equal(text: &str, other: &str, pace: &mut Pace<'_>) -> bool {
    if text.len() != other.len() {
        return false;
    }
    let slices = text.as_bytes().chunks(SLICE);
    for (slice, other_slice) in slices.zip(other.as_bytes().chunks(SLICE)) {
        pace.before(slice.len()).await;
        if slice != other_slice {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------- the component

/// The index, kept in the LanceDB in the data directory.
struct LanceDbIndex {
    data_dir: PathBuf,
    embedding: Arc<dyn Embedding>,
    store: Store,
    hooks: Arc<dyn Hooks>,
    /// Whether the full-text search dictionary configuration has been read
    /// since the component was created. Kept apart from the store, so that
    /// discarding the connection does not make the component read it again.
    /// The lock makes concurrent first writes read it once; a failed or
    /// panicking read leaves it unread, and the next write reads it again.
    dictionary_read: tokio::sync::Mutex<bool>,
    /// Makes the functions that write to the tables write one at a time.
    writing: tokio::sync::Mutex<()>,
    /// Always [`KEY_BATCH_LEN`]; a field so that the unit tests can make the
    /// batches small.
    key_batch_len: usize,
    /// Always [`KEPT_ROWS_PER_COMMIT`]; a field so that the unit tests can
    /// make it small.
    kept_rows_per_commit: usize,
    /// Always [`BULK_DOCUMENTS`] and [`BULK_BYTES`]; fields so that the unit
    /// tests can make them small.
    bulk_documents: usize,
    bulk_bytes: usize,
}

/// The number of keys `delete_others` deletes in one commit of each table. A
/// deletion writes its condition, with every key in it, into the transaction
/// record of the commit, so the keys are split for the condition and the
/// record of one commit to hold at most this many keys, however many
/// documents are deleted at once. A rebuild reads the chunk rows of at most
/// this many documents at once, for the same reason.
const KEY_BATCH_LEN: usize = 1000;

/// How many chunk rows a rebuild reads at most at once when it gathers the
/// document table's vectors, unless one document has more. Each row holds
/// the chunk's text and vector, so this bounds what is held, whatever the
/// size of the documents.
const CHUNKS_PER_READ: usize = 4096;

/// The table where a rebuild keeps the chunk vectors it makes, while it
/// rebuilds, so that a rebuild that stopped goes on from them rather than
/// making them again. Its rows are the vectors of the chunk table's rows in
/// their order, from the first, with the version of the chunk table they
/// were made for; its vector column names the model.
const REBUILD_TABLE: &str = "chunk_embedding_rebuild";

/// How many chunk vectors a rebuild keeps in [`REBUILD_TABLE`] at least, in
/// one commit. A rebuild that stops partway makes again at most the vectors
/// it had not kept yet.
const KEPT_ROWS_PER_COMMIT: usize = 1000;

/// The tables to write a document to, and the embedding model of their
/// vector columns.
struct Prepared {
    tables: Tables,
    model: EmbeddingModel,
}

/// The most documents registering puts in one bulk: the documents it reads
/// the versions of, and writes, together.
const BULK_DOCUMENTS: usize = 500;

/// The most bytes of bodies and embedding vectors registering puts in one
/// bulk, which it holds from making the vectors until it has written them.
/// A document larger than this alone is a bulk of its own. Well below the
/// smallest memory limit worth setting, so that what a bulk holds stays a
/// small part of it.
const BULK_BYTES: usize = 64 << 20;

/// What registering a document comes down to after comparing its versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Write nothing: the document has not changed.
    Skip,
    /// Replace only the time of the document's row.
    ReplaceTime,
    /// Write the document and its chunks, with new embedding vectors.
    Write(DocumentOutcome),
}

/// A document of a bulk, once its versions are compared.
struct Decided {
    document: DocumentToRegister,
    hash: DocumentHash,
    time: Option<TimeMicros>,
    action: Action,
}

/// How a document's chunk rows compare with its row.
#[derive(Debug, PartialEq, Eq)]
enum ChunkRows {
    Agree,
    Disagree,
    /// Two rows have the same chunk number.
    RepeatNumbers,
}

impl LanceDbIndex {
    fn new(data_dir: PathBuf, embedding: Arc<dyn Embedding>, hooks: Arc<dyn Hooks>) -> Self {
        Self {
            store: Store::new(data_dir.clone(), hooks.clone()),
            data_dir,
            embedding,
            hooks,
            dictionary_read: tokio::sync::Mutex::new(false),
            writing: tokio::sync::Mutex::new(()),
            key_batch_len: KEY_BATCH_LEN,
            kept_rows_per_commit: KEPT_ROWS_PER_COMMIT,
            bulk_documents: BULK_DOCUMENTS,
            bulk_bytes: BULK_BYTES,
        }
    }

    /// Deletes the collection's documents other than `keep` and those under
    /// `unreadable_dirs`. The keys come from both tables, so that a key with
    /// chunk rows but no document row, left by a registration that stopped
    /// partway, is deleted too. Only the keys to delete go into the
    /// deletions' conditions, which LanceDB keeps in each version's
    /// transaction record; with no key to delete, nothing is written.
    async fn delete_other_documents(
        &self,
        collection: &CollectionName,
        keep: &HashSet<Key>,
        unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete> {
        let stopped = |deleted| move |error| PartialDelete { deleted, error };
        let Some(tables) = self.store.tables_to_read().await.map_err(stopped(0))? else {
            return Ok(0);
        };
        let mut keys = BTreeSet::new();
        for (table, name) in tables.each() {
            let read = self.read_keys(table, name, collection, &mut keys).await;
            read.map_err(stopped(0))?;
        }
        keys.retain(|key| !keep.contains(key) && !in_any_dir(key, unreadable_dirs));
        let keys: Vec<Key> = keys.into_iter().collect();
        let mut deleted = 0;
        for batch in keys.chunks(self.key_batch_len) {
            let rows = keys_filter(collection, batch);
            let chunk_rows = self.delete_rows(&tables.chunk, TableName::Chunk, &rows);
            chunk_rows.await.map_err(stopped(deleted))?;
            let document_rows = self.delete_rows(&tables.document, TableName::Document, &rows);
            deleted += document_rows.await.map_err(stopped(deleted))?;
        }
        Ok(deleted)
    }

    /// Adds the keys of the collection's rows in `table`, the table `name`, to
    /// `keys`. The rows are read a batch at a time and not held: the chunk
    /// table has a document's key once for each of its chunks.
    async fn read_keys(
        &self,
        table: &lancedb::Table,
        name: TableName,
        collection: &CollectionName,
        keys: &mut BTreeSet<Key>,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(name))
            .await
            .map_err(read_error)?;
        let query = table.query().only_if_expr(collection_filter(collection));
        let query = query.select(Select::columns(&["key"]));
        let rows = query.execute().await;
        let mut rows = rows.map_err(|error| read_error(error.into()))?;
        while let Some(batch) = rows
            .try_next()
            .await
            .map_err(|error| read_error(error.into()))?
        {
            let values = string_column(&batch, "key").map_err(read_error)?;
            keys.extend(values.iter().flatten().map(|key| Key(key.to_owned())));
        }
        Ok(())
    }

    /// Deletes the document's rows, the chunk rows first.
    /// `IndexError::DocumentNotFound` when the document table has no row for
    /// it, even if the chunk table has.
    async fn delete_one_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<(), IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Err(IndexError::DocumentNotFound);
        };
        if !self
            .document_row_exists(&tables.document, collection, key)
            .await?
        {
            return Err(IndexError::DocumentNotFound);
        }
        let rows = document_filter(collection, key);
        self.delete_rows(&tables.chunk, TableName::Chunk, &rows)
            .await?;
        self.delete_rows(&tables.document, TableName::Document, &rows)
            .await?;
        Ok(())
    }

    /// Whether the document has a row in the document table. A key with chunk
    /// rows only is not a document.
    async fn document_exists(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<bool, IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Ok(false);
        };
        self.document_row_exists(&tables.document, collection, key)
            .await
    }

    async fn document_row_exists(
        &self,
        document_table: &lancedb::Table,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<bool, IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Document))
            .await
            .map_err(read_error)?;
        let query = document_table.query();
        let query = query.only_if_expr(document_filter(collection, key));
        let query = query.select(Select::columns(&["key"])).limit(1);
        any_row(query).await.map_err(read_error)
    }

    /// The number of rows of each collection in the document table, read
    /// once.
    async fn count_documents(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Ok(BTreeMap::new());
        };
        self.count_document_rows(&tables.document, None).await
    }

    /// The number of rows of each collection in the document table that
    /// `filter` picks, or in all of it.
    async fn count_document_rows(
        &self,
        table: &lancedb::Table,
        filter: Option<DfExpr>,
    ) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        let mut counts = BTreeMap::new();
        self.hooks
            .before(LanceCall::ReadRows(TableName::Document))
            .await
            .map_err(read_error)?;
        let query = filtered(table.query(), filter).select(Select::columns(&["collection"]));
        let rows = query.execute().await;
        let mut rows = rows.map_err(|error| read_error(error.into()))?;
        while let Some(batch) = rows
            .try_next()
            .await
            .map_err(|error| read_error(error.into()))?
        {
            let names = string_column(&batch, "collection").map_err(read_error)?;
            for name in names.iter().flatten() {
                *counts.entry(CollectionName(name.to_owned())).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// Maintains both tables, each with the steps of [`MaintenanceStep`] in
    /// order. Nothing is done before the tables exist.
    ///
    /// While the rebuild table keeps the vectors of a rebuild that stopped,
    /// the chunk table is only cleaned up: merging its fragments or indexing
    /// its new rows makes a new version of it, and a rebuild uses the kept
    /// vectors only for the version they were made for.
    async fn maintain_tables(&self) -> Result<(), IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Ok(());
        };
        let rebuilding = self.open_rebuild_table().await?.is_some();
        for (table, name) in tables.each() {
            let steps = MaintenanceStep::all().into_iter();
            let keep_version = rebuilding && name == TableName::Chunk;
            for step in steps.filter(|step| !keep_version || step.keeps_the_version()) {
                self.hooks
                    .before(LanceCall::Optimize(name, step))
                    .await
                    .map_err(write_error)?;
                let optimized = table.optimize(step.action()).await;
                optimized.map_err(|error| write_error(error.into()))?;
            }
        }
        Ok(())
    }

    /// The state of the index in `scope`, compared with the model of
    /// `compare_with`. The tables are opened anew and pinned to their latest
    /// versions, so that the state of a table's vector columns and its counts
    /// come from one version of it, whatever another process writes
    /// meanwhile. Making no embedding vectors, it asks `compare_with` for its
    /// model only when `check` says to.
    async fn index_status(
        &self,
        scope: &StatusScope,
        compare_with: &dyn Embedding,
        check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError> {
        let counts = match self.store.pinned().await? {
            Some(tables) => self.count_for_status(&tables, scope).await?,
            None => Counts::default(),
        };
        let compared_name = compare_with.model_name();
        let checks = match check {
            DimensionCheck::Always => true,
            DimensionCheck::OnlyIfSameName => counts
                .model
                .as_ref()
                .is_some_and(|model| model.name == compared_name),
            DimensionCheck::Never => false,
        };
        let dimension = match checks {
            true => match compare_with.model().await {
                Ok(model) => DimensionResult::Known(model.dimension),
                Err(error) => DimensionResult::Unavailable(error),
            },
            false => DimensionResult::NotChecked,
        };
        Ok(counts.into_status(compared_name, dimension))
    }

    /// Counts the chunks and documents of `scope` in the tables.
    async fn count_for_status(
        &self,
        tables: &Tables,
        scope: &StatusScope,
    ) -> Result<Counts, IndexError> {
        let filter = match scope {
            StatusScope::All => None,
            StatusScope::Collections(names) if names.is_empty() => return Ok(Counts::default()),
            StatusScope::Collections(names) => Some(collections_filter(names)),
        };
        let columns = Columns::read(tables).await?;
        // Partway through switching the vector columns, a table lacks the
        // column it reads, or the two tables' columns name different models,
        // and no chunk counts as having a vector.
        let model = columns
            .chunk
            .read
            .filter(|model| columns.document.read.as_ref() == Some(model));
        let chunks = self.count_chunks(&tables.chunk, filter.clone(), model.is_some());
        let chunks = chunks.await?;
        let documents = self.count_document_rows(&tables.document, filter).await?;
        let with_vector = chunks
            .values()
            .map(|counts| counts.with_vector)
            .sum::<u64>();
        Ok(Counts {
            model: model.filter(|_| with_vector > 0),
            documents: documents.values().sum(),
            chunks,
        })
    }

    /// The chunk rows that `filter` picks, or all of them, counted by
    /// collection. With `with_vectors`, the chunks with a vector and those
    /// whose vector does not cover the whole chunk are counted too, from
    /// `covers_whole`: it is null exactly when `vector` is, and reading it
    /// is much cheaper than reading the vectors.
    async fn count_chunks(
        &self,
        table: &lancedb::Table,
        filter: Option<DfExpr>,
        with_vectors: bool,
    ) -> Result<BTreeMap<CollectionName, ChunkCounts>, IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Chunk))
            .await
            .map_err(read_error)?;
        let columns: &[&str] = match with_vectors {
            true => &["collection", "covers_whole"],
            false => &["collection"],
        };
        let query = filtered(table.query(), filter).select(Select::columns(columns));
        let rows = query.execute().await;
        let mut rows = rows.map_err(|error| read_error(error.into()))?;
        let mut counts: BTreeMap<CollectionName, ChunkCounts> = BTreeMap::new();
        while let Some(batch) = rows
            .try_next()
            .await
            .map_err(|error| read_error(error.into()))?
        {
            let names = string_column(&batch, "collection").map_err(read_error)?;
            let covers = match with_vectors {
                true => Some(boolean_column(&batch, "covers_whole").map_err(read_error)?),
                false => None,
            };
            for row in 0..batch.num_rows() {
                let name = CollectionName(names.value(row).to_owned());
                let counted = counts.entry(name).or_default();
                counted.chunks += 1;
                if let Some(covers) = covers.filter(|covers| covers.is_valid(row)) {
                    counted.with_vector += 1;
                    counted.truncated += u64::from(!covers.value(row));
                }
            }
        }
        Ok(counts)
    }

    /// Whether documents can be registered now, as [`Self::check_columns`]
    /// decides. While no vector column of either table has a value, the
    /// vector columns are made for the configured model instead, as no
    /// vector could be mixed with another model's.
    async fn check_tables_ingestable(&self) -> Result<(), IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Ok(());
        };
        let columns = Columns::read(&tables).await?;
        let configured = self.embedding.model().await;
        let ready = !columns.mid_switch()
            && columns
                .read_model()
                .is_some_and(|model| self.is_configured(&configured, model));
        if !ready && !self.has_vectors(&tables, &columns).await? {
            // Making the columns needs the configured model's dimension, as
            // creating the tables does.
            let model = configured.map_err(IndexError::Embedding)?;
            return self
                .make_columns_without_values(&tables, columns, &model)
                .await;
        }
        let checked = self.check_columns(&tables, columns, &configured).await;
        checked.map(|_| ())
    }

    /// Checks, before documents are written, that the vector columns hold
    /// vectors of the configured model only, and returns that model:
    ///
    /// - Partway through switching the vector columns (see
    ///   [`Columns::mid_switch`]), the switch is finished and the rebuild
    ///   table dropped when every vector column is of the configured model;
    ///   otherwise the rebuild has not finished.
    /// - Otherwise, the model the tables read must be the configured one, and
    ///   what a stopped rebuild left (new vector columns, the rebuild table)
    ///   is dropped: the rows written next have no value in the new columns,
    ///   so a rebuild could not go on with them.
    ///
    /// When the state of the columns makes it return an error (a rebuild has
    /// not finished, the model differs), nothing is written.
    async fn check_columns(
        &self,
        tables: &Tables,
        mut columns: Columns,
        configured: &Result<EmbeddingModel, EmbeddingError>,
    ) -> Result<EmbeddingModel, IndexError> {
        if columns.mid_switch() {
            if !columns.can_finish_switch(|model| self.is_configured(configured, model)) {
                return Err(IndexError::RebuildIncomplete);
            }
            self.finish_switch(tables, &mut columns).await?;
            if self.open_rebuild_table().await?.is_some() {
                self.drop_rebuild_table().await?;
            }
            let model = columns.read_model().cloned();
            return model.ok_or(IndexError::RebuildIncomplete);
        }
        let Some(model) = columns.read_model().cloned() else {
            return Err(IndexError::RebuildIncomplete);
        };
        if !self.is_configured(configured, &model) {
            return Err(self.model_mismatch(configured, model));
        }
        let left = self.open_rebuild_table().await?;
        self.drop_leftovers(tables, &mut columns, left).await?;
        Ok(model)
    }

    /// Makes both tables' vector columns for `model`, while no vector column
    /// has a value: no embedding vector is made. What a stopped rebuild left
    /// is dropped first.
    async fn make_columns_without_values(
        &self,
        tables: &Tables,
        mut columns: Columns,
        model: &EmbeddingModel,
    ) -> Result<(), IndexError> {
        let left = self.open_rebuild_table().await?;
        self.drop_leftovers(tables, &mut columns, left).await?;
        let mut remade = Vec::new();
        for (table, name) in tables.each() {
            if columns.of(name).read.as_ref() == Some(model) {
                continue;
            }
            self.add_null_columns(table, name, model).await?;
            self.index_new_vector_column(table, name, model).await?;
            remade.push((table, name));
        }
        for (table, name) in remade {
            let has_read = columns.of(name).read.is_some();
            self.switch_to_new_columns(table, name, has_read).await?;
        }
        Ok(())
    }

    /// Whether any vector column of either table, read or new, has a value.
    async fn has_vectors(&self, tables: &Tables, columns: &Columns) -> Result<bool, IndexError> {
        for (table, name) in tables.each() {
            let present = columns.of(name);
            let vector_columns = [
                present.read.is_some().then_some(name.vector_column()),
                present.new.is_some().then_some(name.new_vector_column()),
            ];
            for column in vector_columns.into_iter().flatten() {
                self.hooks
                    .before(LanceCall::ReadRows(name))
                    .await
                    .map_err(read_error)?;
                let query = table.query().only_if_expr(col(column).is_not_null());
                let query = query.select(Select::columns(&["key"])).limit(1);
                if any_row(query).await.map_err(read_error)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Rebuilds the embedding vectors with the configured model, and returns
    /// the number of chunk rows once the vector columns are rebuilt or
    /// switched over; 0 when nothing was rebuilt.
    ///
    /// A table is rebuilt in four steps: its new vector columns are added
    /// with their values in one commit (1 for the chunk table, made by the
    /// model; 2 for the document table, gathered from the chunk table's
    /// vectors), they are indexed (3), and the columns read are dropped and
    /// the new ones take their names (4). Step 3 is taken on both tables
    /// before step 4 on either, so that a switch that stopped is finished by
    /// dropping and renaming alone.
    ///
    /// What another model left (new columns, the rebuild table) is dropped
    /// first. Then, without `include_configured_model`, each table goes on
    /// from where it is ([`Remaining`]), and nothing is rebuilt when both
    /// tables read the configured model, beyond finishing a switch and
    /// dropping what is left. With it, a switch is finished, what is left
    /// dropped, and both tables rebuilt from the start.
    async fn rebuild(
        &self,
        include_configured_model: bool,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        let Some(tables) = self.store.tables_to_read().await? else {
            return Ok(0);
        };
        let model = self.embedding.model().await;
        let model = model.map_err(IndexError::Embedding)?;
        let mut columns = Columns::read(&tables).await?;
        let mut kept = self.open_rebuild_table().await?;
        self.drop_other_models(&tables, &mut columns, &mut kept, &model)
            .await?;
        let remaining = match include_configured_model {
            true => [Remaining::Fill; 2],
            false => [
                Remaining::of(&columns.chunk, &model),
                Remaining::of(&columns.document, &model),
            ],
        };
        let read_by_both = columns.read_model() == Some(&model);
        if include_configured_model || read_by_both {
            self.finish_switch(&tables, &mut columns).await?;
            self.drop_leftovers(&tables, &mut columns, kept.take())
                .await?;
            if !include_configured_model {
                return Ok(0);
            }
        }
        let rebuilding = Rebuilding {
            tables: &tables,
            columns: &columns,
            model: &model,
            remaining,
        };
        self.rebuild_remaining(&rebuilding, kept, parallelism, progress)
            .await
    }

    /// Takes the steps `rebuilding` has left, and drops the rebuild table.
    async fn rebuild_remaining(
        &self,
        rebuilding: &Rebuilding<'_>,
        kept: Option<lancedb::Table>,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        let Rebuilding { tables, model, .. } = rebuilding;
        let total = self.count_rows(&tables.chunk, TableName::Chunk).await?;
        let [chunk, document] = rebuilding.remaining;
        if chunk == Remaining::Fill {
            let filled =
                self.fill_chunk_vectors(&tables.chunk, model, kept, total, parallelism, progress);
            filled.await?;
        } else {
            Tally::new(progress, count_of(total), count_of(total));
        }
        if document == Remaining::Fill {
            // The chunk table's vectors to gather: those made for it in this
            // rebuild, unless it already read the model's.
            let source = match chunk {
                Remaining::Nothing => TableName::Chunk.vector_column(),
                Remaining::Switch | Remaining::Fill => TableName::Chunk.new_vector_column(),
            };
            self.fill_document_vectors(tables, model, source).await?;
        }
        let rebuilt = || {
            tables
                .each()
                .into_iter()
                .filter(|(_, name)| rebuilding.remaining_of(*name) != Remaining::Nothing)
        };
        for (table, name) in rebuilt() {
            self.index_new_vector_column(table, name, model).await?;
        }
        for (table, name) in rebuilt() {
            let has_read = rebuilding.columns.of(name).read.is_some();
            self.switch_to_new_columns(table, name, has_read).await?;
        }
        self.drop_rebuild_table().await?;
        Ok(count_of(total))
    }

    /// Drops the new vector columns of the tables, and the rebuild table
    /// `kept`, whose model is not `model`: a rebuild with another model left
    /// them.
    async fn drop_other_models(
        &self,
        tables: &Tables,
        columns: &mut Columns,
        kept: &mut Option<lancedb::Table>,
        model: &EmbeddingModel,
    ) -> Result<(), IndexError> {
        for (table, name) in tables.each() {
            let present = columns.of_mut(name);
            if present.new.as_ref().is_some_and(|new| new != model) {
                self.drop_columns(table, name, name.new_vector_columns())
                    .await?;
                present.new = None;
            }
        }
        if let Some(table) = kept {
            let kept_model = self.kept_model(table).await?;
            if kept_model != *model {
                self.drop_rebuild_table().await?;
                *kept = None;
            }
        }
        Ok(())
    }

    /// Finishes switching the vector columns when they are partway through
    /// it ([`Columns::mid_switch`]): each table with new vector columns
    /// switches to them, as step 4 of a rebuild does. Their indexes are made
    /// first if a table lacks one.
    async fn finish_switch(
        &self,
        tables: &Tables,
        columns: &mut Columns,
    ) -> Result<(), IndexError> {
        if !columns.mid_switch() {
            return Ok(());
        }
        for (table, name) in tables.each() {
            if let Some(new) = columns.of(name).new.clone() {
                self.index_new_vector_column(table, name, &new).await?;
            }
        }
        for (table, name) in tables.each() {
            let present = columns.of_mut(name);
            if let Some(new) = present.new.take() {
                self.switch_to_new_columns(table, name, present.read.is_some())
                    .await?;
                present.read = Some(new);
            }
        }
        Ok(())
    }

    /// Drops what a stopped rebuild left: the new vector columns, and the
    /// rebuild table `kept`, if any.
    async fn drop_leftovers(
        &self,
        tables: &Tables,
        columns: &mut Columns,
        kept: Option<lancedb::Table>,
    ) -> Result<(), IndexError> {
        for (table, name) in tables.each() {
            if columns.of_mut(name).new.take().is_some() {
                self.drop_columns(table, name, name.new_vector_columns())
                    .await?;
            }
        }
        if kept.is_some() {
            self.drop_rebuild_table().await?;
        }
        Ok(())
    }

    /// Step 1 of a rebuild: adds the chunk table's new vector columns with
    /// their values, in one commit. The values are streamed into the
    /// addition as they are made: the rows are read in order, and the
    /// vectors of each document's rows (consecutive rows of the same
    /// document) made, up to `parallelism` documents at a time (one when it
    /// is 0). The vectors are kept in the rebuild table as they are made,
    /// and those kept for this version of the chunk table are streamed in
    /// rather than made again. `total` is the number of chunk rows.
    async fn fill_chunk_vectors(
        &self,
        table: &lancedb::Table,
        model: &EmbeddingModel,
        kept: Option<lancedb::Table>,
        total: usize,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<(), IndexError> {
        let version = table.version().await;
        let version = version.map_err(|error| read_error(error.into()))?;
        let kept = self.kept_for(kept, model, version).await?;
        let tally = Tally::new(progress, count_of(kept.rows), count_of(total));
        if total == 0 {
            return self.add_null_columns(table, TableName::Chunk, model).await;
        }
        let filling = Filling {
            table,
            model,
            kept: &kept,
            version,
            parallelism,
            tally: &tally,
        };
        let adding = self.add_streamed_columns(table, TableName::Chunk, model, |values| {
            self.stream_chunk_vectors(&filling, values)
        });
        adding.await
    }

    /// The rebuild table to keep the chunk vectors in, with how many it
    /// keeps: `kept` when it holds vectors for `version` of the chunk table,
    /// or none; otherwise a new one, after dropping `kept`.
    async fn kept_for(
        &self,
        kept: Option<lancedb::Table>,
        model: &EmbeddingModel,
        version: u64,
    ) -> Result<Kept, IndexError> {
        if let Some(table) = kept {
            let rows = self.kept_rows(&table).await?;
            let made_for = self.kept_version(&table).await?;
            if made_for.is_none_or(|made_for| made_for == version) {
                return Ok(Kept { table, rows });
            }
            self.drop_rebuild_table().await?;
        }
        let table = self.create_rebuild_table(model).await?;
        Ok(Kept { table, rows: 0 })
    }

    /// Streams the values of the chunk table's new vector columns into
    /// `values`, in the order of its rows: first those the rebuild table
    /// keeps, then those it makes, keeping them in the rebuild table as it
    /// goes. Returns when `values` is closed, as when adding the columns
    /// failed; what was made is kept all the same.
    async fn stream_chunk_vectors(
        &self,
        filling: &Filling<'_>,
        values: tokio::sync::mpsc::Sender<RecordBatch>,
    ) -> Result<(), IndexError> {
        let schema = TableName::Chunk.new_columns_schema(filling.model);
        let schema = schema.map_err(write_error)?;
        if filling.kept.rows > 0 {
            self.hooks
                .before(LanceCall::RebuildTable(RebuildTableCall::Read))
                .await
                .map_err(read_error)?;
            let columns = TableName::Chunk.new_vector_columns();
            let query = filling.kept.table.query().select(Select::columns(columns));
            let rows = query.execute().await;
            let mut rows = rows.map_err(|error| read_error(error.into()))?;
            while let Some(batch) = rows
                .try_next()
                .await
                .map_err(|error| read_error(error.into()))?
            {
                let batch = RecordBatch::try_new(schema.clone(), batch.columns().to_vec());
                let batch = batch.map_err(|error| read_error(error.into()))?;
                if values.send(batch).await.is_err() {
                    return Ok(());
                }
            }
        }
        let mut to_keep = ToKeep::default();
        let streamed = self.stream_made_chunk_vectors(filling, &values, &mut to_keep);
        let streamed = streamed.await;
        let kept = self.keep(filling, to_keep.take()).await;
        streamed.and(kept)
    }

    /// Makes the vectors of the chunk rows after those the rebuild table
    /// keeps, streams them into `values`, and keeps them every
    /// [`Self::kept_rows_per_commit`] rows or more.
    async fn stream_made_chunk_vectors(
        &self,
        filling: &Filling<'_>,
        values: &tokio::sync::mpsc::Sender<RecordBatch>,
        to_keep: &mut ToKeep,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Chunk))
            .await
            .map_err(read_error)?;
        let columns = ["collection", "key", "text"];
        let query = filling.table.query().select(Select::columns(&columns));
        let rows = query.execute().await;
        let rows = rows.map_err(|error| read_error(error.into()))?;
        let documents = documents_in_chunk_rows(rows, filling.kept.rows);
        let made = documents.map(|texts| async move {
            let texts = texts?;
            self.make_document_vectors(&texts, filling.model, filling.tally)
                .await
        });
        let made = made.buffered(filling.parallelism.max(1));
        futures::pin_mut!(made);
        while let Some(made) = made.next().await {
            let mut pace = Pace::new(self.hooks.as_ref(), Computation::Build);
            let batch = chunk_vector_columns(filling.model, &made?, &mut pace).await;
            let batch = batch.map_err(write_error)?;
            drop(pace);
            to_keep.push(batch.clone());
            if to_keep.rows >= self.kept_rows_per_commit {
                self.keep(filling, to_keep.take()).await?;
            }
            if values.send(batch).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Makes the vectors of one document's chunk texts, as many at a time as
    /// `embedding` takes, adding each call's chunks to `tally`.
    async fn make_document_vectors(
        &self,
        texts: &[String],
        model: &EmbeddingModel,
        tally: &Tally<'_>,
    ) -> Result<Vec<ChunkEmbedding>, IndexError> {
        self.hooks.document_rebuild(DocumentRebuild::Started);
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let made = self.embed_texts(&texts, |made| {
            let mismatched = made
                .iter()
                .find(|embedding| embedding.vector.len() != model.dimension);
            if let Some(mismatched) = mismatched {
                return Err(dimension_changed(model, mismatched.vector.len()));
            }
            tally.add(count_of(made.len()));
            Ok(())
        });
        let made = made.await?;
        self.hooks.document_rebuild(DocumentRebuild::Finished);
        Ok(made)
    }

    /// Adds `batches` of chunk vectors to the rebuild table, in one commit.
    async fn keep(
        &self,
        filling: &Filling<'_>,
        batches: Vec<RecordBatch>,
    ) -> Result<(), IndexError> {
        if batches.is_empty() {
            return Ok(());
        }
        let schema = rebuild_table_schema(filling.model).map_err(write_error)?;
        let mut rows = Vec::with_capacity(batches.len());
        for batch in batches {
            let versions = UInt64Array::from(vec![filling.version; batch.num_rows()]);
            let mut columns: Vec<ArrayRef> = vec![Arc::new(versions)];
            columns.extend(batch.columns().iter().cloned());
            let row = RecordBatch::try_new(schema.clone(), columns);
            rows.push(row.map_err(|error| write_error(error.into()))?);
        }
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Add))
            .await
            .map_err(write_error)?;
        let added = filling.kept.table.add(rows).execute().await;
        added.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Step 2 of a rebuild: adds the document table's new vector column with
    /// its values, in one commit. A document's value is the vectors of its
    /// chunk rows in the chunk table's column `source`, in chunk order; null
    /// for a document without chunks, and for one whose chunk rows disagree
    /// with its row, as when registering it stopped between the two tables.
    /// No embedding vector is made. The chunk table's rows that are not in
    /// its indexes yet are added to them first, since the chunk rows are
    /// read a few documents at a time.
    async fn fill_document_vectors(
        &self,
        tables: &Tables,
        model: &EmbeddingModel,
        source: &'static str,
    ) -> Result<(), IndexError> {
        let step = MaintenanceStep::Index;
        self.hooks
            .before(LanceCall::Optimize(TableName::Chunk, step))
            .await
            .map_err(write_error)?;
        let indexed = tables.chunk.optimize(step.action()).await;
        indexed.map_err(|error| write_error(error.into()))?;
        let documents = self.count_rows(&tables.document, TableName::Document);
        if documents.await? == 0 {
            let table = &tables.document;
            return self
                .add_null_columns(table, TableName::Document, model)
                .await;
        }
        let adding =
            self.add_streamed_columns(&tables.document, TableName::Document, model, |values| {
                self.stream_document_vectors(tables, model, source, values)
            });
        adding.await
    }

    /// Streams the values of the document table's new vector column into
    /// `values`, in the order of its rows, reading the chunk rows of up to
    /// [`Self::key_batch_len`] documents of one collection at a time. Returns
    /// when `values` is closed.
    async fn stream_document_vectors(
        &self,
        tables: &Tables,
        model: &EmbeddingModel,
        source: &'static str,
        values: tokio::sync::mpsc::Sender<RecordBatch>,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Document))
            .await
            .map_err(read_error)?;
        let columns = ["collection", "key", "body", "chunk_ranges"];
        let query = tables.document.query().select(Select::columns(&columns));
        let rows = query.execute().await;
        let mut rows = rows.map_err(|error| read_error(error.into()))?;
        while let Some(batch) = rows
            .try_next()
            .await
            .map_err(|error| read_error(error.into()))?
        {
            let runs = collection_runs(&batch, self.key_batch_len).map_err(read_error)?;
            for run in runs {
                let gathered = self.gather_document_vectors(tables, model, source, &batch, run);
                let gathered = gathered.await?;
                if values.send(gathered).await.is_err() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// The values of the document table's new vector column for the rows
    /// `run` of `documents`, all of one collection.
    async fn gather_document_vectors(
        &self,
        tables: &Tables,
        model: &EmbeddingModel,
        source: &'static str,
        documents: &RecordBatch,
        run: Range<usize>,
    ) -> Result<RecordBatch, IndexError> {
        let names = string_column(documents, "collection").map_err(read_error)?;
        let keys = string_column(documents, "key").map_err(read_error)?;
        let bodies = string_column(documents, "body").map_err(read_error)?;
        let ranges = column(documents, "chunk_ranges").map_err(read_error)?;
        let ranges = ranges.as_list_opt::<i32>();
        let ranges = ranges.ok_or_else(|| read_error(unexpected_type("chunk_ranges")))?;
        let collection = CollectionName(names.value(run.start).to_owned());
        let run_keys: Vec<Key> = run
            .clone()
            .map(|row| Key(keys.value(row).to_owned()))
            .collect();
        self.hooks
            .before(LanceCall::ReadRows(TableName::Chunk))
            .await
            .map_err(read_error)?;
        let columns = [
            "key",
            "chunk_index",
            "range_start",
            "range_end",
            "text",
            source,
        ];
        let chunk_rows = read_rows(&tables.chunk, keys_filter(&collection, &run_keys), &columns);
        let chunk_rows = chunk_rows.await.map_err(read_error)?;
        let by_key = rows_by_key(&chunk_rows).map_err(read_error)?;
        let mut gathered = GatheredVectors::default();
        for row in run {
            let ranges = chunk_ranges_of(&ranges.value(row)).map_err(read_error)?;
            let found = by_key.get(keys.value(row)).map_or(&[][..], Vec::as_slice);
            let vectors =
                self.vectors_in_order(&chunk_rows, found, &ranges, bodies.value(row), source);
            gathered.push(vectors.await.map_err(read_error)?);
        }
        gathered.into_batch(model).map_err(write_error)
    }

    /// The vectors of a document's chunk rows, at `found` in `chunk_rows`,
    /// in chunk order; `None` when the document has no chunks, when its
    /// chunk rows disagree with its row (whose chunk ranges and body are
    /// `ranges` and `body`), and when a chunk row has no vector.
    async fn vectors_in_order(
        &self,
        chunk_rows: &[RecordBatch],
        found: &[(usize, u32)],
        ranges: &[Range<u64>],
        body: &str,
        source: &str,
    ) -> Result<Option<Vec<ArrayRef>>, BoxError> {
        if ranges.is_empty() {
            return Ok(None);
        }
        let rows = rows_at(chunk_rows, found)?;
        let compared = compare_chunk_rows(&rows, ranges, body, self.hooks.as_ref());
        if compared.await? != ChunkRows::Agree {
            return Ok(None);
        }
        let mut ordered: Vec<Option<ArrayRef>> = vec![None; ranges.len()];
        for batch in &rows {
            let numbers = primitive_column::<UInt32Type>(batch, "chunk_index")?;
            let vectors = column(batch, source)?.as_fixed_size_list_opt();
            let vectors = vectors.ok_or_else(|| unexpected_type(source))?;
            for row in 0..batch.num_rows() {
                if vectors.is_null(row) {
                    return Ok(None);
                }
                ordered[usize::try_from(numbers.value(row))?] = Some(vectors.value(row));
            }
        }
        Ok(ordered.into_iter().collect())
    }

    /// Adds the table's new vector columns with the values streamed into the
    /// sender that `stream` is given, in one commit. Making the values and
    /// adding them go on together in this task: LanceDB takes the values as
    /// they come. When making them fails, the addition is given fewer values
    /// than the table has rows, so it fails and writes nothing, and the
    /// error making them is returned.
    async fn add_streamed_columns<F, Made>(
        &self,
        table: &lancedb::Table,
        name: TableName,
        model: &EmbeddingModel,
        stream: F,
    ) -> Result<(), IndexError>
    where
        F: FnOnce(tokio::sync::mpsc::Sender<RecordBatch>) -> Made,
        Made: Future<Output = Result<(), IndexError>>,
    {
        let schema = name.new_columns_schema(model).map_err(write_error)?;
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let values = futures::stream::unfold(receiver, |mut receiver| async move {
            let batch = receiver.recv().await?;
            Some((Ok::<_, lancedb::Error>(batch), receiver))
        });
        let values: SendableRecordBatchStream =
            Box::pin(SimpleRecordBatchStream::new(values, schema));
        let adding = async {
            self.hooks
                .before(LanceCall::AddColumns(name))
                .await
                .map_err(write_error)?;
            let adding = table.add_columns();
            let adding = adding.transform(NewColumnTransform::Stream(values.into_df_stream()));
            let added = adding.execute().await;
            added.map_err(|error| write_error(error.into()))?;
            Ok(())
        };
        let (streamed, added) = futures::future::join(stream(sender), adding).await;
        streamed.and(added)
    }

    /// Adds the table's new vector columns for `model`, all null, in one
    /// commit.
    async fn add_null_columns(
        &self,
        table: &lancedb::Table,
        name: TableName,
        model: &EmbeddingModel,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::AddColumns(name))
            .await
            .map_err(write_error)?;
        let schema = name.new_columns_schema(model).map_err(write_error)?;
        let adding = table.add_columns();
        let adding = adding.transform(NewColumnTransform::AllNulls(schema));
        let added = adding.execute().await;
        added.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    async fn count_rows(
        &self,
        table: &lancedb::Table,
        name: TableName,
    ) -> Result<usize, IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(name))
            .await
            .map_err(read_error)?;
        let counted = table.count_rows(None).await;
        counted.map_err(|error| read_error(error.into()))
    }

    /// The rebuild table; `None` when there is none.
    async fn open_rebuild_table(&self) -> Result<Option<lancedb::Table>, IndexError> {
        let Some(connection) = self.store.connection_to_read().await? else {
            return Ok(None);
        };
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Open))
            .await
            .map_err(read_error)?;
        match connection.open_table(REBUILD_TABLE).execute().await {
            Ok(table) => Ok(Some(table)),
            Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
            Err(error) => Err(read_error(error.into())),
        }
    }

    async fn create_rebuild_table(
        &self,
        model: &EmbeddingModel,
    ) -> Result<lancedb::Table, IndexError> {
        let connection = self.store.connection_to_read().await?;
        let connection =
            connection.ok_or_else(|| write_error("the index has no LanceDB directory".into()))?;
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Create))
            .await
            .map_err(write_error)?;
        let schema = rebuild_table_schema(model).map_err(write_error)?;
        let created = connection.create_empty_table(REBUILD_TABLE, schema);
        created
            .execute()
            .await
            .map_err(|error| write_error(error.into()))
    }

    /// Drops the rebuild table, if there is one.
    async fn drop_rebuild_table(&self) -> Result<(), IndexError> {
        let Some(connection) = self.store.connection_to_read().await? else {
            return Ok(());
        };
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Drop))
            .await
            .map_err(write_error)?;
        match connection.drop_table(REBUILD_TABLE, &[]).await {
            Ok(()) | Err(lancedb::Error::TableNotFound { .. }) => Ok(()),
            Err(error) => Err(write_error(error.into())),
        }
    }

    /// The model of the vectors the rebuild table keeps.
    async fn kept_model(&self, table: &lancedb::Table) -> Result<EmbeddingModel, IndexError> {
        let schema = table.schema().await;
        let schema = schema.map_err(|error| read_error(error.into()))?;
        let field = schema.field_with_name(TableName::Chunk.new_vector_column());
        let field = field.map_err(|error| read_error(error.into()))?;
        model_of_field(field).map_err(read_error)
    }

    async fn kept_rows(&self, table: &lancedb::Table) -> Result<usize, IndexError> {
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Read))
            .await
            .map_err(read_error)?;
        let counted = table.count_rows(None).await;
        counted.map_err(|error| read_error(error.into()))
    }

    /// The version of the chunk table the rebuild table's vectors were made
    /// for; `None` when it keeps none. Its rows are only ever added for one
    /// version, so the first row tells.
    async fn kept_version(&self, table: &lancedb::Table) -> Result<Option<u64>, IndexError> {
        self.hooks
            .before(LanceCall::RebuildTable(RebuildTableCall::Read))
            .await
            .map_err(read_error)?;
        let query = table
            .query()
            .select(Select::columns(&["chunk_version"]))
            .limit(1);
        let rows: Result<Vec<RecordBatch>, _> = match query.execute().await {
            Ok(rows) => rows.try_collect().await,
            Err(error) => Err(error),
        };
        let rows = rows.map_err(|error| read_error(error.into()))?;
        let Some(batch) = rows.iter().find(|batch| batch.num_rows() > 0) else {
            return Ok(None);
        };
        let versions = primitive_column::<UInt64Type>(batch, "chunk_version");
        Ok(Some(versions.map_err(read_error)?.value(0)))
    }

    /// Creates the vector index of the table's new vector column, unless it
    /// has one. LanceDB records the index of a column whose vectors are all
    /// null, and trains it once maintenance finds enough vectors.
    async fn index_new_vector_column(
        &self,
        table: &lancedb::Table,
        name: TableName,
        model: &EmbeddingModel,
    ) -> Result<(), IndexError> {
        let column = name.new_vector_column();
        let existing = table.list_indices().await;
        let existing = existing.map_err(|error| read_error(error.into()))?;
        if existing.iter().any(|index| index.columns == [column]) {
            return Ok(());
        }
        let params = VectorIndexParams::for_dimension(model.dimension);
        self.hooks
            .before(LanceCall::CreateIndex(name))
            .await
            .map_err(write_error)?;
        let index = lancedb::index::Index::IvfPq(params.builder());
        let creating = table.create_index(&[column], index);
        creating
            .execute()
            .await
            .map_err(|error| write_error(error.into()))
    }

    async fn drop_columns(
        &self,
        table: &lancedb::Table,
        name: TableName,
        columns: &[&str],
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::DropColumns(name))
            .await
            .map_err(write_error)?;
        let dropped = table.drop_columns(columns).await;
        dropped.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Makes the table read the vectors of its new columns: drops the columns
    /// it reads, if it `has_read` them, with their index, and gives the new
    /// columns their names, which keeps the index of the new vector column.
    async fn switch_to_new_columns(
        &self,
        table: &lancedb::Table,
        name: TableName,
        has_read: bool,
    ) -> Result<(), IndexError> {
        if has_read {
            self.drop_columns(table, name, name.vector_columns())
                .await?;
        }
        self.hooks
            .before(LanceCall::RenameColumns(name))
            .await
            .map_err(write_error)?;
        let renames: Vec<ColumnAlteration> = name
            .new_vector_columns()
            .iter()
            .zip(name.vector_columns())
            .map(|(new, read)| ColumnAlteration::new((*new).to_owned()).rename((*read).to_owned()))
            .collect();
        let renamed = table.alter_columns(&renames).await;
        renamed.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Updates the columns of `rows` in the rows of `table`, the table `name`,
    /// that have the same values in the columns that find a row, in one
    /// commit. Rows that match none are not added.
    async fn update_rows(
        &self,
        table: &lancedb::Table,
        name: TableName,
        rows: Vec<RecordBatch>,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::WriteRows(name))
            .await
            .map_err(write_error)?;
        let mut merge = table.merge_insert(name.row_columns());
        merge.when_matched_update_all(None);
        let merged = merge.execute(reader(rows)).await;
        merged.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Discards the held connection and tables after an error reading or
    /// writing the index, so that the next call opens LanceDB again.
    async fn discard_after<T>(&self, result: &Result<T, IndexError>) {
        if let Err(error) = result {
            self.discard_after_error(error).await;
        }
    }

    async fn discard_after_error(&self, error: &IndexError) {
        if let IndexError::CannotRead { .. } | IndexError::CannotWrite { .. } = error {
            self.store.discard().await;
        }
    }

    /// Registers the documents of `documents` a bulk at a time: it asks for
    /// the documents of a bulk, registers them, and only then asks for the
    /// next bulk's. The vector columns are checked before the first document
    /// is asked for, and again just before each bulk is written.
    async fn register_documents(
        &self,
        mut documents: BoxStream<'_, DocumentToRegister>,
        parallelism: usize,
        results: &dyn RegistrationSink,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        self.read_dictionary().await?;
        let mut prepared = self.prepare_to_write().await?;
        let mut held = None;
        loop {
            let bulk = self.next_bulk(&mut documents, &mut held, prepared.model.dimension);
            let bulk = bulk.await;
            if bulk.is_empty() {
                return Ok(());
            }
            let registering = Registering {
                parallelism,
                results,
                progress,
            };
            self.register_bulk(&mut prepared, bulk, &registering)
                .await?;
        }
    }

    /// The documents of the next bulk: `held`, if any, then those asked for
    /// from `documents` until the bulk has [`Self::bulk_documents`]
    /// documents, or the next document is of another collection or would
    /// make the bulk larger than [`Self::bulk_bytes`]. That document is held
    /// for the bulk after. Empty when the stream has ended.
    async fn next_bulk(
        &self,
        documents: &mut BoxStream<'_, DocumentToRegister>,
        held: &mut Option<DocumentToRegister>,
        dimension: usize,
    ) -> Vec<DocumentToRegister> {
        let mut bulk: Vec<DocumentToRegister> = Vec::new();
        let mut bytes = 0;
        if let Some(document) = held.take() {
            bytes = held_bytes(&document, dimension);
            bulk.push(document);
        }
        while bulk.len() < self.bulk_documents {
            let Some(document) = documents.next().await else {
                break;
            };
            let size = held_bytes(&document, dimension);
            let fits = match bulk.first() {
                None => true,
                Some(first) => {
                    first.collection == document.collection
                        && bytes.saturating_add(size) <= self.bulk_bytes
                }
            };
            if !fits {
                *held = Some(document);
                break;
            }
            bytes = bytes.saturating_add(size);
            bulk.push(document);
        }
        bulk
    }

    /// Registers the documents of a bulk, all of one collection: reads their
    /// versions at once, decides what each needs, makes the embedding vectors
    /// of those to write, and writes them together. The outcome of a document
    /// that needs nothing is passed as soon as that is decided, and the others'
    /// once they are written. When making the vectors fails, the documents
    /// whose vectors are made are written all the same, and the first error is
    /// returned after.
    async fn register_bulk(
        &self,
        prepared: &mut Prepared,
        bulk: Vec<DocumentToRegister>,
        registering: &Registering<'_>,
    ) -> Result<(), IndexError> {
        let decided = self.decide_bulk(prepared, bulk).await?;
        for skipped in decided.iter().filter(|done| done.action == Action::Skip) {
            let document = &skipped.document;
            let results = registering.results;
            results.registered(
                &document.collection,
                &document.key,
                DocumentOutcome::Unchanged,
            );
        }
        let (made, failed) = self
            .make_bulk_vectors(prepared, &decided, registering)
            .await;
        let written = self.write_bulk(prepared, decided, made, registering.results);
        let written = written.await;
        let Some(error) = failed else {
            return written;
        };
        // The model's error stopped the registration first, so it is the one
        // returned. A failed write still drops the held connection, as it
        // does when it is returned.
        self.discard_after(&written).await;
        Err(error)
    }

    /// Places the full-text search dictionary configuration if it is not
    /// there, and reads it, unless the component has already read it.
    async fn read_dictionary(&self) -> Result<(), IndexError> {
        let mut read = self.dictionary_read.lock().await;
        if *read {
            return Ok(());
        }
        self.hooks
            .before(LanceCall::PlaceDictionary)
            .await
            .map_err(write_error)?;
        place_fts_dictionary_config(&self.data_dir).await?;
        self.hooks
            .before(LanceCall::ReadDictionary)
            .await
            .map_err(read_error)?;
        fts_index_params()
            .build()
            .map_err(|error| read_error(error.into()))?;
        *read = true;
        Ok(())
    }

    /// The tables to write to, after checking their vector columns as
    /// [`Self::check_columns`] does. Creates whatever of the tables and their
    /// indexes is missing. Writes only while no other function writes.
    async fn prepare_to_write(&self) -> Result<Prepared, IndexError> {
        let _writing = self.writing.lock().await;
        let configured = self.embedding.model().await;
        let tables = self.store.tables_to_write(configured.as_ref().ok()).await?;
        let Some(tables) = tables else {
            return Err(tables_not_created(configured));
        };
        let columns = Columns::read(&tables).await?;
        let model = self.check_columns(&tables, columns, &configured).await?;
        Ok(Prepared { tables, model })
    }

    /// Whether `indexed` is the configured model. When the configured model's
    /// dimension cannot be checked, only the names are compared: the
    /// dimension is checked when making embedding vectors, which fails the
    /// same way as checking it.
    fn is_configured(
        &self,
        configured: &Result<EmbeddingModel, EmbeddingError>,
        indexed: &EmbeddingModel,
    ) -> bool {
        match configured {
            Ok(configured) => configured == indexed,
            Err(_) => self.embedding.model_name() == indexed.name,
        }
    }

    fn model_mismatch(
        &self,
        configured: &Result<EmbeddingModel, EmbeddingError>,
        indexed: EmbeddingModel,
    ) -> IndexError {
        IndexError::ModelMismatch {
            configured_name: self.embedding.model_name().to_owned(),
            configured_dimension: configured.as_ref().ok().map(|model| model.dimension),
            indexed,
        }
    }

    /// Decides what each document of the bulk needs: reads the stored
    /// versions of all of them at once, and, for those whose body has not
    /// changed and that are not to be overwritten, their chunk rows at once,
    /// to compare them with their document rows.
    async fn decide_bulk(
        &self,
        prepared: &mut Prepared,
        bulk: Vec<DocumentToRegister>,
    ) -> Result<Vec<Decided>, IndexError> {
        let collection = bulk[0].collection.clone();
        let keys: Vec<Key> = bulk.iter().map(|document| document.key.clone()).collect();
        let stored = self.read_versions(prepared, &collection, &keys).await?;
        let mut hashed = Vec::with_capacity(bulk.len());
        for document in bulk {
            let hash = DocumentHash::of_body(&document.body, self.hooks.as_ref()).await;
            let time = document.time.map(TimeMicros::from_time);
            hashed.push((document, hash, time));
        }
        let versions_of = |key: &Key| stored.get(&key.0).map_or(&[][..], Vec::as_slice);
        let to_compare: Vec<Key> = hashed
            .iter()
            .filter(|(document, hash, _)| compares(document, hash, versions_of(&document.key)))
            .map(|(document, ..)| document.key.clone())
            .collect();
        let chunk_rows = match to_compare.is_empty() {
            true => Vec::new(),
            false => {
                let read = self.read_chunk_rows(&prepared.tables.chunk, &collection, &to_compare);
                read.await?
            }
        };
        let by_key = rows_by_key(&chunk_rows).map_err(read_error)?;
        let mut decided = Vec::with_capacity(hashed.len());
        for (document, hash, time) in hashed {
            let versions = versions_of(&document.key);
            let found = by_key
                .get(document.key.0.as_str())
                .map_or(&[][..], Vec::as_slice);
            let comparing = Comparing {
                hash: &hash,
                time,
                versions,
                chunk_rows: &chunk_rows,
                found,
            };
            let action = self.decide(&document, &comparing).await?;
            decided.push(Decided {
                document,
                hash,
                time,
                action,
            });
        }
        Ok(decided)
    }

    /// The stored versions of the documents of the collection with `keys`,
    /// by key, read in one go. When the read fails, the connection and the
    /// tables are opened again and it is read again, up to
    /// [`VERSION_READ_RETRIES`] times, before the index counts as unreadable.
    async fn read_versions(
        &self,
        prepared: &mut Prepared,
        collection: &CollectionName,
        keys: &[Key],
    ) -> Result<HashMap<String, Vec<StoredVersion>>, IndexError> {
        let mut retried = 0;
        loop {
            let read = self.try_read_versions(&prepared.tables.document, collection, keys);
            let error = match read.await {
                Ok(versions) => return Ok(versions),
                Err(error) => error,
            };
            if retried == VERSION_READ_RETRIES {
                return Err(read_error(error));
            }
            retried += 1;
            warn_versions_unreadable(collection, error.as_ref());
            prepared.tables = self.reopen_tables(&prepared.model).await?;
        }
    }

    async fn try_read_versions(
        &self,
        table: &lancedb::Table,
        collection: &CollectionName,
        keys: &[Key],
    ) -> Result<HashMap<String, Vec<StoredVersion>>, BoxError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Document))
            .await?;
        let columns = ["key", "time", "hash", "chunk_ranges"];
        let batches = read_rows(table, keys_filter(collection, keys), &columns).await?;
        let mut versions: HashMap<String, Vec<StoredVersion>> = HashMap::new();
        for batch in &batches {
            let keys = string_column(batch, "key")?;
            let stored = StoredVersion::from_rows(batch)?;
            for (row, version) in stored.into_iter().enumerate() {
                let key = keys.value(row).to_owned();
                versions.entry(key).or_default().push(version);
            }
        }
        Ok(versions)
    }

    /// Drops the connection and the tables held, and opens them again, while
    /// no other function of this component writes.
    async fn reopen_tables(&self, model: &EmbeddingModel) -> Result<Tables, IndexError> {
        self.store.discard().await;
        let _writing = self.writing.lock().await;
        let tables = self.store.tables_to_write(Some(model)).await?;
        tables.ok_or_else(|| write_error("the tables could not be opened again".into()))
    }

    /// The chunk rows of the documents of the collection with `keys`, with
    /// the columns that are compared with the document rows, read in one go.
    async fn read_chunk_rows(
        &self,
        table: &lancedb::Table,
        collection: &CollectionName,
        keys: &[Key],
    ) -> Result<Vec<RecordBatch>, IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Chunk))
            .await
            .map_err(read_error)?;
        let columns = ["key", "chunk_index", "range_start", "range_end", "text"];
        let read = read_rows(table, keys_filter(collection, keys), &columns).await;
        read.map_err(read_error)
    }

    /// What registering `document` comes down to, as `comparing` finds.
    async fn decide(
        &self,
        document: &DocumentToRegister,
        comparing: &Comparing<'_>,
    ) -> Result<Action, IndexError> {
        let [stored] = comparing.versions else {
            return Ok(Action::Write(match comparing.versions.is_empty() {
                true => DocumentOutcome::Added,
                false => DocumentOutcome::Changed,
            }));
        };
        let same_body = stored.hash == comparing.hash.as_str();
        let same_version = same_body && stored.time == comparing.time;
        if document.overwrite {
            return Ok(Action::Write(match same_version {
                true => DocumentOutcome::Reingested,
                false => DocumentOutcome::Changed,
            }));
        }
        if !same_body {
            return Ok(Action::Write(DocumentOutcome::Changed));
        }
        let rows = rows_at(comparing.chunk_rows, comparing.found).map_err(read_error)?;
        let hooks = self.hooks.as_ref();
        let compared = compare_chunk_rows(&rows, &stored.chunk_ranges, &document.body, hooks);
        Ok(match (compared.await.map_err(read_error)?, same_version) {
            (ChunkRows::Agree, true) => Action::Skip,
            (ChunkRows::Agree, false) => Action::ReplaceTime,
            (ChunkRows::Disagree | ChunkRows::RepeatNumbers, _) => {
                Action::Write(DocumentOutcome::Changed)
            }
        })
    }

    /// Makes the embedding vectors of the documents of the bulk to write, up
    /// to the parallelism at a time. Once a document fails, no other one is
    /// started, and those under way are finished. Returns the vectors made,
    /// by the document's place in the bulk, and the first error.
    async fn make_bulk_vectors(
        &self,
        prepared: &Prepared,
        decided: &[Decided],
        registering: &Registering<'_>,
    ) -> (Vec<Option<Vec<ChunkEmbedding>>>, Option<IndexError>) {
        let mut made: Vec<Option<Vec<ChunkEmbedding>>> = decided.iter().map(|_| None).collect();
        let mut waiting = decided
            .iter()
            .enumerate()
            .filter(|(_, done)| matches!(done.action, Action::Write(_)));
        let mut running = FuturesUnordered::new();
        let mut failed = None;
        let most = registering.parallelism.max(1);
        loop {
            while failed.is_none() && running.len() < most {
                let Some((at, done)) = waiting.next() else {
                    break;
                };
                let embedding = self.embed(&done.document, &prepared.model, registering.progress);
                running.push(async move { (at, embedding.await) });
            }
            match running.next().await {
                Some((at, Ok(vectors))) => made[at] = Some(vectors),
                Some((_, Err(error))) => {
                    failed.get_or_insert(error);
                }
                None => return (made, failed),
            }
        }
    }

    /// Writes the documents of the bulk whose vectors are made, and replaces
    /// the times to replace, then passes their outcomes. Each table's rows of
    /// the documents to write are deleted and their new rows added, the chunk
    /// table first; the times are replaced last. A document's outcome is
    /// passed once both its tables are written.
    ///
    /// The document rows are deleted and added rather than upserted: on
    /// lancedb 0.41.0, upserting document rows with large bodies or many
    /// vectors held several times their size in memory, more with each bulk.
    /// Between the deletion and the addition, readers see the documents
    /// without their rows, and a stop there leaves them so until they are
    /// registered again. Deleting also gets rid of a document's repeated rows,
    /// which only a change from outside the component leaves.
    async fn write_bulk(
        &self,
        prepared: &Prepared,
        decided: Vec<Decided>,
        made: Vec<Option<Vec<ChunkEmbedding>>>,
        results: &dyn RegistrationSink,
    ) -> Result<(), IndexError> {
        let collection = decided[0].document.collection.clone();
        let rows = BulkRows::build(&prepared.model, decided, made, self.hooks.as_ref());
        let rows = rows.await.map_err(write_error)?;
        if rows.written.is_empty() && rows.timed.is_empty() {
            return Ok(());
        }
        let _writing = self.writing.lock().await;
        self.check_columns_again(prepared).await?;
        let tables = &prepared.tables;
        if !rows.written.is_empty() {
            let keys: Vec<Key> = rows.written.iter().map(|(key, _)| key.clone()).collect();
            let keys = keys_filter(&collection, &keys);
            self.delete_rows(&tables.chunk, TableName::Chunk, &keys)
                .await?;
            if !rows.chunk_rows.is_empty() {
                self.add_rows(&tables.chunk, TableName::Chunk, rows.chunk_rows)
                    .await?;
            }
            self.delete_rows(&tables.document, TableName::Document, &keys)
                .await?;
            self.add_rows(&tables.document, TableName::Document, rows.document_rows)
                .await?;
            for (key, outcome) in &rows.written {
                results.registered(&collection, key, *outcome);
            }
        }
        if !rows.timed.is_empty() {
            self.update_rows(&tables.document, TableName::Document, rows.time_rows)
                .await?;
            for key in &rows.timed {
                results.registered(&collection, key, DocumentOutcome::Changed);
            }
        }
        Ok(())
    }

    /// Makes the embedding vectors of the document's chunks, as many at a
    /// time as `embedding` takes, one call after another.
    async fn embed(
        &self,
        document: &DocumentToRegister,
        model: &EmbeddingModel,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<Vec<ChunkEmbedding>, IndexError> {
        let texts: Vec<&str> = document
            .chunks
            .iter()
            .map(|chunk| chunk_text(&document.body, chunk))
            .collect();
        self.embed_texts(&texts, |made| {
            let mismatched = made
                .iter()
                .find(|embedding| embedding.vector.len() != model.dimension);
            if let Some(mismatched) = mismatched {
                return Err(IndexError::ModelMismatch {
                    configured_name: self.embedding.model_name().to_owned(),
                    configured_dimension: Some(mismatched.vector.len()),
                    indexed: model.clone(),
                });
            }
            if let Some(progress) = progress {
                progress.add_embedded_chunks(count_of(made.len()));
            }
            Ok(())
        })
        .await
    }

    /// Makes the embedding vectors of `texts`, as many at a time as
    /// `embedding` takes, one call after another. `made` gets the vectors of
    /// each call before the next call is made, and an error from it stops
    /// there.
    async fn embed_texts(
        &self,
        texts: &[&str],
        mut made: impl FnMut(&[ChunkEmbedding]) -> Result<(), IndexError>,
    ) -> Result<Vec<ChunkEmbedding>, IndexError> {
        let mut all = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.embedding.max_chunks_per_call().get()) {
            let embeddings = self.embedding.embed_chunks(batch).await;
            let embeddings = embeddings.map_err(IndexError::Embedding)?;
            check_embedding_count(batch.len(), embeddings.len());
            made(&embeddings)?;
            all.extend(embeddings);
        }
        Ok(all)
    }

    /// Adds `rows` to `table`, the table `name`, in one commit.
    async fn add_rows(
        &self,
        table: &lancedb::Table,
        name: TableName,
        rows: Vec<RecordBatch>,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::AddRows(name))
            .await
            .map_err(write_error)?;
        let added = table.add(rows).execute().await;
        added.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Deletes the rows of `table`, the table `name`, that `filter` picks, and
    /// returns how many it deleted.
    async fn delete_rows(
        &self,
        table: &lancedb::Table,
        name: TableName,
        filter: &DfExpr,
    ) -> Result<u64, IndexError> {
        self.hooks
            .before(LanceCall::WriteRows(name))
            .await
            .map_err(write_error)?;
        let deleted = table.delete(filter).await;
        let deleted = deleted.map_err(|error| write_error(error.into()))?;
        Ok(deleted.num_deleted_rows)
    }

    /// Checks the vector columns again as [`Self::check_columns`] does, for
    /// the model the rows were made for, just before writing them while no
    /// other function of this component writes: the write lock is not held
    /// while the vectors are made, so a rebuild may have changed the columns
    /// since the first check.
    async fn check_columns_again(&self, prepared: &Prepared) -> Result<(), IndexError> {
        let columns = Columns::read(&prepared.tables).await?;
        let made_for = Ok(prepared.model.clone());
        let checked = self.check_columns(&prepared.tables, columns, &made_for);
        checked.await.map(|_| ())
    }
}

#[async_trait]
impl Index for LanceDbIndex {
    async fn register_stream(
        &self,
        documents: BoxStream<'_, DocumentToRegister>,
        parallelism: usize,
        results: &dyn RegistrationSink,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        let result = self.register_documents(documents, parallelism, results, progress);
        let result = result.await;
        self.discard_after(&result).await;
        result
    }

    #[expect(clippy::expect_used)]
    async fn register(
        &self,
        document: DocumentToRegister,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<DocumentOutcome, IndexError> {
        let outcome = OneOutcome::default();
        let documents = futures::stream::iter([document]).boxed();
        self.register_stream(documents, 1, &outcome, progress)
            .await?;
        let outcome = outcome.0.into_inner();
        let outcome = outcome.unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(outcome.expect("a registration that returned no error passed the document's outcome"))
    }

    async fn delete_others(
        &self,
        collection: &CollectionName,
        keep: &HashSet<Key>,
        unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete> {
        let _writing = self.writing.lock().await;
        let result = self.delete_other_documents(collection, keep, unreadable_dirs);
        let result = result.await;
        if let Err(stopped) = &result {
            self.discard_after_error(&stopped.error).await;
        }
        result
    }

    async fn delete_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<(), IndexError> {
        let _writing = self.writing.lock().await;
        let result = self.delete_one_document(collection, key).await;
        self.discard_after(&result).await;
        result
    }

    async fn has_document(
        &self,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<bool, IndexError> {
        let result = self.document_exists(collection, key).await;
        self.discard_after(&result).await;
        result
    }

    async fn document_counts(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        let result = self.count_documents().await;
        self.discard_after(&result).await;
        result
    }

    async fn status(
        &self,
        scope: StatusScope,
        compare_with: &dyn Embedding,
        check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError> {
        let result = self.index_status(&scope, compare_with, check).await;
        self.discard_after(&result).await;
        result
    }

    async fn rebuild_embeddings(
        &self,
        include_configured_model: bool,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        let _writing = self.writing.lock().await;
        let result = self.rebuild(include_configured_model, parallelism, progress);
        let result = result.await;
        self.discard_after(&result).await;
        result
    }

    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError> {
        let result = self.store.pinned_tables().await;
        self.discard_after(&result).await;
        result
    }

    async fn maintain(&self) -> Result<(), IndexError> {
        let _writing = self.writing.lock().await;
        let result = self.maintain_tables().await;
        self.discard_after(&result).await;
        result
    }

    async fn check_ingestable(&self) -> Result<(), IndexError> {
        let _writing = self.writing.lock().await;
        let result = self.check_tables_ingestable().await;
        self.discard_after(&result).await;
        result
    }
}

/// The error when a table is missing and was not created. Creating it needs
/// the configured embedding model's dimension, so that happens only when the
/// model cannot be used.
#[expect(clippy::panic)]
fn tables_not_created(configured: Result<EmbeddingModel, EmbeddingError>) -> IndexError {
    match configured {
        Err(error) => IndexError::Embedding(error),
        Ok(model) => panic!(
            "tables_to_write had the model {} and still left a table missing",
            model.name
        ),
    }
}

fn warn_versions_unreadable(collection: &CollectionName, cause: &(dyn Error + 'static)) {
    tracing::warn!(
        collection = collection.0.as_str(),
        cause = causes(cause).as_str(),
        "the stored versions of documents cannot be read; opening the index again to read them again"
    );
}

/// How many times registering reads the stored versions of a bulk again,
/// opening the index again first, before the index counts as unreadable.
const VERSION_READ_RETRIES: usize = 2;

/// How a call of `register_stream` registers its documents.
struct Registering<'a> {
    parallelism: usize,
    results: &'a dyn RegistrationSink,
    progress: Option<&'a dyn ProgressSink>,
}

/// What deciding a document of a bulk compares it with.
struct Comparing<'a> {
    hash: &'a DocumentHash,
    time: Option<TimeMicros>,
    /// The document's rows in the document table.
    versions: &'a [StoredVersion],
    /// The chunk rows read for the bulk, and where the document's are in
    /// them.
    chunk_rows: &'a [RecordBatch],
    found: &'a [(usize, u32)],
}

/// Whether deciding `document` compares its chunk rows with its row: it has
/// one row, with the same body, and is not to be overwritten.
fn compares(
    document: &DocumentToRegister,
    hash: &DocumentHash,
    versions: &[StoredVersion],
) -> bool {
    match versions {
        [stored] => !document.overwrite && stored.hash == hash.as_str(),
        _ => false,
    }
}

/// The bytes a bulk holds for `document`: its body, and its embedding vectors
/// of `dimension` numbers, one per chunk, counted even when none is made.
fn held_bytes(document: &DocumentToRegister, dimension: usize) -> usize {
    let vectors = document.chunks.len().saturating_mul(dimension);
    let vectors = vectors.saturating_mul(size_of::<f32>());
    document.body.len().saturating_add(vectors)
}

/// The rows a bulk writes.
struct BulkRows {
    /// The keys of the documents written, with their outcomes.
    written: Vec<(Key, DocumentOutcome)>,
    chunk_rows: Vec<RecordBatch>,
    document_rows: Vec<RecordBatch>,
    /// The keys of the documents whose time alone is replaced.
    timed: Vec<Key>,
    time_rows: Vec<RecordBatch>,
}

impl BulkRows {
    /// Builds the rows of the documents to write whose vectors `made` has,
    /// and of the times to replace, a slice at a time. Each document and its
    /// vectors are dropped once its rows are built, so that the bulk does not
    /// hold its bodies again beside its rows while they are written.
    async fn build(
        model: &EmbeddingModel,
        decided: Vec<Decided>,
        made: Vec<Option<Vec<ChunkEmbedding>>>,
        hooks: &dyn Hooks,
    ) -> Result<Self, BoxError> {
        let mut rows = Self {
            written: Vec::new(),
            chunk_rows: Vec::new(),
            document_rows: Vec::new(),
            timed: Vec::new(),
            time_rows: Vec::new(),
        };
        let mut pace = Pace::new(hooks, Computation::Build);
        for (done, vectors) in decided.into_iter().zip(made) {
            match (done.action, vectors) {
                (Action::Write(outcome), Some(vectors)) => {
                    let new_rows = NewRows::build(model, &done.document, &vectors, &mut pace);
                    let new_rows = new_rows.await?;
                    let document_row = new_rows.document_row(&done.hash, done.time, &mut pace);
                    rows.document_rows.push(document_row.await?);
                    if new_rows.chunk_rows.num_rows() > 0 {
                        rows.chunk_rows.push(new_rows.chunk_rows);
                    }
                    rows.written.push((done.document.key, outcome));
                }
                (Action::ReplaceTime, _) => {
                    rows.time_rows
                        .push(time_row(model, &done.document, done.time)?);
                    rows.timed.push(done.document.key);
                }
                _ => {}
            }
        }
        Ok(rows)
    }
}

/// Collects the one outcome of `register`'s stream of one document.
#[derive(Default)]
struct OneOutcome(std::sync::Mutex<Option<DocumentOutcome>>);

impl RegistrationSink for OneOutcome {
    fn registered(&self, _collection: &CollectionName, _key: &Key, outcome: DocumentOutcome) {
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(outcome);
    }
}

/// The rows of `chunk_rows` at `found`, one batch for each batch they are in.
fn rows_at(
    chunk_rows: &[RecordBatch],
    found: &[(usize, u32)],
) -> Result<Vec<RecordBatch>, BoxError> {
    let mut rows = Vec::new();
    for (position, batch) in chunk_rows.iter().enumerate() {
        let at: Vec<u32> = found
            .iter()
            .filter(|(in_batch, _)| *in_batch == position)
            .map(|(_, row)| *row)
            .collect();
        if !at.is_empty() {
            rows.push(take_record_batch(batch, &UInt32Array::from(at))?);
        }
    }
    Ok(rows)
}

/// The error and its sources, joined with `: `.
fn causes(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// The text of a chunk: the body in the chunk's range.
#[expect(clippy::panic)]
fn chunk_text<'a>(body: &'a str, chunk: &Chunk) -> &'a str {
    match body.get(chunk.range.clone()) {
        Some(text) => text,
        None => panic!(
            "the chunk {:?} is outside the body or splits a character",
            chunk.range
        ),
    }
}

/// Checks that `embedding` made one vector per chunk, as it must.
#[expect(clippy::panic)]
fn check_embedding_count(chunks: usize, vectors: usize) {
    if chunks != vectors {
        panic!("embedding made {vectors} vectors for {chunks} chunks");
    }
}

fn count_of(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// The error when the embedding model makes vectors of another dimension than
/// it said it has.
fn dimension_changed(model: &EmbeddingModel, made: usize) -> IndexError {
    let source = format!(
        "the embedding model {} made vectors of {made} numbers, though its dimension is {}",
        model.name, model.dimension
    );
    IndexError::Embedding(EmbeddingError::ModelUnavailable {
        source: source.into(),
    })
}

// ---------------------------------------------------------------- the status and rebuilds

/// What `status` counts in the tables.
#[derive(Default)]
struct Counts {
    /// The model of the vector columns; `None` partway through switching
    /// them, and when no chunk counted has a vector.
    model: Option<EmbeddingModel>,
    documents: u64,
    /// By collection; a collection without chunks is left out.
    chunks: BTreeMap<CollectionName, ChunkCounts>,
}

#[derive(Clone, Copy, Default)]
struct ChunkCounts {
    chunks: u64,
    with_vector: u64,
    /// Of those with a vector, the chunks whose vector does not cover the
    /// whole chunk.
    truncated: u64,
}

impl Counts {
    fn into_status(self, compared_name: &str, dimension: DimensionResult) -> IndexStatus {
        let same_as_compared = self.model.as_ref().and_then(|model| {
            if model.name != compared_name {
                return Some(false);
            }
            match dimension {
                DimensionResult::Known(dimension) => Some(dimension == model.dimension),
                DimensionResult::Unavailable(_) | DimensionResult::NotChecked => None,
            }
        });
        let models = |counts: &ChunkCounts| match &self.model {
            Some(model) if counts.with_vector > 0 => vec![ModelStatus {
                model: model.clone(),
                same_as_compared,
                chunk_count: counts.with_vector,
                truncated_chunk_count: counts.truncated,
            }],
            _ => Vec::new(),
        };
        let mut total = ChunkCounts::default();
        for counts in self.chunks.values() {
            total.chunks += counts.chunks;
            total.with_vector += counts.with_vector;
            total.truncated += counts.truncated;
        }
        IndexStatus {
            compared_model_name: compared_name.to_owned(),
            compared_dimension: dimension,
            models: models(&total),
            collections: self
                .chunks
                .iter()
                .map(|(name, counts)| (name.clone(), models(counts)))
                .collect(),
            document_count: self.documents,
            chunk_count: total.chunks,
            chunks_without_vector: total.chunks - total.with_vector,
        }
    }
}

/// What a rebuild has left to do with a table's vector columns, when it goes
/// on from where the table is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Remaining {
    /// Nothing: the table reads vectors of the model, and has no new columns.
    Nothing,
    /// The table has its new columns with their values: index them (step 3)
    /// and switch to them (step 4).
    Switch,
    /// Add the new columns with their values (step 1 or 2), index them, and
    /// switch to them.
    Fill,
}

impl Remaining {
    /// What is left for a table whose new columns, if any, are of `model`.
    fn of(columns: &TableColumns, model: &EmbeddingModel) -> Self {
        if columns.new.is_some() {
            return Self::Switch;
        }
        match columns.read.as_ref() == Some(model) {
            true => Self::Nothing,
            false => Self::Fill,
        }
    }
}

/// A rebuild once it has decided what is left to do.
struct Rebuilding<'a> {
    tables: &'a Tables,
    columns: &'a Columns,
    model: &'a EmbeddingModel,
    /// For the chunk table, then the document table.
    remaining: [Remaining; 2],
}

impl Rebuilding<'_> {
    fn remaining_of(&self, name: TableName) -> Remaining {
        match name {
            TableName::Chunk => self.remaining[0],
            TableName::Document => self.remaining[1],
        }
    }
}

/// The rebuild table, and how many chunk vectors it keeps.
struct Kept {
    table: lancedb::Table,
    rows: usize,
}

/// Step 1 of a rebuild: what it fills the chunk table's new columns with.
struct Filling<'a> {
    table: &'a lancedb::Table,
    model: &'a EmbeddingModel,
    kept: &'a Kept,
    /// The version of the chunk table the vectors are made for.
    version: u64,
    parallelism: usize,
    tally: &'a Tally<'a>,
}

/// The chunk vectors made since the rebuild table last kept them.
#[derive(Default)]
struct ToKeep {
    batches: Vec<RecordBatch>,
    rows: usize,
}

impl ToKeep {
    fn push(&mut self, batch: RecordBatch) {
        self.rows += batch.num_rows();
        self.batches.push(batch);
    }

    fn take(&mut self) -> Vec<RecordBatch> {
        self.rows = 0;
        std::mem::take(&mut self.batches)
    }
}

/// The columns of the rebuild table for `model`: the version of the chunk
/// table, and the chunk table's new vector columns.
fn rebuild_table_schema(model: &EmbeddingModel) -> Result<SchemaRef, BoxError> {
    let new_columns = TableName::Chunk.new_columns_schema(model)?;
    let mut fields = vec![Field::new("chunk_version", DataType::UInt64, false)];
    fields.extend(
        new_columns
            .fields()
            .iter()
            .map(|field| field.as_ref().clone()),
    );
    Ok(Arc::new(Schema::new(fields)))
}

/// The chunk texts of each document in the chunk table's rows, read in
/// order, after the first `skip` rows: a document's chunks are the
/// consecutive rows with its collection name and key.
fn documents_in_chunk_rows(
    rows: SendableRecordBatchStream,
    skip: usize,
) -> impl futures::Stream<Item = Result<Vec<String>, IndexError>> {
    let grouping = ChunkRowGrouping {
        rows,
        skip,
        current: None,
        texts: Vec::new(),
        ready: VecDeque::new(),
        ended: false,
    };
    futures::stream::unfold(grouping, |mut grouping| async move {
        let next = grouping.next().await?;
        Some((next, grouping))
    })
}

struct ChunkRowGrouping {
    rows: SendableRecordBatchStream,
    skip: usize,
    /// The collection name and key of the rows in `texts`.
    current: Option<(String, String)>,
    texts: Vec<String>,
    ready: VecDeque<Vec<String>>,
    ended: bool,
}

impl ChunkRowGrouping {
    async fn next(&mut self) -> Option<Result<Vec<String>, IndexError>> {
        loop {
            if let Some(texts) = self.ready.pop_front() {
                return Some(Ok(texts));
            }
            if self.ended {
                return (!self.texts.is_empty()).then(|| Ok(std::mem::take(&mut self.texts)));
            }
            let split = match self.rows.try_next().await {
                Ok(Some(batch)) => self.split(&batch),
                Ok(None) => {
                    self.ended = true;
                    Ok(())
                }
                Err(error) => Err(read_error(error.into())),
            };
            if let Err(error) = split {
                self.ended = true;
                self.texts.clear();
                return Some(Err(error));
            }
        }
    }

    fn split(&mut self, batch: &RecordBatch) -> Result<(), IndexError> {
        let names = string_column(batch, "collection").map_err(read_error)?;
        let keys = string_column(batch, "key").map_err(read_error)?;
        let texts = string_column(batch, "text").map_err(read_error)?;
        for row in 0..batch.num_rows() {
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            let document = (names.value(row), keys.value(row));
            let current = self.current.as_ref();
            if current.map(|(name, key)| (name.as_str(), key.as_str())) != Some(document) {
                if !self.texts.is_empty() {
                    self.ready.push_back(std::mem::take(&mut self.texts));
                }
                self.current = Some((document.0.to_owned(), document.1.to_owned()));
            }
            self.texts.push(texts.value(row).to_owned());
        }
        Ok(())
    }
}

/// The values of the chunk table's new vector columns for `embeddings`,
/// built a slice at a time.
async fn chunk_vector_columns(
    model: &EmbeddingModel,
    embeddings: &[ChunkEmbedding],
    pace: &mut Pace<'_>,
) -> Result<RecordBatch, BoxError> {
    let mut values = Vec::with_capacity(embeddings.len().saturating_mul(model.dimension));
    for embedding in embeddings {
        pace.before(size_of_val(embedding.vector.as_slice())).await;
        values.extend_from_slice(&embedding.vector);
    }
    let values = Arc::new(Float32Array::from(values));
    let size = vector_size(model.dimension)?;
    let vectors = FixedSizeListArray::try_new(vector_item(), size, values, None)?;
    let covers_whole = embeddings
        .iter()
        .map(|embedding| Some(embedding.covers_whole_chunk));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(vectors),
        Arc::new(covers_whole.collect::<BooleanArray>()),
    ];
    Ok(RecordBatch::try_new(
        TableName::Chunk.new_columns_schema(model)?,
        columns,
    )?)
}

/// The runs of consecutive rows of `documents` with the same collection
/// name, each of at most `most_documents` rows whose chunks number at most
/// [`CHUNKS_PER_READ`] together. A document with more chunks than that is a
/// run of its own.
fn collection_runs(
    documents: &RecordBatch,
    most_documents: usize,
) -> Result<Vec<Range<usize>>, BoxError> {
    let names = string_column(documents, "collection")?;
    let ranges = column(documents, "chunk_ranges")?.as_list_opt::<i32>();
    let ranges = ranges.ok_or_else(|| unexpected_type("chunk_ranges"))?;
    let mut runs: Vec<(Range<usize>, usize)> = Vec::new();
    for row in 0..documents.num_rows() {
        let chunks = usize::try_from(ranges.value_length(row))?;
        match runs.last_mut() {
            Some((run, in_run))
                if run.len() < most_documents
                    && *in_run + chunks <= CHUNKS_PER_READ
                    && names.value(run.start) == names.value(row) =>
            {
                run.end = row + 1;
                *in_run += chunks;
            }
            _ => runs.push((row..row + 1, chunks)),
        }
    }
    Ok(runs.into_iter().map(|(run, _)| run).collect())
}

/// Where each key's rows are in a list of batches: the batch and the row in
/// it.
type RowsByKey<'a> = HashMap<&'a str, Vec<(usize, u32)>>;

/// Where each key's rows are in `chunk_rows`.
fn rows_by_key(chunk_rows: &[RecordBatch]) -> Result<RowsByKey<'_>, BoxError> {
    let mut found = RowsByKey::new();
    for (position, batch) in chunk_rows.iter().enumerate() {
        let keys = string_column(batch, "key")?;
        for row in 0..batch.num_rows() {
            let at = (position, u32::try_from(row)?);
            found.entry(keys.value(row)).or_default().push(at);
        }
    }
    Ok(found)
}

/// The values of the document table's new vector column, gathered one
/// document at a time.
#[derive(Default)]
struct GatheredVectors {
    values: Vec<f32>,
    lengths: Vec<usize>,
    valid: Vec<bool>,
}

impl GatheredVectors {
    /// Adds a document's value: its chunks' vectors in order, or null.
    fn push(&mut self, vectors: Option<Vec<ArrayRef>>) {
        let vectors = vectors.unwrap_or_default();
        self.valid.push(!vectors.is_empty());
        self.lengths.push(vectors.len());
        for vector in &vectors {
            let numbers = vector.as_primitive::<Float32Type>();
            self.values.extend_from_slice(numbers.values());
        }
    }

    fn into_batch(self, model: &EmbeddingModel) -> Result<RecordBatch, BoxError> {
        let values = Arc::new(Float32Array::from(self.values));
        let size = vector_size(model.dimension)?;
        let vectors = FixedSizeListArray::try_new(vector_item(), size, values, None)?;
        let offsets = OffsetBuffer::from_lengths(self.lengths);
        let nulls = NullBuffer::from(self.valid);
        let item = vectors_item(model.dimension)?;
        let list = ListArray::try_new(item, offsets, Arc::new(vectors), Some(nulls))?;
        Ok(RecordBatch::try_new(
            TableName::Document.new_columns_schema(model)?,
            vec![Arc::new(list)],
        )?)
    }
}

/// The progress of a rebuild, in chunks, shared by the documents rebuilt at
/// once.
struct Tally<'a> {
    progress: &'a dyn ProgressSink,
    done: std::sync::Mutex<u64>,
    total: u64,
}

impl<'a> Tally<'a> {
    /// Starts at `done` of `total`, and reports that.
    fn new(progress: &'a dyn ProgressSink, done: u64, total: u64) -> Self {
        progress.report(Progress {
            done,
            total: Some(total),
        });
        Self {
            progress,
            done: std::sync::Mutex::new(done),
            total,
        }
    }

    /// Adds the chunks whose vectors were just made, and reports the new
    /// count. The lock keeps the reports in the order of their counts.
    fn add(&self, chunks: u64) {
        self.progress.add_embedded_chunks(chunks);
        let mut done = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *done = done.saturating_add(chunks);
        self.progress.report(Progress {
            done: *done,
            total: Some(self.total),
        });
    }
}

// ---------------------------------------------------------------- the LanceDB connection

/// The directory in the data directory that LanceDB keeps the index in.
const LANCEDB_DIR: &str = "lancedb";

/// What the component keeps between calls of its functions.
#[derive(Default)]
struct Held {
    connection: Option<lancedb::Connection>,
    /// Kept only once both tables and all their indexes exist.
    tables: Option<Tables>,
}

/// The two tables, opened.
#[derive(Clone, Debug)]
struct Tables {
    document: lancedb::Table,
    chunk: lancedb::Table,
}

impl Tables {
    /// Each table with its name, the chunk table first, as the tables are
    /// written.
    fn each(&self) -> [(&lancedb::Table, TableName); 2] {
        [
            (&self.chunk, TableName::Chunk),
            (&self.document, TableName::Document),
        ]
    }
}

/// Opens the LanceDB in the data directory when a function first needs it,
/// and keeps it for the calls after. The lock makes concurrent first calls
/// open it once, the others waiting for that result. Nothing half-opened is
/// kept: a failed or panicking open leaves nothing, and the next call opens
/// again.
struct Store {
    data_dir: PathBuf,
    held: tokio::sync::Mutex<Held>,
    hooks: Arc<dyn Hooks>,
}

impl Store {
    fn new(data_dir: PathBuf, hooks: Arc<dyn Hooks>) -> Self {
        Self {
            data_dir,
            held: tokio::sync::Mutex::default(),
            hooks,
        }
    }

    /// The connection, for a function that only reads. `None` when there is
    /// no LanceDB directory yet: nothing is created, and the next call looks
    /// again, since another process may create the index meanwhile.
    async fn connection_to_read(&self) -> Result<Option<lancedb::Connection>, IndexError> {
        if !self.lancedb_dir_exists().await? {
            return Ok(None);
        }
        let mut held = self.held.lock().await;
        if let Some(connection) = &held.connection {
            return Ok(Some(connection.clone()));
        }
        let connection = self.connect().await.map_err(read_error)?;
        held.connection = Some(connection.clone());
        Ok(Some(connection))
    }

    /// The tables, for a function that reads them or that has nothing to
    /// write without them. `None` when there is no LanceDB directory yet or a
    /// table is missing: nothing is created, and the next call looks again.
    /// Tables whose creation stopped partway are returned but not held, so
    /// that [`Self::tables_to_write`] still finishes them.
    async fn tables_to_read(&self) -> Result<Option<Tables>, IndexError> {
        let mut held = self.held.lock().await;
        if let Some(tables) = &held.tables {
            return Ok(Some(tables.clone()));
        }
        if !self.lancedb_dir_exists().await? {
            return Ok(None);
        }
        // Taken out until the tables are opened and their indexes checked, so
        // that a failure or a panic on the way leaves nothing held. The lock is
        // kept until then, so that what another call discards meanwhile is not
        // put back.
        let taken = std::mem::take(&mut *held);
        let connection = match taken.connection {
            Some(connection) => connection,
            None => self.connect().await.map_err(read_error)?,
        };
        let tables = self.open_tables(&connection).await?;
        let finished = match &tables {
            Some(tables) => Self::has_all_indexes(tables).await?,
            None => false,
        };
        *held = Held {
            connection: Some(connection),
            tables: tables.clone().filter(|_| finished),
        };
        Ok(tables)
    }

    async fn lancedb_dir_exists(&self) -> Result<bool, IndexError> {
        let lancedb_dir = self.data_dir.join(LANCEDB_DIR);
        let exists = run_blocking(move || lancedb_dir.try_exists()).await;
        let exists = exists.map_err(|error| read_error(error.into()))?;
        exists.map_err(|error| read_error(error.into()))
    }

    /// The two tables, newly opened and each pinned to its latest version,
    /// for `query`. `None` when there is no LanceDB directory yet, a table is
    /// missing, or a pinned table lacks its full-text search index: `query`
    /// cannot search a table without it, so the index counts as created only
    /// once both tables have it.
    async fn pinned_tables(&self) -> Result<Option<QueryTables>, IndexError> {
        let Some(tables) = self.pinned().await? else {
            return Ok(None);
        };
        for (table, name) in tables.each() {
            if !Self::has_full_text_index(table, name).await? {
                return Ok(None);
            }
        }
        Ok(Some(QueryTables {
            document: tables.document,
            chunk: tables.chunk,
        }))
    }

    /// The two tables, newly opened and each pinned to its latest version;
    /// the tables held are not pinned. `None` when there is no LanceDB
    /// directory yet or a table is missing.
    async fn pinned(&self) -> Result<Option<Tables>, IndexError> {
        let Some(connection) = self.connection_to_read().await? else {
            return Ok(None);
        };
        let Some(tables) = self.open_tables(&connection).await? else {
            return Ok(None);
        };
        for (table, _) in tables.each() {
            let version = table.version().await;
            let version = version.map_err(|error| read_error(error.into()))?;
            let pinned = table.checkout(version).await;
            pinned.map_err(|error| read_error(error.into()))?;
        }
        Ok(Some(tables))
    }

    /// Drops what is held, so that the next call opens LanceDB again. Called
    /// after a call into LanceDB with what is held fails: LanceDB keeps state
    /// behind locks that a panic inside it leaves poisoned, and a poisoned
    /// table keeps failing until it is opened again.
    async fn discard(&self) {
        *self.held.lock().await = Held::default();
    }

    /// The tables, for a function that writes. Creates whatever of the tables
    /// and their indexes is missing, so that a creation that stopped partway
    /// is finished by the next call. A missing table gets vector columns for
    /// `model`; `None` when a table is missing and there is no `model`.
    async fn tables_to_write(
        &self,
        model: Option<&EmbeddingModel>,
    ) -> Result<Option<Tables>, IndexError> {
        let mut held = self.held.lock().await;
        if let Some(tables) = &held.tables {
            return Ok(Some(tables.clone()));
        }
        // Taken out until the tables are finished, so that a failure or a
        // panic on the way leaves nothing held.
        let taken = std::mem::take(&mut *held);
        let lancedb_dir = self.data_dir.join(LANCEDB_DIR);
        let created = run_blocking(move || std::fs::create_dir_all(lancedb_dir)).await;
        let created = created.map_err(|error| write_error(error.into()))?;
        created.map_err(|error| write_error(error.into()))?;
        let connection = match taken.connection {
            Some(connection) => connection,
            None => self.connect().await.map_err(write_error)?,
        };
        let tables = self.finish_tables(&connection, model).await?;
        *held = Held {
            connection: Some(connection),
            tables: tables.clone(),
        };
        Ok(tables)
    }

    async fn finish_tables(
        &self,
        connection: &lancedb::Connection,
        model: Option<&EmbeddingModel>,
    ) -> Result<Option<Tables>, IndexError> {
        let chunk = self
            .finish_table(connection, TableName::Chunk, model)
            .await?;
        let Some(chunk) = chunk else {
            return Ok(None);
        };
        let document = self
            .finish_table(connection, TableName::Document, model)
            .await?;
        Ok(document.map(|document| Tables { document, chunk }))
    }

    async fn finish_table(
        &self,
        connection: &lancedb::Connection,
        name: TableName,
        model: Option<&EmbeddingModel>,
    ) -> Result<Option<lancedb::Table>, IndexError> {
        let table = match (self.open_table(connection, name).await?, model) {
            (Some(table), _) => table,
            (None, Some(model)) => self.create_table(connection, name, model).await?,
            (None, None) => return Ok(None),
        };
        self.create_missing_indexes(&table, name).await?;
        Ok(Some(table))
    }

    /// Opens both tables, creating nothing; `None` when either is missing.
    async fn open_tables(
        &self,
        connection: &lancedb::Connection,
    ) -> Result<Option<Tables>, IndexError> {
        let chunk = self.open_table(connection, TableName::Chunk).await?;
        let document = self.open_table(connection, TableName::Document).await?;
        Ok(chunk
            .zip(document)
            .map(|(chunk, document)| Tables { document, chunk }))
    }

    /// Whether both tables have all their indexes.
    async fn has_all_indexes(tables: &Tables) -> Result<bool, IndexError> {
        let chunk = Self::missing_indexes(&tables.chunk, TableName::Chunk).await?;
        let document = Self::missing_indexes(&tables.document, TableName::Document).await?;
        Ok(chunk.is_empty() && document.is_empty())
    }

    /// Whether the table has an index on its full-text search column. Reads
    /// only the list of indexes: unlike a vector column, the full-text search
    /// column is never missing from the table.
    async fn has_full_text_index(
        table: &lancedb::Table,
        name: TableName,
    ) -> Result<bool, IndexError> {
        let existing = table.list_indices().await;
        let existing = existing.map_err(|error| read_error(error.into()))?;
        let mut full_text = name
            .indexed_columns()
            .into_iter()
            .filter_map(|(column, kind)| matches!(kind, IndexKind::FullText).then_some(column));
        Ok(full_text.all(|column| existing.iter().any(|index| index.columns == [column])))
    }

    /// The table's indexed columns that lack their index, with the kind of
    /// index and the column. A table lacks its vector column only partway
    /// through a rebuild of the vector columns, which indexes the new column
    /// itself, so a column that is not there does not count.
    async fn missing_indexes(
        table: &lancedb::Table,
        name: TableName,
    ) -> Result<Vec<(&'static str, IndexKind, Field)>, IndexError> {
        let schema = table.schema().await;
        let schema = schema.map_err(|error| read_error(error.into()))?;
        let existing = table.list_indices().await;
        let existing = existing.map_err(|error| read_error(error.into()))?;
        let mut missing = Vec::new();
        for (column, kind) in name.indexed_columns() {
            let Ok(field) = schema.field_with_name(column) else {
                continue;
            };
            if !existing.iter().any(|index| index.columns == [column]) {
                missing.push((column, kind, field.clone()));
            }
        }
        Ok(missing)
    }

    /// Opens a table; `None` when it does not exist.
    async fn open_table(
        &self,
        connection: &lancedb::Connection,
        name: TableName,
    ) -> Result<Option<lancedb::Table>, IndexError> {
        self.hooks
            .before(LanceCall::OpenTable(name))
            .await
            .map_err(read_error)?;
        match connection.open_table(name.as_str()).execute().await {
            Ok(table) => Ok(Some(table)),
            Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
            Err(error) => Err(read_error(error.into())),
        }
    }

    async fn create_table(
        &self,
        connection: &lancedb::Connection,
        name: TableName,
        model: &EmbeddingModel,
    ) -> Result<lancedb::Table, IndexError> {
        self.hooks
            .before(LanceCall::CreateTable(name))
            .await
            .map_err(write_error)?;
        let schema = name.schema(model).map_err(write_error)?;
        let created = connection.create_empty_table(name.as_str(), schema);
        created
            .execute()
            .await
            .map_err(|error| write_error(error.into()))
    }

    async fn create_missing_indexes(
        &self,
        table: &lancedb::Table,
        name: TableName,
    ) -> Result<(), IndexError> {
        for (column, kind, field) in Self::missing_indexes(table, name).await? {
            let index = kind.index(&field).map_err(read_error)?;
            self.hooks
                .before(LanceCall::CreateIndex(name))
                .await
                .map_err(write_error)?;
            let creating = table.create_index(&[column], index);
            creating
                .execute()
                .await
                .map_err(|error| write_error(error.into()))?;
        }
        Ok(())
    }

    async fn connect(&self) -> Result<lancedb::Connection, BoxError> {
        self.hooks.before(LanceCall::Connect).await?;
        // LanceDB takes a path that begins with "db" for a LanceDB server, so
        // it gets an absolute path even if the data directory is relative.
        let lancedb_dir = std::path::absolute(self.data_dir.join(LANCEDB_DIR))?;
        let uri = lancedb_dir
            .to_str()
            .ok_or("LanceDB takes only a Unicode path, and the data directory's path is not")?;
        // Reads see what other connections, including other processes, wrote.
        let builder = lancedb::connect(uri).read_consistency_interval(Duration::ZERO);
        Ok(builder.execute().await?)
    }
}

// ---------------------------------------------------------------- the tables

/// The metadata key on a vector column whose value names the embedding model
/// that the column's vectors were made with. The column's type gives the
/// dimension.
const MODEL_METADATA_KEY: &str = "mdya.embedding_model";

/// The two tables of the index: one row per document, and one row per chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableName {
    Document,
    Chunk,
}

impl TableName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Chunk => "chunk",
        }
    }

    /// The column that the table reads embedding vectors from.
    fn vector_column(self) -> &'static str {
        match self {
            Self::Document => "vectors",
            Self::Chunk => "vector",
        }
    }

    /// The column that a rebuild makes the table's embedding vectors into,
    /// before it takes the name of [`Self::vector_column`].
    fn new_vector_column(self) -> &'static str {
        match self {
            Self::Document => "vectors_new",
            Self::Chunk => "vector_new",
        }
    }

    /// The vector column, and the column that goes with each vector, as the
    /// table reads them.
    fn vector_columns(self) -> &'static [&'static str] {
        match self {
            Self::Document => &["vectors"],
            Self::Chunk => &["vector", "covers_whole"],
        }
    }

    /// The columns of [`Self::vector_columns`] as a rebuild adds them, in the
    /// same order.
    fn new_vector_columns(self) -> &'static [&'static str] {
        match self {
            Self::Document => &["vectors_new"],
            Self::Chunk => &["vector_new", "covers_whole_new"],
        }
    }

    /// The columns whose values find a row: a document, or a document's
    /// chunk.
    fn row_columns(self) -> &'static [&'static str] {
        match self {
            Self::Document => &["collection", "key"],
            Self::Chunk => &["collection", "key", "chunk_index"],
        }
    }

    /// The new vector columns for `model`, as a rebuild adds them.
    fn new_columns_schema(self, model: &EmbeddingModel) -> Result<SchemaRef, BoxError> {
        let schema = self.schema(model)?;
        let renamed = self.vector_columns().iter().zip(self.new_vector_columns());
        let fields = renamed
            .map(|(read, new)| Ok(schema.field_with_name(read)?.clone().with_name(*new)))
            .collect::<Result<Vec<_>, BoxError>>()?;
        Ok(Arc::new(Schema::new(fields)))
    }

    /// The columns of a table made for `model`. `query` reads these columns
    /// by name, so they are part of what the component publishes.
    fn schema(self, model: &EmbeddingModel) -> Result<SchemaRef, BoxError> {
        let vector_field = |data_type| {
            let metadata = HashMap::from([(MODEL_METADATA_KEY.to_owned(), model.name.clone())]);
            Field::new(self.vector_column(), data_type, true).with_metadata(metadata)
        };
        let fields = match self {
            Self::Document => vec![
                Field::new("collection", DataType::Utf8, false),
                Field::new("key", DataType::Utf8, false),
                Field::new("time", time_type(), true),
                Field::new("hash", DataType::Utf8, false),
                Field::new("body", DataType::Utf8, false),
                Field::new("chunk_ranges", DataType::List(chunk_range_item()), false),
                vector_field(DataType::List(vectors_item(model.dimension)?)),
            ],
            Self::Chunk => vec![
                Field::new("collection", DataType::Utf8, false),
                Field::new("key", DataType::Utf8, false),
                Field::new("chunk_index", DataType::UInt32, false),
                Field::new("range_start", DataType::UInt64, false),
                Field::new("range_end", DataType::UInt64, false),
                Field::new("text", DataType::Utf8, false),
                vector_field(vector_type(model.dimension)?),
                Field::new("covers_whole", DataType::Boolean, true),
            ],
        };
        Ok(Arc::new(Schema::new(fields)))
    }

    /// The columns that get an index when the table is made, and the kind of
    /// each.
    fn indexed_columns(self) -> [(&'static str, IndexKind); 4] {
        let text = match self {
            Self::Document => "body",
            Self::Chunk => "text",
        };
        [
            (text, IndexKind::FullText),
            ("collection", IndexKind::BTree),
            ("key", IndexKind::BTree),
            (self.vector_column(), IndexKind::Vector),
        ]
    }
}

/// The type of the `time` column: microseconds since the epoch, in UTC.
fn time_type() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
}

/// The type of one embedding vector of `dimension` numbers.
fn vector_type(dimension: usize) -> Result<DataType, BoxError> {
    Ok(DataType::FixedSizeList(
        vector_item(),
        vector_size(dimension)?,
    ))
}

/// The field of the numbers in an embedding vector.
fn vector_item() -> FieldRef {
    Arc::new(Field::new("item", DataType::Float32, true))
}

fn vector_size(dimension: usize) -> Result<i32, BoxError> {
    i32::try_from(dimension).map_err(|_| {
        format!("an embedding dimension of {dimension} is too large for a vector column").into()
    })
}

/// The field of the embedding vectors in the document table's `vectors`
/// column, one per chunk.
fn vectors_item(dimension: usize) -> Result<FieldRef, BoxError> {
    Ok(Arc::new(Field::new("item", vector_type(dimension)?, true)))
}

/// The field of a chunk's byte range in the body, in the `chunk_ranges`
/// column.
fn chunk_range_item() -> FieldRef {
    Arc::new(Field::new(
        "item",
        DataType::Struct(chunk_range_fields()),
        true,
    ))
}

fn chunk_range_fields() -> Fields {
    Fields::from(vec![
        Field::new("start", DataType::UInt64, false),
        Field::new("end", DataType::UInt64, false),
    ])
}

/// The dimension of the vectors in a vector column, from its type.
fn vector_dimension(field: &Field) -> Result<usize, BoxError> {
    let vector = match field.data_type() {
        DataType::List(item) => item.data_type(),
        other => other,
    };
    let DataType::FixedSizeList(_, size) = vector else {
        return Err(format!("the vector column {} holds no vectors", field.name()).into());
    };
    Ok(usize::try_from(*size)?)
}

#[derive(Clone, Copy, Debug)]
enum IndexKind {
    FullText,
    BTree,
    Vector,
}

impl IndexKind {
    /// The index for the column `field`.
    fn index(self, field: &Field) -> Result<lancedb::index::Index, BoxError> {
        Ok(match self {
            Self::FullText => lancedb::index::Index::FTS(fts_index_params()),
            Self::BTree => lancedb::index::Index::BTree(BTreeIndexBuilder::default()),
            Self::Vector => {
                let params = VectorIndexParams::for_dimension(vector_dimension(field)?);
                lancedb::index::Index::IvfPq(params.builder())
            }
        })
    }
}

/// How the vector indexes are built. Every value is set rather than left to
/// LanceDB's defaults, which are the same today, so that a newer LanceDB does
/// not change them: `query` picks its search settings for these values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VectorIndexParams {
    /// Rows per partition; LanceDB derives the number of partitions from the
    /// rows when it trains the index.
    target_partition_size: u32,
    num_sub_vectors: u32,
    num_bits: u32,
}

impl VectorIndexParams {
    fn for_dimension(dimension: usize) -> Self {
        let num_sub_vectors = match dimension {
            d if d % 16 == 0 => d / 16,
            d if d % 8 == 0 => d / 8,
            _ => 1,
        };
        Self {
            target_partition_size: 8192,
            num_sub_vectors: u32::try_from(num_sub_vectors).unwrap_or(u32::MAX),
            num_bits: 8,
        }
    }

    /// Cosine distance, because search scores are cosine similarities and
    /// LanceDB's multivector search works only with cosine.
    fn builder(self) -> IvfPqIndexBuilder {
        IvfPqIndexBuilder::default()
            .distance_type(DistanceType::Cosine)
            .target_partition_size(self.target_partition_size)
            .num_sub_vectors(self.num_sub_vectors)
            .num_bits(self.num_bits)
    }
}

/// The vector columns of both tables.
#[derive(Clone, Debug)]
struct Columns {
    chunk: TableColumns,
    document: TableColumns,
}

/// A table's vector columns: the one it reads, and a new one that a rebuild
/// left, each with the model of its vectors. `None` when the table lacks it.
#[derive(Clone, Debug)]
struct TableColumns {
    read: Option<EmbeddingModel>,
    new: Option<EmbeddingModel>,
}

impl Columns {
    async fn read(tables: &Tables) -> Result<Self, IndexError> {
        Ok(Self {
            chunk: TableColumns::read(&tables.chunk, TableName::Chunk).await?,
            document: TableColumns::read(&tables.document, TableName::Document).await?,
        })
    }

    /// Whether the vector columns are partway through switching: a rebuild
    /// stopped after it began step 4 (dropping the columns read and renaming
    /// the new ones) and before it finished it on both tables. Then a table
    /// lacks the column it reads, the two tables read vectors of different
    /// models, or the chunk table has switched (no new columns) while the
    /// document table has not.
    fn mid_switch(&self) -> bool {
        match (&self.chunk.read, &self.document.read) {
            (Some(chunk), Some(document)) => {
                chunk != document || (self.chunk.new.is_none() && self.document.new.is_some())
            }
            _ => true,
        }
    }

    /// The model of the vectors both tables read; `None` when a table lacks
    /// the column it reads or the two models differ.
    fn read_model(&self) -> Option<&EmbeddingModel> {
        match (&self.chunk.read, &self.document.read) {
            (Some(chunk), Some(document)) if chunk == document => Some(chunk),
            _ => None,
        }
    }

    /// Whether finishing the switch would leave both tables reading vectors
    /// of a model `wanted` accepts: every table has a vector column, and
    /// `wanted` accepts the model of each.
    fn can_finish_switch(&self, wanted: impl Fn(&EmbeddingModel) -> bool) -> bool {
        [&self.chunk, &self.document].into_iter().all(|table| {
            let mut models = table.read.iter().chain(table.new.iter()).peekable();
            models.peek().is_some() && models.all(&wanted)
        })
    }

    fn of(&self, name: TableName) -> &TableColumns {
        match name {
            TableName::Chunk => &self.chunk,
            TableName::Document => &self.document,
        }
    }

    fn of_mut(&mut self, name: TableName) -> &mut TableColumns {
        match name {
            TableName::Chunk => &mut self.chunk,
            TableName::Document => &mut self.document,
        }
    }
}

impl TableColumns {
    async fn read(table: &lancedb::Table, name: TableName) -> Result<Self, IndexError> {
        let schema = table
            .schema()
            .await
            .map_err(|error| read_error(error.into()))?;
        let model = |column: &str| match schema.field_with_name(column) {
            Ok(field) => model_of_field(field).map(Some).map_err(read_error),
            Err(_) => Ok(None),
        };
        Ok(Self {
            read: model(name.vector_column())?,
            new: model(name.new_vector_column())?,
        })
    }
}

/// The model named in a vector column's metadata, with the dimension of its
/// type. Anything else in that place means the index was changed from
/// outside, so it is an error reading the index.
fn model_of_field(field: &Field) -> Result<EmbeddingModel, BoxError> {
    let name = field.metadata().get(MODEL_METADATA_KEY).ok_or_else(|| {
        format!(
            "the vector column {} names no embedding model",
            field.name()
        )
    })?;
    let dimension = vector_dimension(field)?;
    Ok(EmbeddingModel {
        name: name.clone(),
        dimension,
    })
}

fn read_error(source: BoxError) -> IndexError {
    IndexError::CannotRead { source }
}

fn write_error(source: BoxError) -> IndexError {
    IndexError::CannotWrite { source }
}

// ---------------------------------------------------------------- the values of the document table

/// The value of the document table's `hash` column: the SHA-256 of the
/// document's body as UTF-8, in 64 lower-case hex digits.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DocumentHash(String);

impl DocumentHash {
    async fn of_body(body: &str, hooks: &dyn Hooks) -> Self {
        let mut hasher = Sha256::new();
        let mut pace = Pace::new(hooks, Computation::Hash);
        for slice in body.as_bytes().chunks(SLICE) {
            pace.before(slice.len()).await;
            hasher.update(slice);
        }
        drop(pace);
        let digest = hasher.finalize();
        Self::checked(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Wraps a hash that this component computed before it is written.
    #[expect(clippy::panic)]
    fn checked(value: String) -> Self {
        let well_formed = value.len() == 64
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if !well_formed {
            panic!(
                "a SHA-256 in hex is always 64 lower-case hex digits, so {value:?} is a bug here"
            );
        }
        Self(value)
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The value of the document table's `time` column: microseconds since
/// 1970-01-01 00:00:00 UTC, rounded down. A time beyond what an `i64` of
/// microseconds holds takes the nearest end. Times are compared in this form,
/// so two times that differ only below a microsecond are the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimeMicros(i64);

impl TimeMicros {
    fn from_time(time: Time) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => Self(i64::try_from(after.as_micros()).unwrap_or(i64::MAX)),
            Err(before) => Self(Self::rounded_down_before_epoch(before.duration())),
        }
    }

    /// The microseconds of a time `length` before the epoch, rounded toward
    /// the past.
    fn rounded_down_before_epoch(length: Duration) -> i64 {
        let micros = length.as_nanos().div_ceil(1_000);
        i64::try_from(micros).map_or(i64::MIN, |micros| -micros)
    }
}

// ---------------------------------------------------------------- the rows of documents

/// The condition that picks a document's rows in either table.
fn document_filter(collection: &CollectionName, key: &Key) -> DfExpr {
    collection_filter(collection).and(col("key").eq(lit(key.0.clone())))
}

/// The condition that picks a collection's rows in either table.
fn collection_filter(collection: &CollectionName) -> DfExpr {
    col("collection").eq(lit(collection.0.clone()))
}

/// The condition that picks the rows of any of the collections in either
/// table.
fn collections_filter(collections: &[CollectionName]) -> DfExpr {
    let names = collections.iter().map(|name| lit(name.0.clone()));
    col("collection").in_list(names.collect(), false)
}

/// `query` with `filter`, if any.
fn filtered(query: lancedb::query::Query, filter: Option<DfExpr>) -> lancedb::query::Query {
    match filter {
        Some(filter) => query.only_if_expr(filter),
        None => query,
    }
}

/// Whether `query` returns a row.
async fn any_row(query: lancedb::query::Query) -> Result<bool, BoxError> {
    let mut rows = query.execute().await?;
    while let Some(batch) = rows.try_next().await? {
        if batch.num_rows() > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `key` is in one of `dirs`: it begins with the directory and `/`.
fn in_any_dir(key: &Key, dirs: &[String]) -> bool {
    let under = |dir: &String| {
        key.0
            .strip_prefix(dir.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    };
    dirs.iter().any(under)
}

/// The condition that picks the rows of the documents with `keys` in the
/// collection.
fn keys_filter(collection: &CollectionName, keys: &[Key]) -> DfExpr {
    let keys = keys.iter().map(|key| lit(key.0.clone())).collect();
    collection_filter(collection).and(col("key").in_list(keys, false))
}

async fn read_rows(
    table: &lancedb::Table,
    filter: DfExpr,
    columns: &[&str],
) -> Result<Vec<RecordBatch>, BoxError> {
    let query = table
        .query()
        .only_if_expr(filter)
        .select(Select::columns(columns));
    let rows = query.execute().await?;
    Ok(rows.try_collect().await?)
}

/// `rows`, which have the same columns and at least one batch, as the source
/// of a write.
fn reader(rows: Vec<RecordBatch>) -> Box<dyn RecordBatchReader + Send> {
    let schema = rows[0].schema();
    let rows = rows.into_iter().map(Ok::<_, ArrowError>);
    Box::new(RecordBatchIterator::new(rows, schema))
}

/// What the document table holds about a version of a document.
struct StoredVersion {
    time: Option<TimeMicros>,
    hash: String,
    chunk_ranges: Vec<Range<u64>>,
}

impl StoredVersion {
    fn from_rows(rows: &RecordBatch) -> Result<Vec<Self>, BoxError> {
        let times = primitive_column::<TimestampMicrosecondType>(rows, "time")?;
        let hashes = string_column(rows, "hash")?;
        let ranges = column(rows, "chunk_ranges")?.as_list_opt::<i32>();
        let ranges = ranges.ok_or_else(|| unexpected_type("chunk_ranges"))?;
        (0..rows.num_rows())
            .map(|row| {
                Ok(Self {
                    time: times.is_valid(row).then(|| TimeMicros(times.value(row))),
                    hash: hashes.value(row).to_owned(),
                    chunk_ranges: chunk_ranges_of(&ranges.value(row))?,
                })
            })
            .collect()
    }
}

/// The byte ranges in one value of the `chunk_ranges` column.
fn chunk_ranges_of(ranges: &ArrayRef) -> Result<Vec<Range<u64>>, BoxError> {
    let ranges = ranges
        .as_struct_opt()
        .ok_or_else(|| unexpected_type("chunk_ranges"))?;
    let bound = |name| {
        let values = ranges.column_by_name(name);
        let values = values.and_then(|values| values.as_primitive_opt::<UInt64Type>());
        values.ok_or_else(|| unexpected_type("chunk_ranges"))
    };
    let (starts, ends) = (bound("start")?, bound("end")?);
    let pairs = starts.values().iter().zip(ends.values().iter());
    Ok(pairs.map(|(start, end)| *start..*end).collect())
}

/// How a document's chunk rows compare with its row. They agree when there is
/// one chunk row for each range in the row's `chunk_ranges` and no other, each
/// with the same range, and the text of each is the body in that range.
/// `body` stands in for the row's `body`: the caller has checked that their
/// hashes are the same.
async fn compare_chunk_rows(
    chunk_rows: &[RecordBatch],
    ranges: &[Range<u64>],
    body: &str,
    hooks: &dyn Hooks,
) -> Result<ChunkRows, BoxError> {
    let mut numbers: Vec<u32> = Vec::new();
    for rows in chunk_rows {
        numbers.extend(primitive_column::<UInt32Type>(rows, "chunk_index")?.values());
    }
    numbers.sort_unstable();
    if numbers.windows(2).any(|pair| pair[0] == pair[1]) {
        return Ok(ChunkRows::RepeatNumbers);
    }
    // With no number repeated, as many rows as ranges, and every number
    // within the ranges, every range has its row.
    if numbers.len() != ranges.len() {
        return Ok(ChunkRows::Disagree);
    }
    let mut pace = Pace::new(hooks, Computation::Compare);
    for rows in chunk_rows {
        let numbers = primitive_column::<UInt32Type>(rows, "chunk_index")?;
        let starts = primitive_column::<UInt64Type>(rows, "range_start")?;
        let ends = primitive_column::<UInt64Type>(rows, "range_end")?;
        let texts = string_column(rows, "text")?;
        for row in 0..rows.num_rows() {
            let range = starts.value(row)..ends.value(row);
            let stored = ranges.get(usize::try_from(numbers.value(row))?);
            if stored != Some(&range) {
                return Ok(ChunkRows::Disagree);
            }
            let Some(expected) = text_in(body, &range) else {
                return Ok(ChunkRows::Disagree);
            };
            if !texts_equal(texts.value(row), expected, &mut pace).await {
                return Ok(ChunkRows::Disagree);
            }
        }
    }
    Ok(ChunkRows::Agree)
}

/// The body in `range`; `None` when the range is outside the body or splits a
/// character.
fn text_in<'a>(body: &'a str, range: &Range<u64>) -> Option<&'a str> {
    let start = usize::try_from(range.start).ok()?;
    let end = usize::try_from(range.end).ok()?;
    body.get(start..end)
}

/// A document's rows as they are written.
struct NewRows<'a> {
    model: &'a EmbeddingModel,
    document: &'a DocumentToRegister,
    chunk_rows: RecordBatch,
    /// The embedding vectors of the chunks, in chunk order: the chunk rows'
    /// `vector` column, which the document row's `vectors` also holds.
    vectors: FixedSizeListArray,
}

impl<'a> NewRows<'a> {
    /// Builds the chunk rows, a slice at a time.
    async fn build(
        model: &'a EmbeddingModel,
        document: &'a DocumentToRegister,
        embeddings: &[ChunkEmbedding],
        pace: &mut Pace<'_>,
    ) -> Result<Self, BoxError> {
        let count = document.chunks.len();
        let mut texts = StringBuilder::new();
        let mut values = Vec::with_capacity(count.saturating_mul(model.dimension));
        for (chunk, embedding) in document.chunks.iter().zip(embeddings) {
            for piece in pieces(chunk_text(&document.body, chunk)) {
                pace.before(piece.len()).await;
                texts.write_str(piece)?;
            }
            texts.append_value("");
            pace.before(size_of_val(embedding.vector.as_slice())).await;
            values.extend_from_slice(&embedding.vector);
        }
        let values = Arc::new(Float32Array::from(values));
        let size = vector_size(model.dimension)?;
        let vectors = FixedSizeListArray::try_new(vector_item(), size, values, None)?;
        let indexes: Vec<u32> = (0..count).map(u32::try_from).collect::<Result<_, _>>()?;
        let bound = |bound: fn(&Range<usize>) -> usize| -> Result<UInt64Array, BoxError> {
            let values = document
                .chunks
                .iter()
                .map(|chunk| u64::try_from(bound(&chunk.range)));
            Ok(UInt64Array::from(values.collect::<Result<Vec<_>, _>>()?))
        };
        let covers_whole = embeddings
            .iter()
            .map(|embedding| Some(embedding.covers_whole_chunk));
        let columns: Vec<ArrayRef> = vec![
            repeated(&document.collection.0, count),
            repeated(&document.key.0, count),
            Arc::new(UInt32Array::from(indexes)),
            Arc::new(bound(|range| range.start)?),
            Arc::new(bound(|range| range.end)?),
            Arc::new(texts.finish()),
            Arc::new(vectors.clone()),
            Arc::new(covers_whole.collect::<BooleanArray>()),
        ];
        let chunk_rows = RecordBatch::try_new(TableName::Chunk.schema(model)?, columns)?;
        Ok(Self {
            model,
            document,
            chunk_rows,
            vectors,
        })
    }

    /// Builds the document row, a slice at a time.
    async fn document_row(
        &self,
        hash: &DocumentHash,
        time: Option<TimeMicros>,
        pace: &mut Pace<'_>,
    ) -> Result<RecordBatch, BoxError> {
        let mut body = StringBuilder::new();
        for piece in pieces(&self.document.body) {
            pace.before(piece.len()).await;
            body.write_str(piece)?;
        }
        body.append_value("");
        let count = self.chunk_rows.num_rows();
        let bounds = vec![
            column(&self.chunk_rows, "range_start")?.clone(),
            column(&self.chunk_rows, "range_end")?.clone(),
        ];
        let ranges = StructArray::try_new(chunk_range_fields(), bounds, None)?;
        let lengths = OffsetBuffer::from_lengths([count]);
        let chunk_ranges =
            ListArray::try_new(chunk_range_item(), lengths.clone(), Arc::new(ranges), None)?;
        let vectors_item = vectors_item(self.model.dimension)?;
        let vectors = match count {
            // A document without chunks has no vectors.
            0 => ListArray::new_null(vectors_item, 1),
            _ => ListArray::try_new(vectors_item, lengths, Arc::new(self.vectors.clone()), None)?,
        };
        let columns: Vec<ArrayRef> = vec![
            repeated(&self.document.collection.0, 1),
            repeated(&self.document.key.0, 1),
            Arc::new(time_array(time)),
            Arc::new(StringArray::from(vec![hash.as_str()])),
            Arc::new(body.finish()),
            Arc::new(chunk_ranges),
            Arc::new(vectors),
        ];
        Ok(RecordBatch::try_new(
            TableName::Document.schema(self.model)?,
            columns,
        )?)
    }
}

/// The document row with only the columns that find the row and its time.
fn time_row(
    model: &EmbeddingModel,
    document: &DocumentToRegister,
    time: Option<TimeMicros>,
) -> Result<RecordBatch, BoxError> {
    let schema = TableName::Document.schema(model)?;
    let indices = ["collection", "key", "time"].map(|name| schema.index_of(name));
    let schema = schema.project(&indices.into_iter().collect::<Result<Vec<_>, _>>()?)?;
    let columns: Vec<ArrayRef> = vec![
        repeated(&document.collection.0, 1),
        repeated(&document.key.0, 1),
        Arc::new(time_array(time)),
    ];
    Ok(RecordBatch::try_new(Arc::new(schema), columns)?)
}

fn time_array(time: Option<TimeMicros>) -> TimestampMicrosecondArray {
    TimestampMicrosecondArray::from(vec![time.map(|time| time.0)]).with_timezone("UTC")
}

/// A string column of `count` rows that all hold `value`.
fn repeated(value: &str, count: usize) -> ArrayRef {
    Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
        value, count,
    )))
}

fn column<'a>(rows: &'a RecordBatch, name: &str) -> Result<&'a ArrayRef, BoxError> {
    let found = rows.column_by_name(name);
    found.ok_or_else(|| format!("the index returned no {name} column").into())
}

fn primitive_column<'a, T: ArrowPrimitiveType>(
    rows: &'a RecordBatch,
    name: &str,
) -> Result<&'a PrimitiveArray<T>, BoxError> {
    let values = column(rows, name)?.as_primitive_opt::<T>();
    values.ok_or_else(|| unexpected_type(name))
}

fn string_column<'a>(rows: &'a RecordBatch, name: &str) -> Result<&'a StringArray, BoxError> {
    let values = column(rows, name)?.as_string_opt::<i32>();
    values.ok_or_else(|| unexpected_type(name))
}

fn boolean_column<'a>(rows: &'a RecordBatch, name: &str) -> Result<&'a BooleanArray, BoxError> {
    let values = column(rows, name)?.as_boolean_opt();
    values.ok_or_else(|| unexpected_type(name))
}

fn unexpected_type(name: &str) -> BoxError {
    format!("the {name} column of the index has an unexpected type").into()
}
