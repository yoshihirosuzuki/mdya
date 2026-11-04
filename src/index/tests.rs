//! The unit tests of the `index` component.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_schema::{DataType, Field, Schema, TimeUnit};
use lancedb::index::IndexType;
use lancedb::table::NewColumnTransform;
use sha2::{Digest, Sha256};
use tracing::Level;

use super::*;
use crate::embedding::test_double::{EmbeddingCall, EmbeddingFailure, TestEmbedding};
use crate::test_support::log_receiver::capture_async;
use crate::test_support::prepare_full_text_search;
use crate::types::{EmbeddingVector, Progress};

fn dictionary_config_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(LANGUAGE_MODELS_DIR)
        .join("lindera")
        .join("ipadic")
        .join("config.yml")
}

// ---------------------------------------------------------------- full-text search settings

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

// ---------------------------------------------------------------- test hooks

/// What a long computation reported, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PaceEvent {
    Computed(Computation, usize),
    Yielded,
}

/// What the hooks do at a planned call into LanceDB.
#[derive(Clone, Copy, Debug)]
enum Plan {
    /// Let the call go ahead, so that a later plan for the same call is used
    /// at a later call.
    Allow,
    Fail,
    Panic,
}

/// Hooks that record the calls into LanceDB and the pace of long
/// computations, and that make planned calls fail or panic. Unplanned calls go
/// ahead, and yields yield like the real ones.
#[derive(Default)]
struct TestHooks {
    calls: Mutex<Vec<LanceCall>>,
    plans: Mutex<Vec<(LanceCall, Plan)>>,
    pace: Mutex<Vec<PaceEvent>>,
}

impl TestHooks {
    /// Makes the next `call` do `plan`. Plans for the same call are used in
    /// the order they were made.
    fn plan(&self, call: LanceCall, plan: Plan) {
        self.plans.lock().unwrap().push((call, plan));
    }

    fn calls(&self) -> Vec<LanceCall> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self, call: LanceCall) -> usize {
        self.calls()
            .into_iter()
            .filter(|made| *made == call)
            .count()
    }

    fn pace(&self) -> Vec<PaceEvent> {
        self.pace.lock().unwrap().clone()
    }

    fn take_plan(&self, call: LanceCall) -> Option<Plan> {
        let mut plans = self.plans.lock().unwrap();
        let position = plans.iter().position(|(planned, _)| *planned == call)?;
        Some(plans.remove(position).1)
    }
}

#[async_trait]
impl Hooks for TestHooks {
    async fn before(&self, call: LanceCall) -> Result<(), BoxError> {
        self.calls.lock().unwrap().push(call);
        match self.take_plan(call) {
            Some(Plan::Fail) => Err(format!("planned failure of {call:?}").into()),
            Some(Plan::Panic) => panic!("planned panic in {call:?}"),
            Some(Plan::Allow) | None => Ok(()),
        }
    }

    fn computed(&self, computation: Computation, len: usize) {
        self.pace
            .lock()
            .unwrap()
            .push(PaceEvent::Computed(computation, len));
    }

    async fn yield_now(&self) {
        self.pace.lock().unwrap().push(PaceEvent::Yielded);
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------- the time column

#[test]
fn the_time_of_the_unix_epoch_is_zero_microseconds() {
    assert_eq!(TimeMicros::from_time(UNIX_EPOCH), TimeMicros(0));
}

#[test]
fn a_time_is_rounded_down_to_whole_microseconds() {
    let one_second_and = |nanos| UNIX_EPOCH + Duration::new(1, nanos);

    assert_eq!(
        TimeMicros::from_time(one_second_and(1_500)),
        TimeMicros(1_000_001)
    );
    assert_eq!(
        TimeMicros::from_time(one_second_and(1_999)),
        TimeMicros(1_000_001)
    );
    assert_eq!(
        TimeMicros::from_time(one_second_and(2_000)),
        TimeMicros(1_000_002)
    );
}

#[test]
fn a_time_before_the_epoch_is_rounded_toward_the_past() {
    let before = UNIX_EPOCH - Duration::from_nanos(500);

    assert_eq!(TimeMicros::from_time(before), TimeMicros(-1));
}

#[test]
fn a_time_outside_the_range_of_microseconds_takes_the_nearest_end() {
    // One second past what an i64 of microseconds can hold, on either side.
    let beyond = Duration::from_secs(i64::MAX as u64 / 1_000_000 + 1);

    // Checked only where the OS's time type reaches that far.
    if let Some(late) = UNIX_EPOCH.checked_add(beyond) {
        assert_eq!(TimeMicros::from_time(late), TimeMicros(i64::MAX));
    }
    if let Some(early) = UNIX_EPOCH.checked_sub(beyond) {
        assert_eq!(TimeMicros::from_time(early), TimeMicros(i64::MIN));
    }
}

// ---------------------------------------------------------------- the hash column

/// The SHA-256 of the empty string and of a Japanese and English body,
/// computed with `shasum -a 256`.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MIXED_BODY: &str = "mdya の文書: 日本語と English が混ざる";
const MIXED_BODY_SHA256: &str = "d74d3319645819b5c4ff34c406fd69e737b8d17fcb497f8fb3b9ff7549b3ad9a";

#[tokio::test]
async fn the_hash_is_the_sha256_of_the_body_in_lower_case_hex() {
    let hooks = TestHooks::default();

    assert_eq!(
        DocumentHash::of_body("", &hooks).await.as_str(),
        EMPTY_SHA256
    );
    assert_eq!(
        DocumentHash::of_body(MIXED_BODY, &hooks).await.as_str(),
        MIXED_BODY_SHA256
    );
}

#[tokio::test]
async fn the_hash_of_a_body_longer_than_a_slice_matches_hashing_it_at_once() {
    let body = "あいうえお".repeat(SLICE / 5);
    let at_once: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let hash = DocumentHash::of_body(&body, &TestHooks::default()).await;

    assert!(body.len() > SLICE);
    assert_eq!(hash.as_str(), at_once);
}

#[tokio::test]
async fn hashing_yields_between_slices_that_are_no_larger_than_a_slice() {
    let hooks = TestHooks::default();
    let body = "x".repeat(SLICE * 3 + 10);

    DocumentHash::of_body(&body, &hooks).await;

    let hashed = |len| PaceEvent::Computed(Computation::Hash, len);
    assert_eq!(
        hooks.pace(),
        vec![
            hashed(SLICE),
            PaceEvent::Yielded,
            hashed(SLICE),
            PaceEvent::Yielded,
            hashed(SLICE),
            PaceEvent::Yielded,
            hashed(10),
        ]
    );
}

#[test]
#[should_panic(expected = "64 lower-case hex digits")]
fn a_hash_that_is_not_64_digits_is_refused() {
    DocumentHash::checked(EMPTY_SHA256[..63].to_owned());
}

#[test]
#[should_panic(expected = "64 lower-case hex digits")]
fn a_hash_with_an_upper_case_digit_is_refused() {
    DocumentHash::checked(EMPTY_SHA256.to_uppercase());
}

#[test]
#[should_panic(expected = "64 lower-case hex digits")]
fn a_hash_with_a_character_that_is_not_hex_is_refused() {
    DocumentHash::checked(format!("{}g", &EMPTY_SHA256[..63]));
}

// ---------------------------------------------------------------- the LanceDB connection

fn store_with(hooks: &Arc<TestHooks>) -> (tempfile::TempDir, Store) {
    let data_dir = tempfile::tempdir().unwrap();
    let store = Store::new(data_dir.path().to_path_buf(), hooks.clone());
    (data_dir, store)
}

#[tokio::test]
async fn reading_without_a_lancedb_directory_connects_to_nothing_and_creates_nothing() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);

    let connection = store.connection_to_read().await.unwrap();

    assert!(connection.is_none());
    assert!(!data_dir.path().join("lancedb").exists());
    assert_eq!(hooks.count(LanceCall::Connect), 0);
}

#[tokio::test]
async fn connecting_to_write_creates_the_lancedb_directory() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);

    store.tables_to_write(None).await.unwrap();

    assert!(data_dir.path().join("lancedb").is_dir());
}

#[tokio::test]
async fn reading_connects_once_the_lancedb_directory_exists() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);
    assert!(store.connection_to_read().await.unwrap().is_none());
    std::fs::create_dir(data_dir.path().join("lancedb")).unwrap();

    let connection = store.connection_to_read().await.unwrap();
    let again = store.connection_to_read().await.unwrap();

    assert!(connection.is_some() && again.is_some());
    // The second call uses the connection the first one opened.
    assert_eq!(hooks.count(LanceCall::Connect), 1);
}

#[tokio::test]
async fn a_failed_connect_to_write_is_an_error_writing_and_the_next_call_connects_again() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::Connect, Plan::Fail);

    let first = store.tables_to_write(None).await;
    let second = store.tables_to_write(None).await;

    assert!(matches!(first, Err(IndexError::CannotWrite { .. })));
    assert!(second.is_ok());
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn a_failed_connect_to_read_is_an_error_reading() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);
    std::fs::create_dir(data_dir.path().join("lancedb")).unwrap();
    hooks.plan(LanceCall::Connect, Plan::Fail);

    let result = store.connection_to_read().await;

    assert!(matches!(result, Err(IndexError::CannotRead { .. })));
}

#[tokio::test]
async fn concurrent_first_calls_all_succeed_and_connect_once() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    let results = futures::future::join_all((0..8).map(|_| store.tables_to_write(None))).await;

    assert!(results.iter().all(Result::is_ok));
    assert_eq!(hooks.count(LanceCall::Connect), 1);
}

#[tokio::test]
async fn the_held_connection_is_used_until_it_is_discarded() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    store.tables_to_write(None).await.unwrap();
    store.tables_to_write(None).await.unwrap();
    assert_eq!(hooks.count(LanceCall::Connect), 1);
    store.discard().await;
    store.tables_to_write(None).await.unwrap();

    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn after_a_panic_while_connecting_the_next_call_connects_again() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    let store = Arc::new(store);
    hooks.plan(LanceCall::Connect, Plan::Panic);

    let panicked = tokio::spawn({
        let store = store.clone();
        async move { store.tables_to_write(None).await.map(|_| ()) }
    })
    .await;
    let after = store.tables_to_write(None).await;

    assert!(panicked.unwrap_err().is_panic());
    assert!(after.is_ok());
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn after_a_panic_while_opening_a_table_the_next_call_connects_again() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    let store = Arc::new(store);
    hooks.plan(LanceCall::OpenTable(TableName::Chunk), Plan::Panic);

    let panicked = tokio::spawn({
        let store = store.clone();
        async move { store.tables_to_write(None).await.map(|_| ()) }
    })
    .await;
    let after = store.tables_to_write(None).await;

    assert!(panicked.unwrap_err().is_panic());
    assert!(after.is_ok());
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn a_lancedb_directory_that_cannot_be_made_is_an_error_writing() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);
    std::fs::write(data_dir.path().join("lancedb"), "").unwrap();

    let result = store.tables_to_write(None).await;

    assert!(matches!(result, Err(IndexError::CannotWrite { .. })));
    assert_eq!(hooks.count(LanceCall::Connect), 0);
}

#[tokio::test]
async fn a_held_connection_is_dropped_when_the_lancedb_directory_cannot_be_made() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, store) = store_with(&hooks);
    let lancedb_dir = data_dir.path().join("lancedb");
    std::fs::create_dir(&lancedb_dir).unwrap();
    store.connection_to_read().await.unwrap().unwrap();
    std::fs::remove_dir_all(&lancedb_dir).unwrap();
    std::fs::write(&lancedb_dir, "").unwrap();

    let failed = store.tables_to_write(None).await;
    std::fs::remove_file(&lancedb_dir).unwrap();
    let after = store.tables_to_write(None).await;

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert!(after.is_ok());
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn a_table_that_cannot_be_opened_is_an_error_reading() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::OpenTable(TableName::Chunk), Plan::Fail);

    let result = store.tables_to_write(Some(&model(8))).await;

    assert!(matches!(result, Err(IndexError::CannotRead { .. })));
}

#[tokio::test]
async fn a_table_that_cannot_be_created_is_an_error_writing() {
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::CreateTable(TableName::Chunk), Plan::Fail);

    let result = store.tables_to_write(Some(&model(8))).await;

    assert!(matches!(result, Err(IndexError::CannotWrite { .. })));
}

