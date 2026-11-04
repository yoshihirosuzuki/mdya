//! The `index` component: keeps the documents and chunks that search reads,
//! with their embedding vectors and full-text search indexes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::DistanceType;
use lancedb::arrow::arrow_array::builder::StringBuilder;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::{TimestampMicrosecondType, UInt32Type, UInt64Type};
use lancedb::arrow::arrow_array::{
    Array, ArrayRef, ArrowPrimitiveType, BooleanArray, FixedSizeListArray, Float32Array, ListArray,
    PrimitiveArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray, StructArray,
    TimestampMicrosecondArray, UInt32Array, UInt64Array,
};
use lancedb::arrow::arrow_buffer::OffsetBuffer;
use lancedb::arrow::arrow_schema::{
    ArrowError, DataType, Field, FieldRef, Fields, Schema, SchemaRef, TimeUnit,
};
use lancedb::expr::{DfExpr, col, lit};
use lancedb::index::scalar::{BTreeIndexBuilder, FtsIndexBuilder};
use lancedb::index::vector::IvfPqIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use sha2::{Digest, Sha256};

use crate::embedding::{Embedding, EmbeddingError};
use crate::types::{
    BoxError, Chunk, ChunkEmbedding, CollectionName, DocumentOutcome, EmbeddingModel, Key,
    ProgressSink, Time,
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
    pub collections: BTreeMap<CollectionName, Vec<ModelStatus>>,
    pub document_count: u64,
    pub chunk_count: u64,
    /// Chunks whose vector is null (their vector column is being rebuilt).
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
    /// A rebuild of the embedding vectors was stopped before it switched over;
    /// rebuild first. Only `register` and `check_ingestable` return it.
    #[error("a rebuild of the embedding vectors has not finished")]
    RebuildIncomplete,
    #[error("the index cannot be read")]
    CannotRead { source: BoxError },
    #[error("the index cannot be written")]
    CannotWrite { source: BoxError },
}

