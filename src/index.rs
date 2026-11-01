//! The `index` component: keeps the documents and chunks that search reads,
//! with their embedding vectors and full-text search indexes.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lancedb::index::scalar::FtsIndexBuilder;

use crate::embedding::{Embedding, EmbeddingError};
use crate::types::{
    BoxError, Chunk, CollectionName, DocumentOutcome, EmbeddingModel, Key, ProgressSink, Time,
};

#[cfg(test)]
pub(crate) mod test_double;

/// Creates the component over the LanceDB in the data directory. Nothing is
/// opened until a function first needs the index.
pub fn create(data_dir: PathBuf, embedding: Arc<dyn Embedding>) -> Arc<dyn Index> {
    let _ = (data_dir, embedding);
    todo!()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary_config_path(data_dir: &Path) -> PathBuf {
        data_dir
            .join(LANGUAGE_MODELS_DIR)
            .join("lindera")
            .join("ipadic")
            .join("config.yml")
    }

    #[test]
    fn fts_index_params_tokenize_with_lindera_ipadic_and_set_every_option() {
        let params = serde_json::to_value(fts_index_params()).unwrap();

        assert_eq!(params["base_tokenizer"], "lindera/ipadic");
        assert_eq!(params["language"], "English");
        assert_eq!(params["lower_case"], true);
        assert_eq!(params["ascii_folding"], true);
        assert_eq!(params["stem"], true);
        assert_eq!(params["remove_stop_words"], true);
        assert_eq!(params["max_token_length"], 255);
        assert_eq!(params["with_position"], false);
    }

    #[test]
    fn process_environment_points_lance_at_the_language_models_directory() {
        let data_dir = Path::new("/home/user/.mdya");

        let environment = process_environment(data_dir);

        assert_eq!(
            environment,
            vec![(
                OsString::from("LANCE_LANGUAGE_MODEL_HOME"),
                data_dir.join("index_language_models").into_os_string(),
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_environment_is_empty_when_the_data_directory_is_not_unicode() {
        use std::os::unix::ffi::OsStrExt;
        let data_dir = Path::new(std::ffi::OsStr::from_bytes(b"/home/user/\xff"));

        assert!(process_environment(data_dir).is_empty());
    }

    #[tokio::test]
    async fn place_fts_dictionary_config_writes_the_configuration_and_nothing_else() {
        let data_dir = tempfile::tempdir().unwrap();

        place_fts_dictionary_config(data_dir.path()).await.unwrap();

        let path = dictionary_config_path(data_dir.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DICTIONARY_CONFIG);
        let files: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(files, vec![OsString::from("config.yml")]);
    }

    #[test]
    fn dictionary_config_uses_the_embedded_ipadic_nfkc_and_lucenes_27_stop_tags() {
        assert!(DICTIONARY_CONFIG.contains("dictionary: \"embedded://ipadic\""));
        assert!(DICTIONARY_CONFIG.contains("kind: unicode_normalize"));
        assert!(DICTIONARY_CONFIG.contains("kind: nfkc"));
        let tags = DICTIONARY_CONFIG
            .lines()
            .skip_while(|line| !line.contains("tags:"))
            .skip(1)
            .count();
        assert_eq!(tags, 27);
    }

    #[tokio::test]
    async fn place_fts_dictionary_config_keeps_an_existing_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let path = dictionary_config_path(data_dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "existing").unwrap();

        place_fts_dictionary_config(data_dir.path()).await.unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing");
    }

    #[test]
    fn write_unless_present_keeps_a_file_placed_after_the_check_and_leaves_nothing_else() {
        // Stands for another process placing a different file between the
        // existence check and the move into place.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        std::fs::write(&path, "placed by someone else").unwrap();

        write_unless_present(dir.path(), &path, DICTIONARY_CONFIG).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "placed by someone else"
        );
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(files, vec![OsString::from("config.yml")]);
    }

    #[tokio::test]
    async fn place_fts_dictionary_config_fails_when_the_directory_cannot_be_made() {
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("a-file");
        std::fs::write(&data_dir, "").unwrap();

        let result = place_fts_dictionary_config(&data_dir).await;

        assert!(matches!(result, Err(IndexError::CannotWrite { .. })));
    }
}