// ---------------------------------------------------------------- the tables

const MODEL_NAME: &str = "test-model";

fn model(dimension: usize) -> EmbeddingModel {
    EmbeddingModel {
        name: MODEL_NAME.to_owned(),
        dimension,
    }
}

fn vector_of(dimension: i32) -> DataType {
    DataType::FixedSizeList(
        Arc::new(Field::new("item", DataType::Float32, true)),
        dimension,
    )
}

fn field(schema: &Schema, name: &str) -> Field {
    schema.field_with_name(name).unwrap().clone()
}

async fn indexed_columns(table: &lancedb::Table) -> Vec<(String, IndexType)> {
    let mut indexes: Vec<_> = table
        .list_indices()
        .await
        .unwrap()
        .into_iter()
        .map(|index| (index.columns.join(","), index.index_type))
        .collect();
    indexes.sort_by(|a, b| a.0.cmp(&b.0));
    indexes
}

#[test]
fn the_vector_index_splits_vectors_by_16_or_else_8_or_else_not_at_all() {
    assert_eq!(VectorIndexParams::for_dimension(768).num_sub_vectors, 48);
    assert_eq!(VectorIndexParams::for_dimension(24).num_sub_vectors, 3);
    assert_eq!(VectorIndexParams::for_dimension(7).num_sub_vectors, 1);
    let params = VectorIndexParams::for_dimension(768);
    assert_eq!((params.target_partition_size, params.num_bits), (8192, 8));
}

#[tokio::test]
async fn the_document_table_has_the_published_columns() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    let schema = tables.document.schema().await.unwrap();
    let range = DataType::Struct(
        vec![
            Field::new("start", DataType::UInt64, false),
            Field::new("end", DataType::UInt64, false),
        ]
        .into(),
    );
    let names: Vec<_> = schema.fields().iter().map(|f| f.name().clone()).collect();
    assert_eq!(
        names,
        [
            "collection",
            "key",
            "time",
            "hash",
            "body",
            "chunk_ranges",
            "vectors"
        ]
    );
    assert_eq!(field(&schema, "collection").data_type(), &DataType::Utf8);
    assert_eq!(field(&schema, "key").data_type(), &DataType::Utf8);
    let time = field(&schema, "time");
    assert_eq!(
        time.data_type(),
        &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    assert!(time.is_nullable());
    assert_eq!(field(&schema, "hash").data_type(), &DataType::Utf8);
    assert_eq!(field(&schema, "body").data_type(), &DataType::Utf8);
    let DataType::List(range_item) = field(&schema, "chunk_ranges").data_type().clone() else {
        panic!("chunk_ranges is a list");
    };
    assert_eq!(range_item.data_type(), &range);
    let vectors = field(&schema, "vectors");
    let DataType::List(vector_item) = vectors.data_type().clone() else {
        panic!("vectors is a list");
    };
    assert_eq!(vector_item.data_type(), &vector_of(8));
    assert!(vectors.is_nullable());
    assert_eq!(
        vectors
            .metadata()
            .get("mdya.embedding_model")
            .map(String::as_str),
        Some(MODEL_NAME)
    );
}

#[tokio::test]
async fn the_chunk_table_has_the_published_columns() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    let schema = tables.chunk.schema().await.unwrap();
    let names: Vec<_> = schema.fields().iter().map(|f| f.name().clone()).collect();
    assert_eq!(
        names,
        [
            "collection",
            "key",
            "chunk_index",
            "range_start",
            "range_end",
            "text",
            "vector",
            "covers_whole"
        ]
    );
    assert_eq!(field(&schema, "chunk_index").data_type(), &DataType::UInt32);
    assert_eq!(field(&schema, "range_start").data_type(), &DataType::UInt64);
    assert_eq!(field(&schema, "range_end").data_type(), &DataType::UInt64);
    assert_eq!(field(&schema, "text").data_type(), &DataType::Utf8);
    let vector = field(&schema, "vector");
    assert_eq!(vector.data_type(), &vector_of(8));
    assert!(vector.is_nullable());
    assert_eq!(
        vector
            .metadata()
            .get("mdya.embedding_model")
            .map(String::as_str),
        Some(MODEL_NAME)
    );
    let covers_whole = field(&schema, "covers_whole");
    assert_eq!(covers_whole.data_type(), &DataType::Boolean);
    assert!(covers_whole.is_nullable());
}

#[tokio::test]
async fn both_tables_get_full_text_btree_and_vector_indexes() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    let expected = |text: &str, vector: &str| {
        let mut indexes = vec![
            (text.to_owned(), IndexType::FTS),
            ("collection".to_owned(), IndexType::BTree),
            ("key".to_owned(), IndexType::BTree),
            (vector.to_owned(), IndexType::IvfPq),
        ];
        indexes.sort_by(|a, b| a.0.cmp(&b.0));
        indexes
    };
    assert_eq!(
        indexed_columns(&tables.document).await,
        expected("body", "vectors")
    );
    assert_eq!(
        indexed_columns(&tables.chunk).await,
        expected("text", "vector")
    );
}

#[tokio::test]
async fn after_creating_stops_partway_the_next_call_creates_only_what_is_missing() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::CreateIndex(TableName::Document), Plan::Fail);

    let stopped = store.tables_to_write(Some(&model(8))).await;
    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(stopped, Err(IndexError::CannotWrite { .. })));
    assert_eq!(hooks.count(LanceCall::CreateTable(TableName::Chunk)), 1);
    assert_eq!(hooks.count(LanceCall::CreateTable(TableName::Document)), 1);
    assert_eq!(indexed_columns(&tables.document).await.len(), 4);
    assert_eq!(indexed_columns(&tables.chunk).await.len(), 4);
}

#[tokio::test]
async fn after_a_failure_with_the_tables_the_connection_is_opened_again() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::CreateIndex(TableName::Chunk), Plan::Fail);

    store.tables_to_write(Some(&model(8))).await.unwrap_err();
    store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn finished_tables_are_held_and_not_opened_again() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);

    store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();
    store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(hooks.count(LanceCall::OpenTable(TableName::Document)), 1);
    assert_eq!(hooks.count(LanceCall::OpenTable(TableName::Chunk)), 1);
}

#[tokio::test]
async fn tables_another_store_finished_are_opened_not_created() {
    prepare_full_text_search().await;
    let (data_dir, first) = store_with(&Arc::new(TestHooks::default()));
    first
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();
    let hooks = Arc::new(TestHooks::default());
    let second = Store::new(data_dir.path().to_path_buf(), hooks.clone());

    second
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(hooks.count(LanceCall::CreateTable(TableName::Document)), 0);
    assert_eq!(hooks.count(LanceCall::CreateTable(TableName::Chunk)), 0);
    assert_eq!(hooks.count(LanceCall::CreateIndex(TableName::Document)), 0);
}

// ---------------------------------------------------------------- the state of the vector columns

#[tokio::test]
async fn new_tables_read_vectors_made_with_the_model_they_were_created_for() {
    prepare_full_text_search().await;
    let (_data_dir, store) = store_with(&Arc::new(TestHooks::default()));
    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    let state = VectorColumns::read(&tables).await.unwrap();

    assert_eq!(state, VectorColumns::Ready(model(8)));
}

#[tokio::test]
async fn a_new_vector_column_left_in_a_table_means_a_rebuild_has_not_finished() {
    prepare_full_text_search().await;
    let (_data_dir, store) = store_with(&Arc::new(TestHooks::default()));
    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();
    let new_column = Schema::new(vec![Field::new("vector_new", vector_of(4), true)]);
    tables
        .chunk
        .add_columns()
        .transform(NewColumnTransform::AllNulls(Arc::new(new_column)))
        .execute()
        .await
        .unwrap();

    let state = VectorColumns::read(&tables).await.unwrap();

    assert_eq!(state, VectorColumns::RebuildUnfinished);
}

#[tokio::test]
async fn a_table_without_its_vector_column_means_a_rebuild_has_not_finished() {
    prepare_full_text_search().await;
    let (_data_dir, store) = store_with(&Arc::new(TestHooks::default()));
    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();
    tables.document.drop_columns(&["vectors"]).await.unwrap();

    let state = VectorColumns::read(&tables).await.unwrap();

    assert_eq!(state, VectorColumns::RebuildUnfinished);
}

#[tokio::test]
async fn tables_whose_vector_columns_name_different_models_mean_a_rebuild_has_not_finished() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, store) = store_with(&hooks);
    hooks.plan(LanceCall::CreateTable(TableName::Document), Plan::Fail);
    store.tables_to_write(Some(&model(8))).await.unwrap_err();

    let tables = store
        .tables_to_write(Some(&model(16)))
        .await
        .unwrap()
        .unwrap();
    let state = VectorColumns::read(&tables).await.unwrap();

    assert_eq!(state, VectorColumns::RebuildUnfinished);
}

#[tokio::test]
async fn a_table_without_its_vector_column_still_gets_its_other_indexes() {
    prepare_full_text_search().await;
    let (data_dir, store) = store_with(&Arc::new(TestHooks::default()));
    let tables = store
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();
    tables.document.drop_columns(&["vectors"]).await.unwrap();
    let hooks = Arc::new(TestHooks::default());
    let other = Store::new(data_dir.path().to_path_buf(), hooks.clone());

    let reopened = other
        .tables_to_write(Some(&model(8)))
        .await
        .unwrap()
        .unwrap();

    let indexed: Vec<_> = indexed_columns(&reopened.document).await;
    let columns: Vec<_> = indexed.iter().map(|(column, _)| column.as_str()).collect();
    assert_eq!(columns, ["body", "collection", "key"]);
    assert_eq!(hooks.count(LanceCall::CreateIndex(TableName::Document)), 0);
}

// ---------------------------------------------------------------- comparing rows

/// Chunk rows with only the columns that are compared with the document row.
fn compared_chunk_rows(numbers: Vec<u32>, ranges: &[Range<u64>], texts: Vec<&str>) -> RecordBatch {
    let starts: Vec<u64> = ranges.iter().map(|range| range.start).collect();
    let ends: Vec<u64> = ranges.iter().map(|range| range.end).collect();
    RecordBatch::try_from_iter([
        (
            "chunk_index",
            Arc::new(UInt32Array::from(numbers)) as ArrayRef,
        ),
        ("range_start", Arc::new(UInt64Array::from(starts))),
        ("range_end", Arc::new(UInt64Array::from(ends))),
        ("text", Arc::new(StringArray::from(texts))),
    ])
    .unwrap()
}

#[tokio::test]
async fn chunk_rows_whose_range_is_outside_the_body_disagree() {
    let ranges = [0..5, 5..100];
    let rows = compared_chunk_rows(vec![0, 1], &ranges, vec!["alpha", " beta"]);

    let compared = compare_chunk_rows(&[rows], &ranges, "alpha beta", &TestHooks::default()).await;

    assert_eq!(compared.unwrap(), ChunkRows::Disagree);
}

#[tokio::test]
async fn chunk_rows_that_agree_in_two_batches_agree() {
    let ranges = [0..5, 5..10];
    let first = compared_chunk_rows(vec![1], &ranges[1..], vec![" beta"]);
    let second = compared_chunk_rows(vec![0], &ranges[..1], vec!["alpha"]);

    let compared = compare_chunk_rows(
        &[first, second],
        &ranges,
        "alpha beta",
        &TestHooks::default(),
    )
    .await;

    assert_eq!(compared.unwrap(), ChunkRows::Agree);
}

#[tokio::test]
async fn chunk_rows_with_a_column_of_another_type_cannot_be_compared() {
    let rows = RecordBatch::try_from_iter([(
        "chunk_index",
        Arc::new(StringArray::from(vec!["0"])) as ArrayRef,
    )])
    .unwrap();

    let ranges = vec![Range { start: 0, end: 5 }];

    let compared = compare_chunk_rows(&[rows], &ranges, "alpha", &TestHooks::default()).await;

    assert!(compared.is_err());
}