#[async_trait]
pub trait Index: Send + Sync {
    /// Registers a document: adds it, replaces it, updates only its time, or
    /// skips it, and says which.
    async fn register(
        &self,
        document: DocumentToRegister,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<DocumentOutcome, IndexError>;
    /// Deletes the documents of the collection whose key is neither in `keep`
    /// nor under one of `unreadable_dirs` (key prefixes), and returns how many
    /// it deleted.
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
    /// Rebuilds the embedding vectors and returns how many chunks it rebuilt.
    async fn rebuild_embeddings(
        &self,
        include_configured_model: bool,
        parallelism: usize,
        progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError>;
    /// The tables `query` reads, pinned to the latest version. `None` when the
    /// index has no documents yet.
    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError>;
    /// Compacts the tables, cleans up old versions, and indexes new rows.
    async fn maintain(&self) -> Result<(), IndexError>;
    /// Whether documents can be registered now (the embedding model matches
    /// and no rebuild is pending).
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
    // The file operations block, so they run on tokio's blocking threads. The
    // task cannot be cancelled: if the caller stops waiting, the task still
    // runs to the end, placing the file or trying to remove its temporary file.
    let task = tokio::task::spawn_blocking(move || {
        let path = dir.join("config.yml");
        if path.try_exists().map_err(cannot_write)? {
            return Ok(());
        }
        std::fs::create_dir_all(&dir).map_err(cannot_write)?;
        write_unless_present(&dir, &path, DICTIONARY_CONFIG)
    });
    match task.await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(IndexError::CannotWrite {
            source: Box::new(error),
        }),
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
    /// Reading a document's rows.
    ReadRows(TableName),
    /// Replacing or deleting a document's rows.
    WriteRows(TableName),
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
}

struct RealHooks;

#[async_trait]
impl Hooks for RealHooks {
    async fn before(&self, _call: LanceCall) -> Result<(), BoxError> {
        Ok(())
    }

    fn computed(&self, _computation: Computation, _len: usize) {}

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
}

/// The tables to write a document to, and the embedding model of their
/// vector columns.
struct Prepared {
    tables: Tables,
    model: EmbeddingModel,
}

/// What registering a document comes down to after comparing its versions.
enum Action {
    /// Write nothing: the document has not changed.
    Skip,
    /// Replace only the time of the document's row.
    ReplaceTime,
    /// Write the document and its chunks, with new embedding vectors.
    Write {
        outcome: DocumentOutcome,
        delete_first: DeleteFirst,
    },
}

impl Action {
    fn write(outcome: DocumentOutcome) -> Self {
        Self::Write {
            outcome,
            delete_first: DeleteFirst::Nothing,
        }
    }
}

/// Rows of the document to delete before writing them, each in a commit of
/// its own. Replacing rows cannot get rid of two rows that match one new row:
/// LanceDB leaves both, and its documentation calls that case undefined. Only
/// a change from outside the component leaves such rows.
///
/// Between the deletion and the write, readers see the document without the
/// deleted rows, and a stop there leaves it so until the document is
/// registered again, which writes what is missing.
///
/// Two chunk rows with the same number are found only when the chunk rows are
/// compared with the document row, which happens only when the body has not
/// changed, the registration does not overwrite, and the document has one
/// row. When the document has more than one row, the chunk rows are deleted
/// without being compared. Other registrations can leave such rows: LanceDB
/// leaves every row of a number that the new version also has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteFirst {
    Nothing,
    /// Two of the document's chunk rows have the same chunk number.
    ChunkRows,
    /// The document has more than one row in the document table. Its chunk
    /// rows are deleted too, since they are not compared and may be repeated
    /// as well.
    AllRows,
}

/// What [`LanceDbIndex::write`] writes besides the document it is given.
struct NewVersion<'a> {
    hash: &'a DocumentHash,
    time: Option<TimeMicros>,
    delete_first: DeleteFirst,
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
        }
    }

    /// Discards the held connection and tables after an error reading or
    /// writing the index, so that the next call opens LanceDB again.
    async fn discard_after<T>(&self, result: &Result<T, IndexError>) {
        if let Err(IndexError::CannotRead { .. } | IndexError::CannotWrite { .. }) = result {
            self.store.discard().await;
        }
    }

    async fn register_document(
        &self,
        document: &DocumentToRegister,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<DocumentOutcome, IndexError> {
        self.read_dictionary().await?;
        let prepared = self.prepare_to_write().await?;
        let hash = DocumentHash::of_body(&document.body, self.hooks.as_ref()).await;
        let time = document.time.map(TimeMicros::from_time);
        let (prepared, action) = match self.read_version(&prepared.tables.document, document).await
        {
            Ok(stored) => {
                let action = self
                    .decide(&prepared.tables.chunk, document, &hash, time, stored)
                    .await?;
                (prepared, action)
            }
            Err(cause) => {
                warn_version_unreadable(document, cause.as_ref());
                self.store.discard().await;
                let reopened = self.prepare_to_write().await?;
                (reopened, Action::write(DocumentOutcome::Changed))
            }
        };
        match action {
            Action::Skip => Ok(DocumentOutcome::Unchanged),
            Action::ReplaceTime => {
                self.replace_time(&prepared, document, time).await?;
                Ok(DocumentOutcome::Changed)
            }
            Action::Write {
                outcome,
                delete_first,
            } => {
                let new_version = NewVersion {
                    hash: &hash,
                    time,
                    delete_first,
                };
                self.write(&prepared, document, new_version, progress)
                    .await?;
                Ok(outcome)
            }
        }
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

    /// The tables to write to, after checking that the configured embedding
    /// model is the one their vector columns were made with. Creates whatever
    /// of the tables and their indexes is missing.
    async fn prepare_to_write(&self) -> Result<Prepared, IndexError> {
        let configured = self.embedding.model().await;
        let tables = self.store.tables_to_write(configured.as_ref().ok()).await?;
        let Some(tables) = tables else {
            return Err(tables_not_created(configured));
        };
        let VectorColumns::Ready(model) = VectorColumns::read(&tables).await? else {
            return Err(IndexError::RebuildIncomplete);
        };
        let same = match &configured {
            Ok(configured) => *configured == model,
            // The name is checked now, and the dimension when making
            // embedding vectors, which fails the same way as checking it.
            Err(_) => self.embedding.model_name() == model.name,
        };
        if !same {
            return Err(IndexError::ModelMismatch {
                configured_name: self.embedding.model_name().to_owned(),
                configured_dimension: configured.ok().map(|model| model.dimension),
                indexed: model,
            });
        }
        Ok(Prepared { tables, model })
    }

    /// The versions of the document in the index, one per row: none when the
    /// index does not have the document. Any error means the version cannot
    /// be read.
    async fn read_version(
        &self,
        table: &lancedb::Table,
        document: &DocumentToRegister,
    ) -> Result<Vec<StoredVersion>, BoxError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Document))
            .await?;
        let columns = ["time", "hash", "chunk_ranges"];
        let batches = read_rows(table, document_filter(document), &columns).await?;
        let mut versions = Vec::new();
        for batch in &batches {
            versions.extend(StoredVersion::from_rows(batch)?);
        }
        Ok(versions)
    }

    async fn decide(
        &self,
        chunk_table: &lancedb::Table,
        document: &DocumentToRegister,
        hash: &DocumentHash,
        time: Option<TimeMicros>,
        stored: Vec<StoredVersion>,
    ) -> Result<Action, IndexError> {
        let stored = match <[StoredVersion; 1]>::try_from(stored) {
            Ok([stored]) => stored,
            Err(stored) if stored.is_empty() => return Ok(Action::write(DocumentOutcome::Added)),
            Err(_) => {
                return Ok(Action::Write {
                    outcome: DocumentOutcome::Changed,
                    delete_first: DeleteFirst::AllRows,
                });
            }
        };
        let same_body = stored.hash == hash.as_str();
        let same_version = same_body && stored.time == time;
        if document.overwrite {
            let outcome = match same_version {
                true => DocumentOutcome::Reingested,
                false => DocumentOutcome::Changed,
            };
            return Ok(Action::write(outcome));
        }
        if !same_body {
            return Ok(Action::write(DocumentOutcome::Changed));
        }
        let chunks = self.read_chunks(chunk_table, document).await?;
        let hooks = self.hooks.as_ref();
        let compared = compare_chunk_rows(&chunks, &stored.chunk_ranges, &document.body, hooks);
        match (compared.await.map_err(read_error)?, same_version) {
            (ChunkRows::Agree, true) => Ok(Action::Skip),
            (ChunkRows::Agree, false) => Ok(Action::ReplaceTime),
            (ChunkRows::Disagree, _) => Ok(Action::write(DocumentOutcome::Changed)),
            (ChunkRows::RepeatNumbers, _) => Ok(Action::Write {
                outcome: DocumentOutcome::Changed,
                delete_first: DeleteFirst::ChunkRows,
            }),
        }
    }

    /// The document's rows in the chunk table, with the columns that are
    /// compared with the document's row.
    async fn read_chunks(
        &self,
        table: &lancedb::Table,
        document: &DocumentToRegister,
    ) -> Result<Vec<RecordBatch>, IndexError> {
        self.hooks
            .before(LanceCall::ReadRows(TableName::Chunk))
            .await
            .map_err(read_error)?;
        let columns = ["chunk_index", "range_start", "range_end", "text"];
        read_rows(table, document_filter(document), &columns)
            .await
            .map_err(read_error)
    }

    /// Writes the document. Makes the embedding vectors and builds the rows
    /// before writing anything.
    async fn write(
        &self,
        prepared: &Prepared,
        document: &DocumentToRegister,
        new_version: NewVersion<'_>,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<(), IndexError> {
        let embeddings = self.embed(document, &prepared.model, progress).await?;
        let mut pace = Pace::new(self.hooks.as_ref(), Computation::Build);
        let new_rows = NewRows::build(&prepared.model, document, &embeddings, &mut pace);
        let new_rows = new_rows.await.map_err(write_error)?;
        let document_row = new_rows.document_row(new_version.hash, new_version.time, &mut pace);
        let document_row = document_row.await.map_err(write_error)?;
        drop(pace);
        let (chunk_table, document_table) = (&prepared.tables.chunk, &prepared.tables.document);
        let delete_first = new_version.delete_first;
        let delete_chunks_first =
            matches!(delete_first, DeleteFirst::ChunkRows | DeleteFirst::AllRows);
        // Writing no chunk rows deletes them anyway.
        if delete_chunks_first && new_rows.chunk_rows.num_rows() > 0 {
            self.delete_rows(chunk_table, TableName::Chunk, document)
                .await?;
        }
        self.replace_chunks(chunk_table, document, new_rows.chunk_rows)
            .await?;
        if delete_first == DeleteFirst::AllRows {
            self.delete_rows(document_table, TableName::Document, document)
                .await?;
        }
        self.replace_document(document_table, document_row).await
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
        let mut made = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.embedding.max_chunks_per_call().get()) {
            let embeddings = self.embedding.embed_chunks(batch).await;
            let embeddings = embeddings.map_err(IndexError::Embedding)?;
            check_embedding_count(batch.len(), embeddings.len());
            let mismatched = embeddings
                .iter()
                .find(|embedding| embedding.vector.len() != model.dimension);
            if let Some(mismatched) = mismatched {
                return Err(IndexError::ModelMismatch {
                    configured_name: self.embedding.model_name().to_owned(),
                    configured_dimension: Some(mismatched.vector.len()),
                    indexed: model.clone(),
                });
            }
            made.extend(embeddings);
            if let Some(progress) = progress {
                progress.add_embedded_chunks(u64::try_from(batch.len()).unwrap_or(u64::MAX));
            }
        }
        Ok(made)
    }

    /// Replaces the document's rows in the chunk table with `rows`, deleting
    /// the rows left over, in one commit.
    async fn replace_chunks(
        &self,
        table: &lancedb::Table,
        document: &DocumentToRegister,
        rows: RecordBatch,
    ) -> Result<(), IndexError> {
        if rows.num_rows() == 0 {
            return self.delete_rows(table, TableName::Chunk, document).await;
        }
        self.hooks
            .before(LanceCall::WriteRows(TableName::Chunk))
            .await
            .map_err(write_error)?;
        let mut merge = table.merge_insert(&["collection", "key", "chunk_index"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all()
            .when_not_matched_by_source_delete_expr(document_filter(document));
        let merged = merge.execute(reader(rows)).await;
        merged.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Deletes the document's rows in `table`, the table `name`.
    async fn delete_rows(
        &self,
        table: &lancedb::Table,
        name: TableName,
        document: &DocumentToRegister,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::WriteRows(name))
            .await
            .map_err(write_error)?;
        let deleted = table.delete(&document_filter(document)).await;
        deleted.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Replaces the document's row in the document table with `row`.
    async fn replace_document(
        &self,
        table: &lancedb::Table,
        row: RecordBatch,
    ) -> Result<(), IndexError> {
        self.hooks
            .before(LanceCall::WriteRows(TableName::Document))
            .await
            .map_err(write_error)?;
        let mut merge = table.merge_insert(&["collection", "key"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        let merged = merge.execute(reader(row)).await;
        merged.map_err(|error| write_error(error.into()))?;
        Ok(())
    }

    /// Replaces only the time in the document's row.
    async fn replace_time(
        &self,
        prepared: &Prepared,
        document: &DocumentToRegister,
        time: Option<TimeMicros>,
    ) -> Result<(), IndexError> {
        let row = time_row(&prepared.model, document, time).map_err(write_error)?;
        self.hooks
            .before(LanceCall::WriteRows(TableName::Document))
            .await
            .map_err(write_error)?;
        let mut merge = prepared
            .tables
            .document
            .merge_insert(&["collection", "key"]);
        merge.when_matched_update_all(None);
        let merged = merge.execute(reader(row)).await;
        merged.map_err(|error| write_error(error.into()))?;
        Ok(())
    }
}

#[async_trait]
impl Index for LanceDbIndex {
    async fn register(
        &self,
        document: DocumentToRegister,
        progress: Option<&dyn ProgressSink>,
    ) -> Result<DocumentOutcome, IndexError> {
        let result = self.register_document(&document, progress).await;
        self.discard_after(&result).await;
        result
    }

    async fn delete_others(
        &self,
        _collection: &CollectionName,
        _keep: &HashSet<Key>,
        _unreadable_dirs: &[String],
    ) -> Result<u64, PartialDelete> {
        todo!()
    }

    async fn delete_document(
        &self,
        _collection: &CollectionName,
        _key: &Key,
    ) -> Result<(), IndexError> {
        todo!()
    }

    async fn has_document(
        &self,
        _collection: &CollectionName,
        _key: &Key,
    ) -> Result<bool, IndexError> {
        todo!()
    }

    async fn document_counts(&self) -> Result<BTreeMap<CollectionName, u64>, IndexError> {
        todo!()
    }

    async fn status(
        &self,
        _scope: StatusScope,
        _compare_with: &dyn Embedding,
        _check: DimensionCheck,
    ) -> Result<IndexStatus, IndexError> {
        todo!()
    }

    async fn rebuild_embeddings(
        &self,
        _include_configured_model: bool,
        _parallelism: usize,
        _progress: &dyn ProgressSink,
    ) -> Result<u64, IndexError> {
        todo!()
    }

    async fn tables_for_query(&self) -> Result<Option<QueryTables>, IndexError> {
        todo!()
    }

    async fn maintain(&self) -> Result<(), IndexError> {
        todo!()
    }

    async fn check_ingestable(&self) -> Result<(), IndexError> {
        todo!()
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

fn warn_version_unreadable(document: &DocumentToRegister, cause: &(dyn Error + 'static)) {
    tracing::warn!(
        collection = document.collection.0.as_str(),
        key = document.key.0.as_str(),
        cause = causes(cause).as_str(),
        "the previous version of a document cannot be read, so the document is registered again"
    );
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "its callers, the other functions of Index, are still todo"
        )
    )]
    async fn connection_to_read(&self) -> Result<Option<lancedb::Connection>, IndexError> {
        let lancedb_dir = self.data_dir.join(LANCEDB_DIR);
        let exists = tokio::fs::try_exists(&lancedb_dir).await;
        if !exists.map_err(|error| read_error(error.into()))? {
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
        let created = tokio::fs::create_dir_all(&lancedb_dir).await;
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
        let schema = table.schema().await;
        let schema = schema.map_err(|error| read_error(error.into()))?;
        let existing = table.list_indices().await;
        let existing = existing.map_err(|error| read_error(error.into()))?;
        for (column, kind) in name.indexed_columns() {
            // A table lacks its vector column only partway through a rebuild
            // of the vector columns, which indexes the new column itself.
            let Ok(field) = schema.field_with_name(column) else {
                continue;
            };
            if existing.iter().any(|index| index.columns == [column]) {
                continue;
            }
            let index = kind.index(field).map_err(read_error)?;
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

/// The state of the two tables' vector columns.
#[derive(Clone, Debug, PartialEq, Eq)]
enum VectorColumns {
    /// Both tables read vectors made with this model, and no rebuild is left.
    Ready(EmbeddingModel),
    /// A rebuild of the vector columns has not finished: a table has a new
    /// vector column left, a table lacks its vector column, or the two tables'
    /// vector columns name different models.
    RebuildUnfinished,
}

impl VectorColumns {
    async fn read(tables: &Tables) -> Result<Self, IndexError> {
        let chunk = Self::model_of(&tables.chunk, TableName::Chunk).await?;
        let document = Self::model_of(&tables.document, TableName::Document).await?;
        match (chunk, document) {
            (Some(chunk), Some(document)) if chunk == document => Ok(Self::Ready(chunk)),
            _ => Ok(Self::RebuildUnfinished),
        }
    }

    /// The model of the table's vector column; `None` when the table lacks it
    /// or has a new vector column left from a rebuild.
    async fn model_of(
        table: &lancedb::Table,
        name: TableName,
    ) -> Result<Option<EmbeddingModel>, IndexError> {
        let schema = table
            .schema()
            .await
            .map_err(|error| read_error(error.into()))?;
        let new_column = format!("{}_new", name.vector_column());
        if schema.field_with_name(&new_column).is_ok() {
            return Ok(None);
        }
        let Ok(field) = schema.field_with_name(name.vector_column()) else {
            return Ok(None);
        };
        model_of_field(field).map(Some).map_err(read_error)
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

// ---------------------------------------------------------------- a document's rows

/// The condition that picks a document's rows in either table.
fn document_filter(document: &DocumentToRegister) -> DfExpr {
    let collection = col("collection").eq(lit(document.collection.0.clone()));
    collection.and(col("key").eq(lit(document.key.0.clone())))
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

/// `rows` as the source of a write.
fn reader(rows: RecordBatch) -> Box<dyn RecordBatchReader + Send> {
    let schema = rows.schema();
    Box::new(RecordBatchIterator::new(
        [Ok::<_, ArrowError>(rows)],
        schema,
    ))
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

fn unexpected_type(name: &str) -> BoxError {
    format!("the {name} column of the index has an unexpected type").into()
}