#[test]
fn a_document_row_with_a_column_of_another_type_cannot_be_read() {
    let rows =
        RecordBatch::try_from_iter([("time", Arc::new(StringArray::from(vec!["0"])) as ArrayRef)])
            .unwrap();

    assert!(StoredVersion::from_rows(&rows).is_err());
}

#[tokio::test]
async fn texts_are_compared_by_length_and_by_every_slice() {
    let hooks = TestHooks::default();
    let mut pace = Pace::new(&hooks, Computation::Compare);
    let long = "a".repeat(SLICE + 1);
    let differs_at_the_end = format!("{}b", "a".repeat(SLICE));

    assert!(!texts_equal("abc", "ab", &mut pace).await);
    assert!(!texts_equal(&long, &differs_at_the_end, &mut pace).await);
    assert!(texts_equal(&long, &long.clone(), &mut pace).await);
}

// ---------------------------------------------------------------- registering a document: helpers

const COLLECTION: &str = "notes";
/// The length the tests' documents are split by into chunks, in bytes.
const CHUNK_LEN: usize = 8;
const ASCII_BODY: &str = "alpha beta gamma delta epsilon";

/// A time with a part below a microsecond, and its value in the time column.
fn base_time() -> Time {
    UNIX_EPOCH + Duration::new(1_700_000_000, 1_500)
}
const BASE_MICROS: i64 = 1_700_000_000_000_001;

fn embedding_of(name: &str, dimension: usize) -> Arc<TestEmbedding> {
    Arc::new(TestEmbedding::new(name, dimension))
}

/// A component over a new data directory.
fn index_with(
    embedding: Arc<dyn Embedding>,
    hooks: &Arc<TestHooks>,
) -> (tempfile::TempDir, LanceDbIndex) {
    let data_dir = tempfile::tempdir().unwrap();
    let index = index_in(data_dir.path(), embedding, hooks);
    (data_dir, index)
}

/// A component over the data directory `data_dir`.
fn index_in(
    data_dir: &Path,
    embedding: Arc<dyn Embedding>,
    hooks: &Arc<TestHooks>,
) -> LanceDbIndex {
    LanceDbIndex::new(data_dir.to_path_buf(), embedding, hooks.clone())
}

/// A document whose body is split into chunks of [`CHUNK_LEN`] bytes.
fn document(key: &str, body: &str) -> DocumentToRegister {
    DocumentToRegister {
        collection: CollectionName(COLLECTION.to_owned()),
        key: Key(key.to_owned()),
        time: Some(base_time()),
        body: body.to_owned(),
        chunks: chunks_of(body, CHUNK_LEN),
        overwrite: false,
    }
}

/// Splits `body` into chunks of `len` bytes, each widened to end between
/// characters, the last one shorter.
fn chunks_of(body: &str, len: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let mut end = (start + len).min(body.len());
        while !body.is_char_boundary(end) {
            end += 1;
        }
        chunks.push(Chunk { range: start..end });
        start = end;
    }
    chunks
}

fn chunk_texts(document: &DocumentToRegister) -> Vec<&str> {
    document
        .chunks
        .iter()
        .map(|chunk| &document.body[chunk.range.clone()])
        .collect()
}

async fn register(
    index: &LanceDbIndex,
    document: &DocumentToRegister,
) -> Result<DocumentOutcome, IndexError> {
    index.register(document.clone(), None).await
}

/// How many calls to make chunk vectors `embedding` got.
fn embed_calls(embedding: &TestEmbedding) -> usize {
    let calls = embedding.calls();
    calls
        .iter()
        .filter(|call| matches!(call, EmbeddingCall::EmbedChunks(_)))
        .count()
}

fn sha256_hex(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The tables in `data_dir`, opened apart from the component under test. The
/// indexes they lack are created first: to see which indexes a table has, use
/// [`tables_as_they_are`].
async fn open_tables(data_dir: &Path) -> Tables {
    let store = Store::new(data_dir.to_path_buf(), Arc::new(TestHooks::default()));
    store.tables_to_write(None).await.unwrap().unwrap()
}

/// The tables in `data_dir` as they are, opened with LanceDB alone, so that
/// nothing they lack is created.
async fn tables_as_they_are(data_dir: &Path) -> Tables {
    let uri = data_dir.join(LANCEDB_DIR);
    let connection = lancedb::connect(uri.to_str().unwrap());
    let connection = connection.execute().await.unwrap();
    let document = connection.open_table(TableName::Document.as_str());
    let chunk = connection.open_table(TableName::Chunk.as_str());
    Tables {
        document: document.execute().await.unwrap(),
        chunk: chunk.execute().await.unwrap(),
    }
}

/// Drops the index on `column` of `table`, so that the table lacks it as
/// tables whose creation stopped partway do.
async fn drop_index_on(table: &lancedb::Table, column: &str) {
    let existing = table.list_indices().await.unwrap();
    let on_column = existing.iter().find(|config| config.columns == [column]);
    table.drop_index(&on_column.unwrap().name).await.unwrap();
}

async fn all_rows(table: &lancedb::Table) -> Vec<RecordBatch> {
    let rows = table.query().execute().await.unwrap();
    rows.try_collect().await.unwrap()
}

#[derive(Clone, Debug, PartialEq)]
struct DocumentRow {
    collection: String,
    key: String,
    time: Option<i64>,
    hash: String,
    body: String,
    chunk_ranges: Vec<Range<u64>>,
    vectors: Option<Vec<EmbeddingVector>>,
}

#[derive(Clone, Debug, PartialEq)]
struct ChunkRow {
    collection: String,
    key: String,
    chunk_index: u32,
    range: Range<u64>,
    text: String,
    vector: Option<EmbeddingVector>,
    covers_whole: Option<bool>,
}

fn floats(array: &ArrayRef) -> EmbeddingVector {
    array.as_primitive::<Float32Type>().values().to_vec()
}

/// The rows of the document table in `data_dir`, by key.
async fn document_rows(data_dir: &Path) -> Vec<DocumentRow> {
    let mut rows = Vec::new();
    for batch in all_rows(&open_tables(data_dir).await.document).await {
        let strings = |name: &str| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_string::<i32>()
                .clone()
        };
        let column = |name: &str| batch.column_by_name(name).unwrap().clone();
        let times = column("time");
        let times = times.as_primitive::<TimestampMicrosecondType>();
        let ranges = column("chunk_ranges");
        let ranges = ranges.as_list::<i32>();
        let vectors = column("vectors");
        let vectors = vectors.as_list::<i32>();
        for row in 0..batch.num_rows() {
            let vectors = vectors.is_valid(row).then(|| {
                let list = vectors.value(row);
                let list = list.as_fixed_size_list();
                (0..list.len())
                    .map(|item| floats(&list.value(item)))
                    .collect()
            });
            rows.push(DocumentRow {
                collection: strings("collection").value(row).to_owned(),
                key: strings("key").value(row).to_owned(),
                time: times.is_valid(row).then(|| times.value(row)),
                hash: strings("hash").value(row).to_owned(),
                body: strings("body").value(row).to_owned(),
                chunk_ranges: chunk_ranges_of(&ranges.value(row)).unwrap(),
                vectors,
            });
        }
    }
    rows.sort_by(|a, b| (&a.collection, &a.key).cmp(&(&b.collection, &b.key)));
    rows
}

/// The rows of the chunk table in `data_dir`, by key and chunk number.
async fn chunk_rows(data_dir: &Path) -> Vec<ChunkRow> {
    let mut rows = Vec::new();
    for batch in all_rows(&open_tables(data_dir).await.chunk).await {
        let strings = |name: &str| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_string::<i32>()
                .clone()
        };
        let bounds = |name: &str| {
            batch
                .column_by_name(name)
                .unwrap()
                .as_primitive::<UInt64Type>()
                .clone()
        };
        let column = |name: &str| batch.column_by_name(name).unwrap().clone();
        let indexes = column("chunk_index");
        let indexes = indexes.as_primitive::<UInt32Type>();
        let vectors = column("vector");
        let vectors = vectors.as_fixed_size_list();
        let covers = column("covers_whole");
        let covers = covers.as_boolean();
        for row in 0..batch.num_rows() {
            rows.push(ChunkRow {
                collection: strings("collection").value(row).to_owned(),
                key: strings("key").value(row).to_owned(),
                chunk_index: indexes.value(row),
                range: bounds("range_start").value(row)..bounds("range_end").value(row),
                text: strings("text").value(row).to_owned(),
                vector: vectors.is_valid(row).then(|| floats(&vectors.value(row))),
                covers_whole: covers.is_valid(row).then(|| covers.value(row)),
            });
        }
    }
    rows.sort_by(|a, b| {
        (&a.collection, &a.key, a.chunk_index).cmp(&(&b.collection, &b.key, b.chunk_index))
    });
    rows
}

fn ranges_u64(document: &DocumentToRegister) -> Vec<Range<u64>> {
    let range = |chunk: &Chunk| chunk.range.start as u64..chunk.range.end as u64;
    document.chunks.iter().map(range).collect()
}

fn expected_document_row(document: &DocumentToRegister, embedding: &TestEmbedding) -> DocumentRow {
    let texts = chunk_texts(document);
    DocumentRow {
        collection: document.collection.0.clone(),
        key: document.key.0.clone(),
        time: document.time.map(|time| TimeMicros::from_time(time).0),
        hash: sha256_hex(&document.body),
        body: document.body.clone(),
        chunk_ranges: ranges_u64(document),
        vectors: (!texts.is_empty()).then(|| {
            texts
                .iter()
                .map(|text| embedding.vector_for(text))
                .collect()
        }),
    }
}

fn expected_chunk_rows(document: &DocumentToRegister, embedding: &TestEmbedding) -> Vec<ChunkRow> {
    let texts = chunk_texts(document);
    let ranges = ranges_u64(document);
    texts
        .iter()
        .zip(ranges)
        .enumerate()
        .map(|(index, (text, range))| ChunkRow {
            collection: document.collection.0.clone(),
            key: document.key.0.clone(),
            chunk_index: index as u32,
            range,
            text: (*text).to_owned(),
            vector: Some(embedding.vector_for(text)),
            covers_whole: Some(true),
        })
        .collect()
}

/// Checks that the index in `data_dir` holds exactly `documents`, with the
/// vectors `embedding` makes, and chunk rows that agree with each
/// document's row.
async fn assert_index_holds(
    data_dir: &Path,
    documents: &[&DocumentToRegister],
    embedding: &TestEmbedding,
) {
    let mut expected: Vec<_> = documents
        .iter()
        .map(|document| expected_document_row(document, embedding))
        .collect();
    expected.sort_by(|a, b| (&a.collection, &a.key).cmp(&(&b.collection, &b.key)));
    assert_eq!(document_rows(data_dir).await, expected);
    let mut expected: Vec<_> = documents
        .iter()
        .flat_map(|document| expected_chunk_rows(document, embedding))
        .collect();
    expected.sort_by(|a, b| {
        (&a.collection, &a.key, a.chunk_index).cmp(&(&b.collection, &b.key, b.chunk_index))
    });
    assert_eq!(chunk_rows(data_dir).await, expected);
}

/// What the component did with `embedding` and with the progress sink, in
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    EmbedStarted(Vec<String>),
    EmbedReturned(usize),
    ChunksAdded(u64),
    Reported(Progress),
}

/// Wraps a [`TestEmbedding`] to log when each call to make chunk vectors
/// starts and returns, in one log with the progress the component reports
/// to a [`LoggedProgress`]. A call yields between the two, so that a call
/// made before the one before it returned would show up between them.
struct LoggedEmbedding {
    inner: Arc<TestEmbedding>,
    log: Arc<Mutex<Vec<Step>>>,
}

#[async_trait]
impl Embedding for LoggedEmbedding {
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn max_chunks_per_call(&self) -> std::num::NonZeroUsize {
        self.inner.max_chunks_per_call()
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        self.inner.model().await
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        let started = texts.iter().map(|text| (*text).to_owned()).collect();
        self.log.lock().unwrap().push(Step::EmbedStarted(started));
        tokio::task::yield_now().await;
        let made = self.inner.embed_chunks(texts).await;
        self.log
            .lock()
            .unwrap()
            .push(Step::EmbedReturned(texts.len()));
        made
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        self.inner.embed_query(query).await
    }
}

struct LoggedProgress {
    log: Arc<Mutex<Vec<Step>>>,
}

impl ProgressSink for LoggedProgress {
    fn report(&self, progress: Progress) {
        self.log.lock().unwrap().push(Step::Reported(progress));
    }

    fn add_embedded_chunks(&self, count: u64) {
        self.log.lock().unwrap().push(Step::ChunksAdded(count));
    }
}

/// Wraps a [`TestEmbedding`] whose dimension cannot be checked: asking for
/// the model fails, but making vectors works.
struct UncheckableModel(Arc<TestEmbedding>);

#[async_trait]
impl Embedding for UncheckableModel {
    fn model_name(&self) -> &str {
        self.0.model_name()
    }

    fn max_chunks_per_call(&self) -> std::num::NonZeroUsize {
        self.0.max_chunks_per_call()
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        Err(EmbeddingError::ModelUnavailable {
            source: "the model cannot be checked now".into(),
        })
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        self.0.embed_chunks(texts).await
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        self.0.embed_query(query).await
    }
}

/// A way the chunk rows of the document `a.md` (with [`ASCII_BODY`]) can
/// disagree with its row.
#[derive(Clone, Copy, Debug)]
enum Mismatch {
    /// A row whose number is not in the document row's `chunk_ranges`, added
    /// to the others.
    ExtraNumber,
    /// A row whose number is not in the document row's `chunk_ranges`, in
    /// place of the last row, so that there are as many rows as ranges.
    RenumberedRow,
    /// Two rows with the same number.
    RepeatedNumber,
    /// No row for one of the numbers.
    MissingNumber,
    /// A row whose range differs from the document row's.
    OtherRange,
    /// A row with the same range whose text differs from the body's.
    OtherText,
}

const MISMATCHES: [Mismatch; 6] = [
    Mismatch::ExtraNumber,
    Mismatch::RenumberedRow,
    Mismatch::RepeatedNumber,
    Mismatch::MissingNumber,
    Mismatch::OtherRange,
    Mismatch::OtherText,
];

/// Rewrites the chunk rows in `data_dir` to disagree as `mismatch` says.
/// The conditions are constants, so they are written as SQL.
async fn make_mismatch(data_dir: &Path, mismatch: Mismatch) {
    let table = open_tables(data_dir).await.chunk;
    match mismatch {
        Mismatch::ExtraNumber => append_first_chunk_row(&table, Some(99)).await,
        Mismatch::RenumberedRow => {
            let update = table.update().only_if("chunk_index = 3");
            update.column("chunk_index", "99").execute().await.unwrap();
        }
        Mismatch::RepeatedNumber => append_first_chunk_row(&table, None).await,
        Mismatch::MissingNumber => {
            table.delete("chunk_index = 1").await.unwrap();
        }
        Mismatch::OtherRange => {
            let update = table.update().only_if("chunk_index = 0");
            update
                .column("range_end", "range_end - 1")
                .execute()
                .await
                .unwrap();
        }
        Mismatch::OtherText => {
            let update = table.update().only_if("chunk_index = 0");
            update
                .column("text", "upper(text)")
                .execute()
                .await
                .unwrap();
        }
    }
}

/// Appends a copy of the first chunk row, with the chunk number `number` if
/// given.
async fn append_first_chunk_row(table: &lancedb::Table, number: Option<u32>) {
    let query = table.query().only_if("chunk_index = 0");
    let rows: Vec<RecordBatch> = query.execute().await.unwrap().try_collect().await.unwrap();
    let row = rows.into_iter().next().unwrap();
    let mut columns = row.columns().to_vec();
    if let Some(number) = number {
        let position = row.schema().index_of("chunk_index").unwrap();
        columns[position] = Arc::new(UInt32Array::from(vec![number]));
    }
    let row = RecordBatch::try_new(row.schema(), columns).unwrap();
    table.add(row).execute().await.unwrap();
}

// ---------------------------------------------------------------- registering a document

#[tokio::test]
async fn a_new_document_is_added_with_its_chunks_and_their_vectors() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let added = document("a.md", MIXED_BODY);

    let outcome = register(&index, &added).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Added);
    assert!(data_dir.path().join("lancedb").is_dir());
    assert_index_holds(data_dir.path(), &[&added], &embedding).await;
    let row = &document_rows(data_dir.path()).await[0];
    assert_eq!(row.time, Some(BASE_MICROS));
    assert_eq!(row.hash, MIXED_BODY_SHA256);
}

#[tokio::test]
async fn a_document_with_an_empty_body_is_added_without_chunks_or_vectors() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let empty = document("empty.md", "");

    let outcome = register(&index, &empty).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Added);
    assert_index_holds(data_dir.path(), &[&empty], &embedding).await;
    let row = &document_rows(data_dir.path()).await[0];
    assert_eq!((row.chunk_ranges.len(), row.vectors.as_ref()), (0, None));
    assert_eq!(embed_calls(&embedding), 0);
}

#[tokio::test]
async fn a_document_without_a_time_has_no_time_in_the_index() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let raw = DocumentToRegister {
        time: None,
        ..document("raw-1", ASCII_BODY)
    };

    let first = register(&index, &raw).await.unwrap();
    let again = register(&index, &raw).await.unwrap();

    assert_eq!(
        (first, again),
        (DocumentOutcome::Added, DocumentOutcome::Unchanged)
    );
    assert_eq!(document_rows(data_dir.path()).await[0].time, None);
}

#[tokio::test]
async fn chunks_go_to_embedding_in_order_a_limited_number_at_a_time_with_progress_after_each() {
    prepare_full_text_search().await;
    let log = Arc::new(Mutex::new(Vec::new()));
    let inner = Arc::new(TestEmbedding::new(MODEL_NAME, 8).with_max_chunks_per_call(3));
    let embedding = Arc::new(LoggedEmbedding {
        inner,
        log: log.clone(),
    });
    let (_data_dir, index) = index_with(embedding, &Arc::new(TestHooks::default()));
    let added = document("a.md", ASCII_BODY);
    let texts: Vec<String> = chunk_texts(&added).into_iter().map(str::to_owned).collect();
    let progress = LoggedProgress { log: log.clone() };

    index
        .register(added.clone(), Some(&progress))
        .await
        .unwrap();

    assert_eq!(texts.len(), 4);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            Step::EmbedStarted(texts[0..3].to_vec()),
            Step::EmbedReturned(3),
            Step::ChunksAdded(3),
            Step::EmbedStarted(texts[3..4].to_vec()),
            Step::EmbedReturned(1),
            Step::ChunksAdded(1),
        ]
    );
}

#[tokio::test]
async fn registering_the_same_version_again_skips_it_without_making_vectors() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    let versions = open_tables(data_dir.path())
        .await
        .document
        .version()
        .await
        .unwrap();

    let outcome = register(&index, &same).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Unchanged);
    assert_eq!(embed_calls(&embedding), 1);
    let tables = open_tables(data_dir.path()).await;
    assert_eq!(tables.document.version().await.unwrap(), versions);
    assert_index_holds(data_dir.path(), &[&same], &embedding).await;
}

#[tokio::test]
async fn registering_the_same_version_to_overwrite_registers_it_again() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();

    let overwrite = DocumentToRegister {
        overwrite: true,
        ..same.clone()
    };
    let outcome = register(&index, &overwrite).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Reingested);
    assert_eq!(embed_calls(&embedding), 2);
    assert_index_holds(data_dir.path(), &[&same], &embedding).await;
}

#[tokio::test]
async fn a_new_version_to_overwrite_is_changed() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let later = DocumentToRegister {
        time: Some(base_time() + Duration::from_micros(1)),
        overwrite: true,
        ..document("a.md", ASCII_BODY)
    };
    let other_body = DocumentToRegister {
        overwrite: true,
        ..document("a.md", MIXED_BODY)
    };

    let outcomes = (
        register(&index, &later).await.unwrap(),
        register(&index, &other_body).await.unwrap(),
    );

    assert_eq!(
        outcomes,
        (DocumentOutcome::Changed, DocumentOutcome::Changed)
    );
    assert_eq!(embed_calls(&embedding), 3);
    assert_index_holds(data_dir.path(), &[&other_body], &embedding).await;
}

#[tokio::test]
async fn two_rows_of_a_document_and_of_a_chunk_are_replaced_by_one() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    let tables = open_tables(data_dir.path()).await;
    let rows = all_rows(&tables.document).await;
    tables
        .document
        .add(rows[0].clone())
        .execute()
        .await
        .unwrap();
    append_first_chunk_row(&tables.chunk, None).await;
    assert_eq!(document_rows(data_dir.path()).await.len(), 2);
    assert_eq!(
        chunk_rows(data_dir.path()).await.len(),
        same.chunks.len() + 1
    );

    let (outcome, logs) = capture_async(register(&index, &same)).await;

    assert_eq!(outcome.unwrap(), DocumentOutcome::Changed);
    assert!(logs.iter().all(|log| log.level != Level::WARN));
    assert_index_holds(data_dir.path(), &[&same], &embedding).await;
}

#[tokio::test]
async fn two_rows_of_a_document_whose_body_becomes_empty_are_replaced_by_one() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let tables = open_tables(data_dir.path()).await;
    let rows = all_rows(&tables.document).await;
    tables
        .document
        .add(rows[0].clone())
        .execute()
        .await
        .unwrap();
    append_first_chunk_row(&tables.chunk, None).await;
    let chunk_writes = hooks.count(LanceCall::WriteRows(TableName::Chunk));

    let emptied = document("a.md", "");
    let outcome = register(&index, &emptied).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_index_holds(data_dir.path(), &[&emptied], &embedding).await;
    // Writing no chunk rows deletes them, so they are not deleted first too.
    let chunk_writes = hooks.count(LanceCall::WriteRows(TableName::Chunk)) - chunk_writes;
    assert_eq!(chunk_writes, 1);
}

#[tokio::test]
async fn a_changed_body_replaces_the_document_and_leaves_no_old_chunk_rows() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let other = document("b.md", MIXED_BODY);
    register(&index, &other).await.unwrap();

    let shorter = document("a.md", "short body");
    let outcome = register(&index, &shorter).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_index_holds(data_dir.path(), &[&shorter, &other], &embedding).await;
}

#[tokio::test]
async fn a_body_that_becomes_empty_leaves_no_chunk_rows() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    let emptied = document("a.md", "");
    let outcome = register(&index, &emptied).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_index_holds(data_dir.path(), &[&emptied], &embedding).await;
}

#[tokio::test]
async fn a_time_that_differs_only_below_a_microsecond_is_the_same_time() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (_data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    let later_by_499_nanos = DocumentToRegister {
        time: Some(base_time() + Duration::from_nanos(499)),
        ..document("a.md", ASCII_BODY)
    };
    let outcome = register(&index, &later_by_499_nanos).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Unchanged);
}

#[tokio::test]
async fn two_times_beyond_the_same_end_of_the_range_are_the_same_time() {
    prepare_full_text_search().await;
    let beyond = Duration::from_secs(i64::MAX as u64 / 1_000_000 + 1);
    // Checked only where the OS's time type reaches that far.
    let Some(late) = UNIX_EPOCH.checked_add(beyond) else {
        return;
    };
    let Some(later) = late.checked_add(Duration::from_secs(1)) else {
        return;
    };
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let at = |time| DocumentToRegister {
        time: Some(time),
        ..document("a.md", ASCII_BODY)
    };
    register(&index, &at(late)).await.unwrap();

    let outcome = register(&index, &at(later)).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Unchanged);
    assert_eq!(document_rows(data_dir.path()).await[0].time, Some(i64::MAX));
}

#[tokio::test]
async fn a_time_that_differs_in_microseconds_replaces_only_the_time() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    let later = DocumentToRegister {
        time: Some(base_time() + Duration::from_micros(1)),
        ..document("a.md", ASCII_BODY)
    };
    let outcome = register(&index, &later).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_eq!(embed_calls(&embedding), 1);
    assert_index_holds(data_dir.path(), &[&later], &embedding).await;
    assert_eq!(
        document_rows(data_dir.path()).await[0].time,
        Some(BASE_MICROS + 1)
    );
}

#[tokio::test]
async fn an_earlier_time_with_the_same_body_replaces_only_the_time() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    let earlier = DocumentToRegister {
        time: Some(base_time() - Duration::from_secs(60)),
        ..document("a.md", ASCII_BODY)
    };
    let outcome = register(&index, &earlier).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_eq!(embed_calls(&embedding), 1);
    assert_index_holds(data_dir.path(), &[&earlier], &embedding).await;
}

#[tokio::test]
async fn an_earlier_time_with_another_body_registers_the_document_again() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    let earlier = DocumentToRegister {
        time: Some(base_time() - Duration::from_secs(60)),
        ..document("a.md", MIXED_BODY)
    };
    let outcome = register(&index, &earlier).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_eq!(embed_calls(&embedding), 2);
    assert_index_holds(data_dir.path(), &[&earlier], &embedding).await;
}

#[tokio::test]
async fn a_stored_hash_that_is_not_64_digits_makes_the_document_registered_again() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    let table = open_tables(data_dir.path()).await.document;
    table
        .update()
        .column("hash", "'abc'")
        .execute()
        .await
        .unwrap();

    let outcome = register(&index, &same).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_eq!(embed_calls(&embedding), 2);
    assert_index_holds(data_dir.path(), &[&same], &embedding).await;
}

#[tokio::test]
async fn each_mismatch_of_the_chunk_rows_makes_the_same_version_registered_again() {
    prepare_full_text_search().await;
    for mismatch in MISMATCHES {
        let embedding = embedding_of(MODEL_NAME, 8);
        let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
        let same = document("a.md", ASCII_BODY);
        register(&index, &same).await.unwrap();
        make_mismatch(data_dir.path(), mismatch).await;

        let outcome = register(&index, &same).await.unwrap();
        let after = register(&index, &same).await.unwrap();

        assert_eq!(outcome, DocumentOutcome::Changed, "{mismatch:?}");
        assert_eq!(after, DocumentOutcome::Unchanged, "{mismatch:?}");
        assert_eq!(embed_calls(&embedding), 2, "{mismatch:?}");
        assert_index_holds(data_dir.path(), &[&same], &embedding).await;
    }
}

#[tokio::test]
async fn each_mismatch_of_the_chunk_rows_makes_a_new_time_register_the_document_again() {
    prepare_full_text_search().await;
    for mismatch in MISMATCHES {
        let embedding = embedding_of(MODEL_NAME, 8);
        let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
        register(&index, &document("a.md", ASCII_BODY))
            .await
            .unwrap();
        make_mismatch(data_dir.path(), mismatch).await;

        let later = DocumentToRegister {
            time: Some(base_time() + Duration::from_micros(1)),
            ..document("a.md", ASCII_BODY)
        };
        let outcome = register(&index, &later).await.unwrap();

        assert_eq!(outcome, DocumentOutcome::Changed, "{mismatch:?}");
        assert_eq!(embed_calls(&embedding), 2, "{mismatch:?}");
        assert_index_holds(data_dir.path(), &[&later], &embedding).await;
    }
}

#[tokio::test]
async fn a_configured_model_of_another_dimension_or_name_is_a_model_mismatch() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let first = document("a.md", ASCII_BODY);
    register(&index, &first).await.unwrap();

    for (name, dimension) in [(MODEL_NAME, 16), ("other-model", 8)] {
        let configured = embedding_of(name, dimension);
        let hooks = Arc::new(TestHooks::default());
        let other = index_in(data_dir.path(), configured.clone(), &hooks);

        let result = register(&other, &document("b.md", MIXED_BODY)).await;

        let Err(IndexError::ModelMismatch {
            configured_name,
            configured_dimension,
            indexed,
        }) = result
        else {
            panic!("expected a model mismatch, got {result:?}");
        };
        assert_eq!(
            (configured_name.as_str(), configured_dimension, indexed),
            (name, Some(dimension), model(8))
        );
        assert_eq!(embed_calls(&configured), 0);
    }
    assert_index_holds(data_dir.path(), &[&first], &embedding).await;
}

#[tokio::test]
async fn a_model_that_cannot_be_checked_is_a_model_mismatch_when_its_name_differs() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let unavailable = Arc::new(
        TestEmbedding::new("other-model", 8).failing_with(EmbeddingFailure::ModelUnavailable),
    );
    let other = index_in(
        data_dir.path(),
        unavailable,
        &Arc::new(TestHooks::default()),
    );

    let result = register(&other, &document("b.md", "")).await;

    assert!(matches!(
        result,
        Err(IndexError::ModelMismatch {
            configured_dimension: None,
            ..
        })
    ));
}

#[tokio::test]
async fn a_model_that_cannot_be_checked_but_has_the_same_name_still_registers_documents() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let first = document("a.md", ASCII_BODY);
    register(&index, &first).await.unwrap();
    let uncheckable = Arc::new(UncheckableModel(embedding.clone()));
    let other = index_in(
        data_dir.path(),
        uncheckable,
        &Arc::new(TestHooks::default()),
    );

    let empty = document("empty.md", "");
    let with_chunks = document("b.md", MIXED_BODY);
    let outcomes = (
        register(&other, &empty).await.unwrap(),
        register(&other, &with_chunks).await.unwrap(),
    );

    assert_eq!(outcomes, (DocumentOutcome::Added, DocumentOutcome::Added));
    assert_index_holds(data_dir.path(), &[&first, &empty, &with_chunks], &embedding).await;
}

#[tokio::test]
async fn a_model_that_cannot_be_checked_and_makes_vectors_of_another_dimension_is_a_model_mismatch()
{
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let first = document("a.md", ASCII_BODY);
    register(&index, &first).await.unwrap();
    let uncheckable = Arc::new(UncheckableModel(embedding_of(MODEL_NAME, 16)));
    let other = index_in(
        data_dir.path(),
        uncheckable,
        &Arc::new(TestHooks::default()),
    );

    let result = register(&other, &document("b.md", MIXED_BODY)).await;

    let Err(IndexError::ModelMismatch {
        configured_dimension,
        indexed,
        ..
    }) = result
    else {
        panic!("expected a model mismatch, got {result:?}");
    };
    assert_eq!((configured_dimension, indexed), (Some(16), model(8)));
    assert_eq!(document_rows(data_dir.path()).await.len(), 1);
}

#[tokio::test]
async fn an_unavailable_model_cannot_make_the_vectors_of_a_document_with_chunks() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let unavailable = Arc::new(
        TestEmbedding::new(MODEL_NAME, 8).failing_with(EmbeddingFailure::ModelUnavailable),
    );
    let other = index_in(
        data_dir.path(),
        unavailable,
        &Arc::new(TestHooks::default()),
    );

    let result = register(&other, &document("b.md", MIXED_BODY)).await;

    assert!(matches!(result, Err(IndexError::Embedding(_))));
}

#[tokio::test]
async fn without_tables_an_unavailable_model_cannot_be_used() {
    prepare_full_text_search().await;
    let unavailable = Arc::new(
        TestEmbedding::new(MODEL_NAME, 8).failing_with(EmbeddingFailure::ModelUnavailable),
    );
    let (_data_dir, index) = index_with(unavailable, &Arc::new(TestHooks::default()));

    let result = register(&index, &document("a.md", "")).await;

    assert!(matches!(result, Err(IndexError::Embedding(_))));
}

#[tokio::test]
async fn an_unfinished_rebuild_stops_registration() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let new_column = Schema::new(vec![Field::new("vector_new", vector_of(4), true)]);
    let tables = open_tables(data_dir.path()).await;
    let adding = tables.chunk.add_columns();
    let adding = adding.transform(NewColumnTransform::AllNulls(Arc::new(new_column)));
    adding.execute().await.unwrap();

    let result = register(&index, &document("b.md", ASCII_BODY)).await;

    assert!(matches!(result, Err(IndexError::RebuildIncomplete)));
}

#[tokio::test]
async fn after_a_failed_write_the_next_registration_opens_lancedb_again() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    let added = document("a.md", ASCII_BODY);
    hooks.plan(LanceCall::WriteRows(TableName::Chunk), Plan::Fail);

    let failed = register(&index, &added).await;
    let outcome = register(&index, &added).await.unwrap();

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(hooks.count(LanceCall::Connect), 2);
    assert_index_holds(data_dir.path(), &[&added], &embedding).await;
}

#[tokio::test]
async fn a_failure_reading_the_chunk_rows_is_an_error_reading() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    hooks.plan(LanceCall::ReadRows(TableName::Chunk), Plan::Fail);

    let result = register(&index, &same).await;

    assert!(matches!(result, Err(IndexError::CannotRead { .. })));
}

#[tokio::test]
async fn when_the_previous_version_cannot_be_read_the_document_is_registered_again_with_a_warning()
{
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    hooks.plan(LanceCall::ReadRows(TableName::Document), Plan::Fail);

    let (outcome, logs) = capture_async(register(&index, &same)).await;

    assert_eq!(outcome.unwrap(), DocumentOutcome::Changed);
    // LanceDB logs too, at lower levels.
    let warnings: Vec<_> = logs.iter().filter(|log| log.level == Level::WARN).collect();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .fields
            .contains(&("key".to_owned(), "a.md".to_owned()))
    );
    assert_eq!(hooks.count(LanceCall::Connect), 2);
    assert_eq!(embed_calls(&embedding), 2);
    assert_index_holds(data_dir.path(), &[&same], &embedding).await;
}

#[tokio::test]
async fn when_lancedb_cannot_be_opened_again_after_an_unreadable_version_the_registration_fails() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    let same = document("a.md", ASCII_BODY);
    register(&index, &same).await.unwrap();
    hooks.plan(LanceCall::ReadRows(TableName::Document), Plan::Fail);
    hooks.plan(LanceCall::Connect, Plan::Fail);

    let failed = register(&index, &same).await;
    let outcome = register(&index, &same).await.unwrap();

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Unchanged);
}

#[tokio::test]
async fn a_failure_writing_the_document_row_is_mended_by_the_next_registration() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let changed = document("a.md", MIXED_BODY);
    hooks.plan(LanceCall::WriteRows(TableName::Document), Plan::Fail);

    let failed = register(&index, &changed).await;
    let outcome = register(&index, &changed).await.unwrap();

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_index_holds(data_dir.path(), &[&changed], &embedding).await;
}

#[tokio::test]
async fn a_failure_replacing_the_time_is_an_error_writing() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let later = DocumentToRegister {
        time: Some(base_time() + Duration::from_micros(1)),
        ..document("a.md", ASCII_BODY)
    };
    hooks.plan(LanceCall::WriteRows(TableName::Document), Plan::Fail);

    let failed = register(&index, &later).await;
    let outcome = register(&index, &later).await.unwrap();

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Changed);
    assert_index_holds(data_dir.path(), &[&later], &embedding).await;
}

/// Wraps a [`TestEmbedding`] that makes one vector fewer than it is asked
/// for.
struct ShortEmbedding(Arc<TestEmbedding>);

#[async_trait]
impl Embedding for ShortEmbedding {
    fn model_name(&self) -> &str {
        self.0.model_name()
    }

    fn max_chunks_per_call(&self) -> std::num::NonZeroUsize {
        self.0.max_chunks_per_call()
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        self.0.model().await
    }

    async fn embed_chunks(&self, texts: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        let mut made = self.0.embed_chunks(texts).await?;
        made.pop();
        Ok(made)
    }

    async fn embed_query(&self, query: &str) -> Result<EmbeddingVector, EmbeddingError> {
        self.0.embed_query(query).await
    }
}

#[tokio::test]
#[should_panic(expected = "embedding made 3 vectors for 4 chunks")]
async fn embedding_that_makes_too_few_vectors_is_a_bug() {
    prepare_full_text_search().await;
    let short = Arc::new(ShortEmbedding(embedding_of(MODEL_NAME, 8)));
    let (_data_dir, index) = index_with(short, &Arc::new(TestHooks::default()));

    let _ = register(&index, &document("a.md", ASCII_BODY)).await;
}

#[tokio::test]
#[should_panic(expected = "is outside the body or splits a character")]
async fn a_chunk_outside_the_body_is_a_bug_of_the_caller() {
    prepare_full_text_search().await;
    let (_data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let outside = DocumentToRegister {
        chunks: vec![Chunk { range: 0..100 }],
        ..document("a.md", ASCII_BODY)
    };

    let _ = register(&index, &outside).await;
}

#[tokio::test]
async fn registering_after_creating_the_tables_stopped_partway_finishes_them() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    hooks.plan(LanceCall::CreateIndex(TableName::Document), Plan::Fail);

    let stopped = register(&index, &document("a.md", ASCII_BODY)).await;
    let outcome = register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    assert!(matches!(stopped, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Added);
    let tables = tables_as_they_are(data_dir.path()).await;
    assert_eq!(indexed_columns(&tables.document).await.len(), 4);
    assert_eq!(indexed_columns(&tables.chunk).await.len(), 4);
}

#[tokio::test]
async fn held_tables_read_what_another_component_wrote() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, first) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let hooks = Arc::new(TestHooks::default());
    let second = index_in(data_dir.path(), embedding.clone(), &hooks);
    register(&second, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let written_elsewhere = document("b.md", MIXED_BODY);
    register(&first, &written_elsewhere).await.unwrap();

    let outcome = register(&second, &written_elsewhere).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Unchanged);
    assert_eq!(hooks.count(LanceCall::OpenTable(TableName::Document)), 1);
}

#[tokio::test]
async fn registering_eight_documents_at_once_writes_them_all() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let documents: Vec<_> = (0..8)
        .map(|number| {
            document(
                &format!("doc-{number}.md"),
                &format!("{ASCII_BODY} {number}"),
            )
        })
        .collect();

    let outcomes =
        futures::future::join_all(documents.iter().map(|document| register(&index, document)))
            .await;

    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Ok(DocumentOutcome::Added)))
    );
    let documents: Vec<_> = documents.iter().collect();
    assert_index_holds(data_dir.path(), &documents, &embedding).await;
}

#[tokio::test]
async fn quotes_and_wildcards_in_keys_and_collection_names_pick_only_their_own_rows() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let in_collection = |collection: &str, key: &str, body: &str| DocumentToRegister {
        collection: CollectionName(collection.to_owned()),
        ..document(key, body)
    };
    let keys = [
        "it's.md",
        "100%.md",
        "a_b.md",
        r"back\slash.md",
        "日本語.md",
    ];
    let first: Vec<_> = keys
        .iter()
        .map(|key| in_collection("it's", key, ASCII_BODY))
        .collect();
    // Keys that `%` and `_` would match as wildcards, and the same key in
    // another collection.
    let kept = [
        in_collection("it's", "plain.md", ASCII_BODY),
        in_collection("it's", "100x.md", ASCII_BODY),
        in_collection("it's", "aXb.md", ASCII_BODY),
        in_collection("other", "it's.md", ASCII_BODY),
    ];
    for document in first.iter().chain(&kept) {
        register(&index, document).await.unwrap();
    }

    let mut again = Vec::new();
    for document in &first {
        again.push(register(&index, document).await.unwrap());
    }
    // A shorter body replaces chunk rows and deletes the rest; an empty body
    // deletes them all.
    let replaced: Vec<_> = keys
        .iter()
        .enumerate()
        .map(|(number, key)| match number % 2 {
            0 => in_collection("it's", key, "short body"),
            _ => in_collection("it's", key, ""),
        })
        .collect();
    for document in &replaced {
        register(&index, document).await.unwrap();
    }

    assert!(
        again
            .iter()
            .all(|outcome| *outcome == DocumentOutcome::Unchanged)
    );
    let expected: Vec<_> = replaced.iter().chain(&kept).collect();
    assert_index_holds(data_dir.path(), &expected, &embedding).await;
}

// ---------------------------------------------------------------- the dictionary configuration

#[tokio::test]
async fn the_dictionary_configuration_is_placed_and_read_once() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);

    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    register(&index, &document("b.md", ASCII_BODY))
        .await
        .unwrap();

    assert_eq!(hooks.count(LanceCall::PlaceDictionary), 1);
    assert_eq!(hooks.count(LanceCall::ReadDictionary), 1);
    let placed = std::fs::read_to_string(dictionary_config_path(data_dir.path())).unwrap();
    assert_eq!(placed, DICTIONARY_CONFIG);
}

#[tokio::test]
async fn concurrent_first_registrations_read_the_dictionary_configuration_once() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    let documents: Vec<_> = (0..8)
        .map(|number| document(&format!("doc-{number}.md"), ASCII_BODY))
        .collect();

    let outcomes =
        futures::future::join_all(documents.iter().map(|document| register(&index, document)))
            .await;

    assert!(outcomes.iter().all(Result::is_ok));
    assert_eq!(hooks.count(LanceCall::ReadDictionary), 1);
}

#[tokio::test]
async fn a_dictionary_configuration_that_cannot_be_placed_is_placed_by_the_next_registration() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    hooks.plan(LanceCall::PlaceDictionary, Plan::Fail);

    let failed = register(&index, &document("a.md", ASCII_BODY)).await;
    let outcome = register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    assert!(matches!(failed, Err(IndexError::CannotWrite { .. })));
    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(hooks.count(LanceCall::PlaceDictionary), 2);
}

#[tokio::test]
async fn a_dictionary_configuration_that_cannot_be_read_is_read_by_the_next_registration() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    hooks.plan(LanceCall::ReadDictionary, Plan::Fail);

    let failed = register(&index, &document("a.md", ASCII_BODY)).await;
    let outcome = register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    assert!(matches!(failed, Err(IndexError::CannotRead { .. })));
    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(hooks.count(LanceCall::ReadDictionary), 2);
}

#[tokio::test]
async fn after_a_panic_while_reading_the_dictionary_configuration_the_next_registration_reads_it() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    let index = Arc::new(index);
    hooks.plan(LanceCall::ReadDictionary, Plan::Panic);

    let panicked = tokio::spawn({
        let index = index.clone();
        async move {
            register(&index, &document("a.md", ASCII_BODY))
                .await
                .map(|_| ())
        }
    })
    .await;
    let outcome = register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    assert!(panicked.unwrap_err().is_panic());
    assert_eq!(outcome, DocumentOutcome::Added);
    assert_eq!(hooks.count(LanceCall::ReadDictionary), 2);
}

// ---------------------------------------------------------------- the pace of long computations

#[tokio::test]
async fn registering_a_long_body_computes_in_slices_and_yields_between_them() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let embedding = embedding_of(MODEL_NAME, 8);
    let (_data_dir, index) = index_with(embedding, &hooks);
    let body = "あいうえお".repeat(SLICE / 6);
    let long = DocumentToRegister {
        chunks: chunks_of(&body, 64 * 1024),
        ..document("long.md", &body)
    };
    let later = DocumentToRegister {
        time: Some(base_time() + Duration::from_micros(1)),
        ..long.clone()
    };

    register(&index, &long).await.unwrap();
    let outcome = register(&index, &later).await.unwrap();

    assert_eq!(outcome, DocumentOutcome::Changed);
    assert!(body.len() > SLICE * 2);
    let pace = hooks.pace();
    for computation in [Computation::Hash, Computation::Compare, Computation::Build] {
        let slices: Vec<usize> = pace
            .iter()
            .filter_map(|event| match event {
                PaceEvent::Computed(done, len) if *done == computation => Some(*len),
                _ => None,
            })
            .collect();
        assert!(slices.len() > 2, "{computation:?}");
        assert!(slices.iter().all(|len| *len <= SLICE), "{computation:?}");
    }
    // Two slices of the same computation always have a yield between them.
    let mut last: Option<Computation> = None;
    for event in &pace {
        match event {
            PaceEvent::Computed(computation, _) => {
                assert_ne!(
                    last,
                    Some(*computation),
                    "no yield after a slice of {computation:?}"
                );
                last = Some(*computation);
            }
            PaceEvent::Yielded => last = None,
        }
    }
}

// ---------------------------------------------------------------- reading and deleting documents: helpers

fn name(value: &str) -> CollectionName {
    CollectionName(value.to_owned())
}

fn key(value: &str) -> Key {
    Key(value.to_owned())
}

fn keys(values: &[&str]) -> HashSet<Key> {
    values.iter().map(|value| key(value)).collect()
}

fn document_in(collection: &str, key: &str) -> DocumentToRegister {
    DocumentToRegister {
        collection: name(collection),
        ..document(key, ASCII_BODY)
    }
}

/// Registers each document, one after another.
async fn register_all(index: &LanceDbIndex, documents: &[DocumentToRegister]) {
    for document in documents {
        register(index, document).await.unwrap();
    }
}

/// Deletes the document row of `key` in `collection`, leaving its chunk rows,
/// as a registration that stopped partway does.
async fn leave_only_chunk_rows(data_dir: &Path, collection: &str, key: &str) {
    let table = open_tables(data_dir).await.document;
    let row = col("collection")
        .eq(lit(collection))
        .and(col("key").eq(lit(key)));
    table.delete(&row).await.unwrap();
}

async fn versions(table: &lancedb::Table) -> Vec<u64> {
    let listed = table.list_versions().await.unwrap();
    listed.iter().map(|version| version.version).collect()
}

/// The keys in `data_dir`, as (collection, key) pairs, from the document rows
/// and from the chunk rows.
async fn keys_in(data_dir: &Path) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let documents = document_rows(data_dir).await;
    let documents = documents
        .into_iter()
        .map(|row| (row.collection, row.key))
        .collect();
    let mut chunks: Vec<_> = chunk_rows(data_dir)
        .await
        .into_iter()
        .map(|row| (row.collection, row.key))
        .collect();
    chunks.dedup();
    (documents, chunks)
}

fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(collection, key)| ((*collection).to_owned(), (*key).to_owned()))
        .collect()
}

/// The calls into LanceDB that wrote or deleted rows, in order.
fn writes(hooks: &TestHooks) -> Vec<LanceCall> {
    let calls = hooks.calls();
    let write = |call: &LanceCall| matches!(call, LanceCall::WriteRows(_));
    calls.into_iter().filter(write).collect()
}

// ---------------------------------------------------------------- reading and deleting documents

#[tokio::test]
async fn functions_that_find_no_lancedb_directory_answer_without_creating_it() {
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);

    let found = index.has_document(&name(COLLECTION), &key("a.md")).await;
    let counts = index.document_counts().await;
    let tables = index.tables_for_query().await;
    let deleted = index
        .delete_others(&name(COLLECTION), &HashSet::new(), &[])
        .await;
    let deleted_one = index.delete_document(&name(COLLECTION), &key("a.md")).await;
    let maintained = index.maintain().await;

    assert!(!found.unwrap());
    assert!(counts.unwrap().is_empty());
    assert!(tables.unwrap().is_none());
    assert_eq!(deleted.unwrap(), 0);
    assert!(matches!(deleted_one, Err(IndexError::DocumentNotFound)));
    maintained.unwrap();
    assert!(!data_dir.path().join("lancedb").exists());
    assert_eq!(hooks.count(LanceCall::Connect), 0);
}

#[tokio::test]
async fn a_document_another_component_registers_later_is_found() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, reader) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let writer = index_in(data_dir.path(), embedding, &Arc::new(TestHooks::default()));
    let before = reader.has_document(&name(COLLECTION), &key("a.md")).await;

    register(&writer, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let after = reader.has_document(&name(COLLECTION), &key("a.md")).await;
    // The reader now holds the tables, and still reads what is written later.
    register(&writer, &document("b.md", ASCII_BODY))
        .await
        .unwrap();
    let later = reader.has_document(&name(COLLECTION), &key("b.md")).await;

    assert_eq!(
        (before.unwrap(), after.unwrap(), later.unwrap()),
        (false, true, true)
    );
}

#[tokio::test]
async fn tables_another_component_creates_after_a_read_are_found() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, reader) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    std::fs::create_dir(data_dir.path().join("lancedb")).unwrap();
    let writer = index_in(data_dir.path(), embedding, &Arc::new(TestHooks::default()));
    let before = reader.has_document(&name(COLLECTION), &key("a.md")).await;

    register(&writer, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let after = reader.has_document(&name(COLLECTION), &key("a.md")).await;

    assert_eq!((before.unwrap(), after.unwrap()), (false, true));
}

#[tokio::test]
async fn has_document_answers_for_the_key_in_the_collection_only() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    // Each name is both a collection name and, with ".md", a key.
    let special = ["it's", "100%", "a_b", r"back\slash", "日本語"];
    let special_key = |special: &str| format!("{special}.md");
    let mut documents: Vec<_> = special
        .iter()
        .map(|special| document_in(special, &special_key(special)))
        .collect();
    documents.push(document_in("aXb", "aXb.md"));
    documents.push(document_in("other", "elsewhere.md"));
    documents.push(document_in("it's", "chunks-only.md"));
    register_all(&index, &documents).await;
    leave_only_chunk_rows(data_dir.path(), "it's", "chunks-only.md").await;

    for registered in special {
        let (collection, key) = (name(registered), key(&special_key(registered)));
        let found = index.has_document(&collection, &key).await;
        assert!(found.unwrap(), "{registered}");
    }
    let absent = [
        ("it's", "missing.md"),
        ("it's", "elsewhere.md"),
        ("it's", "chunks-only.md"),
        // As patterns, these would match "100%" and "aXb".
        ("1%", "1%.md"),
        ("aX_", "aX_.md"),
    ];
    for (collection, absent) in absent {
        let found = index.has_document(&name(collection), &key(absent)).await;
        assert!(!found.unwrap(), "{collection} {absent}");
    }
}

#[tokio::test]
async fn document_counts_counts_document_rows_of_each_collection() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let documents = [
        document_in("a", "1.md"),
        document_in("a", "2.md"),
        document_in("a", "chunks-only.md"),
        document_in("b", "1.md"),
        document_in("emptied", "1.md"),
    ];
    register_all(&index, &documents).await;
    leave_only_chunk_rows(data_dir.path(), "a", "chunks-only.md").await;
    let emptied = index.delete_document(&name("emptied"), &key("1.md")).await;
    emptied.unwrap();

    let counts = index.document_counts().await.unwrap();

    assert_eq!(counts, BTreeMap::from([(name("a"), 2), (name("b"), 1)]));
}

#[tokio::test]
async fn delete_document_deletes_the_rows_of_that_document_in_both_tables() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    let kept = document("b.md", MIXED_BODY);
    register_all(&index, &[document("a.md", ASCII_BODY), kept.clone()]).await;
    let written = writes(&hooks).len();

    index
        .delete_document(&name(COLLECTION), &key("a.md"))
        .await
        .unwrap();

    assert_index_holds(data_dir.path(), &[&kept], &embedding).await;
    let deletions = &writes(&hooks)[written..];
    let order = [
        LanceCall::WriteRows(TableName::Chunk),
        LanceCall::WriteRows(TableName::Document),
    ];
    assert_eq!(deletions, order);
}

#[tokio::test]
async fn delete_document_of_a_missing_or_chunk_only_key_is_document_not_found() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("chunks-only.md", ASCII_BODY))
        .await
        .unwrap();
    leave_only_chunk_rows(data_dir.path(), COLLECTION, "chunks-only.md").await;

    let missing = index
        .delete_document(&name(COLLECTION), &key("missing.md"))
        .await;
    let chunks_only = index
        .delete_document(&name(COLLECTION), &key("chunks-only.md"))
        .await;

    assert!(matches!(missing, Err(IndexError::DocumentNotFound)));
    assert!(matches!(chunks_only, Err(IndexError::DocumentNotFound)));
    assert_eq!(
        keys_in(data_dir.path()).await.1,
        pairs(&[(COLLECTION, "chunks-only.md")])
    );
}

#[tokio::test]
async fn tables_whose_creation_stopped_partway_are_read_but_left_to_be_finished() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, writer) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&writer, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    drop_index_on(&tables_as_they_are(data_dir.path()).await.document, "key").await;
    let index = index_in(data_dir.path(), embedding, &Arc::new(TestHooks::default()));

    let found = index.has_document(&name(COLLECTION), &key("a.md")).await;
    let tables = tables_as_they_are(data_dir.path()).await;
    let indexes_after_reading = indexed_columns(&tables.document).await.len();
    let outcome = register(&index, &document("a.md", ASCII_BODY)).await;

    assert!(found.unwrap());
    assert_eq!(indexes_after_reading, 3);
    assert_eq!(outcome.unwrap(), DocumentOutcome::Unchanged);
    let tables = tables_as_they_are(data_dir.path()).await;
    assert_eq!(indexed_columns(&tables.document).await.len(), 4);
}

#[tokio::test]
async fn first_reads_at_once_open_the_tables_once() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, writer) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    register(&writer, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let hooks = Arc::new(TestHooks::default());
    let index = index_in(data_dir.path(), embedding, &hooks);
    let (collection, key) = (name(COLLECTION), key("a.md"));

    let reads = (0..8).map(|_| index.has_document(&collection, &key));
    let found = futures::future::join_all(reads).await;

    assert!(found.into_iter().all(|found| found.unwrap()));
    assert_eq!(hooks.count(LanceCall::Connect), 1);
    assert_eq!(hooks.count(LanceCall::OpenTable(TableName::Chunk)), 1);
    assert_eq!(hooks.count(LanceCall::OpenTable(TableName::Document)), 1);
}

#[tokio::test]
async fn after_a_panic_while_opening_the_tables_to_read_the_next_call_connects_again() {
    prepare_full_text_search().await;
    let (data_dir, writer) = store_with(&Arc::new(TestHooks::default()));
    writer.tables_to_write(Some(&model(8))).await.unwrap();
    let hooks = Arc::new(TestHooks::default());
    let store = Arc::new(Store::new(data_dir.path().to_path_buf(), hooks.clone()));
    hooks.plan(LanceCall::OpenTable(TableName::Chunk), Plan::Panic);

    let panicked = tokio::spawn({
        let store = store.clone();
        async move { store.tables_to_read().await.map(|_| ()) }
    })
    .await;
    let after = store.tables_to_read().await;

    assert!(panicked.unwrap_err().is_panic());
    assert!(after.unwrap().is_some());
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn delete_others_deletes_the_keys_neither_kept_nor_in_an_unreadable_directory() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let in_c = [
        "it's.md",
        "100%.md",
        "a_b.md",
        r"back\slash.md",
        "日本語.md",
        "docs/a.md",
        "docs/sub/b.md",
        "docs2/c.md",
        "docs",
    ];
    let mut documents: Vec<_> = in_c.iter().map(|key| document_in("c", key)).collect();
    documents.push(document_in("other", "100%.md"));
    register_all(&index, &documents).await;

    let kept = keys(&["it's.md", "日本語.md"]);
    let deleted = index
        .delete_others(&name("c"), &kept, &["docs".to_owned()])
        .await;

    assert_eq!(deleted.unwrap(), 5);
    let mut left = pairs(&[
        ("c", "docs/a.md"),
        ("c", "docs/sub/b.md"),
        ("c", "it's.md"),
        ("c", "日本語.md"),
        ("other", "100%.md"),
    ]);
    left.sort();
    assert_eq!(keys_in(data_dir.path()).await, (left.clone(), left));
}

#[tokio::test]
async fn an_unreadable_directory_with_wildcards_keeps_only_the_keys_in_it() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let in_c = ["a%_b/1.md", "aX_b/2.md", "a%Xb/3.md", "abcd/4.md"];
    let documents: Vec<_> = in_c.iter().map(|key| document_in("c", key)).collect();
    register_all(&index, &documents).await;

    let deleted = index
        .delete_others(&name("c"), &HashSet::new(), &["a%_b".to_owned()])
        .await;

    assert_eq!(deleted.unwrap(), 3);
    let left = pairs(&[("c", "a%_b/1.md")]);
    assert_eq!(keys_in(data_dir.path()).await, (left.clone(), left));
}

#[tokio::test]
async fn delete_others_without_keys_or_directories_deletes_the_whole_collection() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let documents = [
        document_in("c", "1.md"),
        document_in("c", "2.md"),
        document_in("other", "1.md"),
    ];
    register_all(&index, &documents).await;

    let deleted = index.delete_others(&name("c"), &HashSet::new(), &[]).await;

    assert_eq!(deleted.unwrap(), 2);
    let left = pairs(&[("other", "1.md")]);
    assert_eq!(keys_in(data_dir.path()).await, (left.clone(), left));
}

#[tokio::test]
async fn chunk_only_keys_are_deleted_without_being_counted() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register_all(
        &index,
        &[document("a.md", ASCII_BODY), document("b.md", ASCII_BODY)],
    )
    .await;
    leave_only_chunk_rows(data_dir.path(), COLLECTION, "b.md").await;

    let deleted = index
        .delete_others(&name(COLLECTION), &keys(&["a.md"]), &[])
        .await;

    assert_eq!(deleted.unwrap(), 0);
    let left = pairs(&[(COLLECTION, "a.md")]);
    assert_eq!(keys_in(data_dir.path()).await, (left.clone(), left));
}

#[tokio::test]
async fn a_document_without_chunk_rows_is_deleted_and_counted() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register_all(
        &index,
        &[document("a.md", ASCII_BODY), document("empty.md", "")],
    )
    .await;
    let with_chunks = pairs(&[(COLLECTION, "a.md")]);
    assert_eq!(keys_in(data_dir.path()).await.1, with_chunks);

    let deleted = index
        .delete_others(&name(COLLECTION), &keys(&["a.md"]), &[])
        .await;

    assert_eq!(deleted.unwrap(), 1);
    assert_eq!(
        keys_in(data_dir.path()).await,
        (with_chunks.clone(), with_chunks)
    );
}

#[tokio::test]
async fn delete_others_with_nothing_to_delete_writes_nothing() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let tables = open_tables(data_dir.path()).await;
    let before = (
        versions(&tables.document).await,
        versions(&tables.chunk).await,
    );
    let written = writes(&hooks).len();

    let deleted = index
        .delete_others(&name(COLLECTION), &keys(&["a.md"]), &[])
        .await;

    assert_eq!(deleted.unwrap(), 0);
    let after = (
        versions(&tables.document).await,
        versions(&tables.chunk).await,
    );
    assert_eq!(after, before);
    assert_eq!(writes(&hooks).len(), written);
}

#[tokio::test]
async fn delete_others_deletes_in_batches_with_the_chunk_rows_first() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, mut index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    index.key_batch_len = 2;
    let documents: Vec<_> = (0..5)
        .map(|number| document(&format!("{number}.md"), ASCII_BODY))
        .collect();
    register_all(&index, &documents).await;
    let written = writes(&hooks).len();

    let deleted = index
        .delete_others(&name(COLLECTION), &HashSet::new(), &[])
        .await;

    assert_eq!(deleted.unwrap(), 5);
    let chunk_then_document = [
        LanceCall::WriteRows(TableName::Chunk),
        LanceCall::WriteRows(TableName::Document),
    ];
    assert_eq!(writes(&hooks)[written..], chunk_then_document.repeat(3));
    assert_eq!(keys_in(data_dir.path()).await, (Vec::new(), Vec::new()));
}

#[tokio::test]
async fn a_failure_in_the_second_batch_returns_what_the_first_batch_deleted() {
    prepare_full_text_search().await;
    let not_first_batch = pairs(&[
        (COLLECTION, "2.md"),
        (COLLECTION, "3.md"),
        (COLLECTION, "4.md"),
    ]);
    // The keys are deleted in their order, two at a time: "0.md" and "1.md"
    // first, then "2.md" and "3.md", whose deletion fails in `table`.
    let cases = [
        (TableName::Chunk, not_first_batch.clone()),
        (TableName::Document, pairs(&[(COLLECTION, "4.md")])),
    ];
    for (table, chunk_keys_left) in cases {
        let hooks = Arc::new(TestHooks::default());
        let (data_dir, mut index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
        index.key_batch_len = 2;
        let documents: Vec<_> = (0..5)
            .map(|number| document(&format!("{number}.md"), ASCII_BODY))
            .collect();
        register_all(&index, &documents).await;
        hooks.plan(LanceCall::WriteRows(table), Plan::Allow);
        hooks.plan(LanceCall::WriteRows(table), Plan::Fail);

        let deleted = index
            .delete_others(&name(COLLECTION), &HashSet::new(), &[])
            .await;

        let Err(PartialDelete { deleted, error }) = deleted else {
            panic!("{table:?}: expected the deletion to stop, got {deleted:?}");
        };
        assert_eq!(deleted, 2, "{table:?}");
        assert!(matches!(error, IndexError::CannotWrite { .. }), "{table:?}");
        assert_eq!(
            keys_in(data_dir.path()).await,
            (not_first_batch.clone(), chunk_keys_left),
            "{table:?}"
        );
    }
}

#[tokio::test]
async fn a_failure_reading_the_keys_stops_before_deleting_anything() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding.clone(), &hooks);
    let registered = document("a.md", ASCII_BODY);
    register(&index, &registered).await.unwrap();
    hooks.plan(LanceCall::ReadRows(TableName::Chunk), Plan::Fail);

    let deleted = index
        .delete_others(&name(COLLECTION), &HashSet::new(), &[])
        .await;

    let Err(PartialDelete { deleted, error }) = deleted else {
        panic!("expected the deletion to stop, got {deleted:?}");
    };
    assert_eq!(deleted, 0);
    assert!(matches!(error, IndexError::CannotRead { .. }));
    assert_index_holds(data_dir.path(), &[&registered], &embedding).await;
    // The connection was dropped, so the next call connects again.
    index.document_counts().await.unwrap();
    assert_eq!(hooks.count(LanceCall::Connect), 2);
}

#[tokio::test]
async fn deleting_different_documents_at_once_deletes_them_all() {
    prepare_full_text_search().await;
    let embedding = embedding_of(MODEL_NAME, 8);
    let (data_dir, index) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
    let collections = ["c1", "c2", "c3", "c4"];
    let documents: Vec<_> = collections
        .iter()
        .flat_map(|collection| {
            [
                document_in(collection, "1.md"),
                document_in(collection, "2.md"),
            ]
        })
        .collect();
    register_all(&index, &documents).await;
    let none = HashSet::new();
    let [c1, c2, c3, c4] = collections.map(name);
    let first_key = key("1.md");

    let (first, second, third, fourth) = tokio::join!(
        index.delete_others(&c1, &none, &[]),
        index.delete_others(&c2, &none, &[]),
        index.delete_document(&c3, &first_key),
        index.delete_document(&c4, &first_key),
    );

    assert_eq!((first.unwrap(), second.unwrap()), (2, 2));
    third.unwrap();
    fourth.unwrap();
    let left = pairs(&[("c3", "2.md"), ("c4", "2.md")]);
    assert_eq!(keys_in(data_dir.path()).await, (left.clone(), left));
}

// ---------------------------------------------------------------- the tables for `query`

#[tokio::test]
async fn tables_for_query_are_none_until_both_tables_have_their_full_text_indexes() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    std::fs::create_dir(data_dir.path().join("lancedb")).unwrap();
    let without_tables = index.tables_for_query().await.unwrap();
    // The first index made on the document table is its full-text search
    // index.
    hooks.plan(LanceCall::CreateIndex(TableName::Document), Plan::Fail);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap_err();

    let without_index = index.tables_for_query().await.unwrap();
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let finished = index.tables_for_query().await.unwrap();

    assert!(without_tables.is_none());
    assert!(without_index.is_none());
    assert!(finished.is_some());
}

#[tokio::test]
async fn tables_for_query_are_none_while_the_chunk_table_lacks_its_full_text_index() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    drop_index_on(&tables_as_they_are(data_dir.path()).await.chunk, "text").await;

    let tables = index.tables_for_query().await.unwrap();

    assert!(tables.is_none());
}

#[tokio::test]
async fn tables_for_query_need_only_the_full_text_indexes() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();
    let tables = tables_as_they_are(data_dir.path()).await;
    for (table, vector) in [(&tables.document, "vectors"), (&tables.chunk, "vector")] {
        for column in ["collection", "key", vector] {
            drop_index_on(table, column).await;
        }
    }

    let tables = index.tables_for_query().await.unwrap();

    assert!(tables.is_some());
}

#[tokio::test]
async fn tables_for_query_stay_at_their_version_until_called_again() {
    prepare_full_text_search().await;
    let (_data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    let first = document("a.md", ASCII_BODY);
    register(&index, &first).await.unwrap();
    let pinned = index.tables_for_query().await.unwrap().unwrap();
    let pinned_version = pinned.document.version().await.unwrap();

    let second = document("b.md", MIXED_BODY);
    let outcome = register(&index, &second).await;
    let next = index.tables_for_query().await.unwrap().unwrap();

    assert_eq!(outcome.unwrap(), DocumentOutcome::Added);
    assert!(
        index
            .has_document(&name(COLLECTION), &key("b.md"))
            .await
            .unwrap()
    );
    assert_eq!(pinned.document.version().await.unwrap(), pinned_version);
    assert_eq!(pinned.document.count_rows(None).await.unwrap(), 1);
    assert_eq!(
        pinned.chunk.count_rows(None).await.unwrap(),
        first.chunks.len()
    );
    assert_eq!(next.document.count_rows(None).await.unwrap(), 2);
    assert_eq!(
        next.chunk.count_rows(None).await.unwrap(),
        first.chunks.len() + second.chunks.len()
    );
}

// ---------------------------------------------------------------- maintenance

#[tokio::test]
async fn maintain_takes_the_three_steps_on_each_table_in_order() {
    prepare_full_text_search().await;
    let hooks = Arc::new(TestHooks::default());
    let (_data_dir, index) = index_with(embedding_of(MODEL_NAME, 8), &hooks);
    register(&index, &document("a.md", ASCII_BODY))
        .await
        .unwrap();

    index.maintain().await.unwrap();

    let optimize = |call: &LanceCall| matches!(call, LanceCall::Optimize(..));
    let steps: Vec<_> = hooks.calls().into_iter().filter(optimize).collect();
    let seven_days = MaintenanceStep::Prune {
        older_than: TimeDelta::days(7),
    };
    let on = |table| {
        [MaintenanceStep::Compact, seven_days, MaintenanceStep::Index]
            .map(|step| LanceCall::Optimize(table, step))
    };
    let expected: Vec<_> = on(TableName::Chunk)
        .into_iter()
        .chain(on(TableName::Document))
        .collect();
    assert_eq!(steps, expected);
}

#[tokio::test]
async fn maintain_keeps_the_versions_just_made() {
    prepare_full_text_search().await;
    let (data_dir, index) =
        index_with(embedding_of(MODEL_NAME, 8), &Arc::new(TestHooks::default()));
    register_all(
        &index,
        &[document("a.md", ASCII_BODY), document("b.md", MIXED_BODY)],
    )
    .await;
    let tables = open_tables(data_dir.path()).await;
    let before = [
        versions(&tables.document).await,
        versions(&tables.chunk).await,
    ];

    index.maintain().await.unwrap();

    let after = [
        versions(&tables.document).await,
        versions(&tables.chunk).await,
    ];
    for (before, after) in before.iter().zip(&after) {
        assert!(
            before.iter().all(|version| after.contains(version)),
            "{before:?} {after:?}"
        );
    }
}

#[test]
fn the_maintenance_steps_compact_then_clean_up_versions_older_than_seven_days_then_index() {
    let [compact, prune, indexing] = MaintenanceStep::all().map(MaintenanceStep::action);

    assert!(matches!(
        compact,
        OptimizeAction::Compact { options, remap_options: None }
            if options == CompactionOptions::default()
    ));
    assert!(matches!(
        prune,
        OptimizeAction::Prune {
            older_than: Some(older_than),
            delete_unverified: None,
            error_if_tagged_old_versions: None,
        } if older_than == TimeDelta::days(7)
    ));
    let OptimizeAction::Index(options) = indexing else {
        panic!("the last step does not index");
    };
    // `OptimizeOptions` has no `PartialEq`.
    let default = OptimizeOptions::default();
    assert_eq!(format!("{options:?}"), format!("{default:?}"));
}

// ---------------------------------------------------------------- failures of the functions that do not create the tables

/// A function of the component that opens the tables but does not create
/// them: it reads them, or has nothing to write without them.
#[derive(Clone, Copy, Debug)]
enum OpensTablesOnly {
    HasDocument,
    DocumentCounts,
    DeleteDocument,
    DeleteOthers,
    TablesForQuery,
    Maintain,
}

impl OpensTablesOnly {
    /// Calls the function on the document `a.md` of [`COLLECTION`], keeping
    /// only the error. `delete_others` keeps no key, so it deletes `a.md`.
    async fn call(self, index: &LanceDbIndex) -> Result<(), IndexError> {
        let collection = name(COLLECTION);
        let key = key("a.md");
        match self {
            Self::HasDocument => index.has_document(&collection, &key).await.map(drop),
            Self::DocumentCounts => index.document_counts().await.map(drop),
            Self::DeleteDocument => index.delete_document(&collection, &key).await,
            Self::DeleteOthers => {
                let keep = HashSet::new();
                match index.delete_others(&collection, &keep, &[]).await {
                    Ok(_) => Ok(()),
                    Err(stopped) => {
                        // It stops before it finishes deleting the one document.
                        assert_eq!(stopped.deleted, 0);
                        Err(stopped.error)
                    }
                }
            }
            Self::TablesForQuery => index.tables_for_query().await.map(drop),
            Self::Maintain => index.maintain().await,
        }
    }
}

/// The error that a failed call into LanceDB should make.
#[derive(Clone, Copy, Debug)]
enum ExpectedError {
    CannotRead,
    CannotWrite,
}

#[tokio::test]
async fn each_failure_is_an_error_reading_or_writing_and_the_next_call_connects_again() {
    use ExpectedError::{CannotRead, CannotWrite};
    use LanceCall::{Connect, OpenTable, Optimize, ReadRows, WriteRows};
    use OpensTablesOnly::*;
    use TableName::{Chunk, Document};
    prepare_full_text_search().await;
    let (compact, index_step) = (MaintenanceStep::Compact, MaintenanceStep::Index);
    let cases = [
        (HasDocument, Connect, CannotRead),
        (HasDocument, OpenTable(Chunk), CannotRead),
        (HasDocument, ReadRows(Document), CannotRead),
        (DocumentCounts, Connect, CannotRead),
        (DocumentCounts, OpenTable(Chunk), CannotRead),
        (DocumentCounts, ReadRows(Document), CannotRead),
        (DeleteDocument, Connect, CannotRead),
        (DeleteDocument, OpenTable(Chunk), CannotRead),
        (DeleteDocument, ReadRows(Document), CannotRead),
        (DeleteDocument, WriteRows(Chunk), CannotWrite),
        (DeleteDocument, WriteRows(Document), CannotWrite),
        (DeleteOthers, Connect, CannotRead),
        (DeleteOthers, OpenTable(Chunk), CannotRead),
        (DeleteOthers, ReadRows(Chunk), CannotRead),
        (DeleteOthers, ReadRows(Document), CannotRead),
        (DeleteOthers, WriteRows(Chunk), CannotWrite),
        (DeleteOthers, WriteRows(Document), CannotWrite),
        (TablesForQuery, Connect, CannotRead),
        (TablesForQuery, OpenTable(Chunk), CannotRead),
        (TablesForQuery, OpenTable(Document), CannotRead),
        (Maintain, Connect, CannotRead),
        (Maintain, OpenTable(Chunk), CannotRead),
        (Maintain, Optimize(Chunk, compact), CannotWrite),
        (Maintain, Optimize(Document, index_step), CannotWrite),
    ];
    let embedding = embedding_of(MODEL_NAME, 8);
    for (function, call, expected) in cases {
        let (data_dir, writer) = index_with(embedding.clone(), &Arc::new(TestHooks::default()));
        register(&writer, &document("a.md", ASCII_BODY))
            .await
            .unwrap();
        let hooks = Arc::new(TestHooks::default());
        let index = index_in(data_dir.path(), embedding.clone(), &hooks);
        hooks.plan(call, Plan::Fail);

        let first = function.call(&index).await;
        let second = function.call(&index).await;

        let case = format!("{function:?} with {call:?} failing");
        match (expected, first) {
            (CannotRead, Err(IndexError::CannotRead { .. }))
            | (CannotWrite, Err(IndexError::CannotWrite { .. })) => {}
            (_, first) => panic!("{case}: {first:?}"),
        }
        second.unwrap_or_else(|error| panic!("{case}, then: {error:?}"));
        // Nothing was held after the error, so the second call connected.
        assert_eq!(hooks.count(Connect), 2, "{case}");
    }
}
