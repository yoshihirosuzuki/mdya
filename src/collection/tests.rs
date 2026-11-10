//! The unit tests of the `collection` component.

use std::collections::HashSet;
use std::error::Error;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use lancedb::arrow::arrow_array::{RecordBatch, StringArray};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
use serde_json::{Map, json};
use tempfile::TempDir;

use super::store::{Hooks, LANCEDB_DIR, LanceCall, TABLE};
use super::*;
use crate::index::PartialDelete;
use crate::index::test_double::{IndexCall, TestIndex};

#[derive(Clone, Copy, Debug)]
enum Plan {
    Fail,
    Panic,
}

/// Hooks that record the calls into LanceDB and make planned calls fail or
/// panic. Unplanned calls go ahead.
#[derive(Default)]
struct TestHooks {
    calls: Mutex<Vec<LanceCall>>,
    plans: Mutex<Vec<(LanceCall, Plan)>>,
}

impl TestHooks {
    /// Makes the next `call` do `plan`.
    fn plan(&self, call: LanceCall, plan: Plan) {
        self.plans.lock().unwrap().push((call, plan));
    }

    fn calls(&self) -> Vec<LanceCall> {
        self.calls.lock().unwrap().clone()
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
            Some(Plan::Fail) => Err(planned_failure(call).into()),
            Some(Plan::Panic) => panic!("planned panic in {call:?}"),
            None => Ok(()),
        }
    }
}

fn planned_failure(call: LanceCall) -> String {
    format!("planned failure of {call:?}")
}

/// A component over a data directory that does not exist yet.
struct Fixture {
    /// Holds the directory for as long as the fixture lives.
    _parent: TempDir,
    data_dir: PathBuf,
    hooks: Arc<TestHooks>,
    index: Arc<TestIndex>,
    collections: Arc<LanceDbCollections>,
}

impl Fixture {
    fn new() -> Self {
        let parent = tempfile::tempdir().unwrap();
        let data_dir = parent.path().join("data");
        let hooks = Arc::new(TestHooks::default());
        let index = Arc::new(TestIndex::new());
        let collections = component(&data_dir, index.clone(), &hooks);
        Self {
            _parent: parent,
            data_dir,
            hooks,
            index,
            collections,
        }
    }

    /// A new component over the same data directory, which has opened nothing
    /// yet.
    fn reopened(self) -> Self {
        self.reopened_with(TestIndex::new())
    }

    fn reopened_with(self, index: TestIndex) -> Self {
        let hooks = Arc::new(TestHooks::default());
        let index = Arc::new(index);
        let collections = component(&self.data_dir, index.clone(), &hooks);
        Self {
            hooks,
            index,
            collections,
            ..self
        }
    }

    /// A component over a data directory that holds one collection, `notes`,
    /// and that has opened nothing yet.
    async fn holding_notes() -> Self {
        Self::holding_notes_with(TestIndex::new()).await
    }

    async fn holding_notes_with(index: TestIndex) -> Self {
        let fixture = Self::new();
        fixture.create(collection("notes")).await;
        fixture.reopened_with(index)
    }

    async fn create(&self, collection: CollectionInfo) {
        self.collections
            .create_collection(collection)
            .await
            .unwrap();
    }

    async fn get(&self, text: &str) -> Option<CollectionInfo> {
        let found = self.collections.get_collection(&name(text)).await;
        found.unwrap()
    }

    /// Adds a row whose name the rules would refuse. The functions that take a
    /// name can be given any string.
    async fn add_directly(&self, collection: &CollectionInfo) {
        let store = &self.collections.store;
        let table = store.table_to_write().await.unwrap();
        store.add(&table, collection).await.unwrap();
    }

    fn index_calls(&self) -> Vec<IndexCall> {
        self.index.calls()
    }

    async fn names(&self) -> Vec<String> {
        let listed = self.collections.list_collections().await.unwrap();
        listed.into_iter().map(|info| info.name.0).collect()
    }

    /// Makes the data directory a regular file, so that nothing can be made
    /// inside it.
    fn make_data_dir_a_file(&self) {
        std::fs::write(&self.data_dir, "").unwrap();
    }

    fn lancedb_dir(&self) -> PathBuf {
        self.data_dir.join(LANCEDB_DIR)
    }

    /// The table, opened over a connection of its own, for writing rows that
    /// the component would refuse to write.
    async fn write_table_directly(&self, rows: RecordBatch) {
        std::fs::create_dir_all(self.lancedb_dir()).unwrap();
        let connection = lancedb::connect(self.lancedb_dir().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        connection
            .create_table(TABLE, rows)
            .execute()
            .await
            .unwrap();
    }
}

fn component(
    data_dir: &Path,
    index: Arc<TestIndex>,
    hooks: &Arc<TestHooks>,
) -> Arc<LanceDbCollections> {
    Arc::new(LanceDbCollections::new(
        data_dir.to_path_buf(),
        index,
        hooks.clone(),
    ))
}

fn name(text: &str) -> CollectionName {
    CollectionName(text.to_owned())
}

fn attributes(value: serde_json::Value) -> TypeAttributes {
    let serde_json::Value::Object(map) = value else {
        panic!("the attributes of a test are a JSON object");
    };
    TypeAttributes(map)
}

fn collection(text: &str) -> CollectionInfo {
    CollectionInfo {
        name: name(text),
        collection_type: CollectionType("directory".to_owned()),
        attributes: attributes(json!({ "location": "/some/dir" })),
        description: Some("my notes".to_owned()),
    }
}

fn cause_of(error: &CollectionError) -> String {
    error.source().unwrap().to_string()
}

/// One row of the columns the component writes, with `attributes` as given.
fn row_with_attributes(attributes: &str) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("collection_type", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
        Field::new("description", DataType::Utf8, true),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(vec!["notes"])),
            Arc::new(StringArray::from(vec!["directory"])),
            Arc::new(StringArray::from(vec![attributes])),
            Arc::new(StringArray::from(vec![None::<&str>])),
        ],
    )
    .unwrap()
}

// ------------------------------------------------- the check of the name

fn check(name: &str) -> Result<(), CollectionError> {
    check_collection_name(&CollectionName(name.to_owned()))
}

#[test]
fn accepts_ascii_letters_digits_hyphens_and_underscores() {
    for name in ["a", "notes", "Work-Notes_2", "_", "-", "0"] {
        assert!(check(name).is_ok(), "{name:?}");
    }
}

#[test]
fn accepts_a_name_of_64_characters() {
    assert!(check(&"a".repeat(64)).is_ok());
}

#[test]
fn rejects_an_empty_name() {
    assert!(matches!(check(""), Err(CollectionError::NameInvalid)));
}

#[test]
fn rejects_a_name_of_65_characters() {
    assert!(matches!(
        check(&"a".repeat(65)),
        Err(CollectionError::NameInvalid)
    ));
}

#[test]
fn rejects_other_characters() {
    for name in ["my notes", "a.b", "a/b", "a\\b", "メモ", "é", "a\n"] {
        assert!(
            matches!(check(name), Err(CollectionError::NameInvalid)),
            "{name:?}"
        );
    }
}

// ------------------------------------------- creating and answering

#[tokio::test]
async fn a_created_collection_is_answered_as_it_was_given() {
    let fixture = Fixture::new();
    let notes = collection("notes");

    fixture.create(notes.clone()).await;

    let found = fixture.collections.get_collection(&notes.name).await;
    assert_eq!(found.unwrap(), Some(notes.clone()));
    let listed = fixture.collections.list_collections().await;
    assert_eq!(listed.unwrap(), vec![notes]);
}

#[tokio::test]
async fn a_collection_without_a_description_is_answered_without_one() {
    let fixture = Fixture::new();
    let notes = CollectionInfo {
        description: None,
        ..collection("notes")
    };

    fixture.create(notes.clone()).await;

    let found = fixture.collections.get_collection(&notes.name).await;
    assert_eq!(found.unwrap(), Some(notes));
}

// The component holds the attributes without knowing what they mean, so a
// value of any JSON type comes back as it went in.
#[tokio::test]
async fn type_attributes_keep_values_of_every_json_type() {
    let fixture = Fixture::new();
    let notes = CollectionInfo {
        attributes: attributes(json!({
            "text": "it's \"quoted\"",
            "number": 1.5,
            "flag": true,
            "nothing": null,
            "list": [1, "two", [3]],
            "object": { "inner": { "deep": 1 } },
        })),
        ..collection("notes")
    };

    fixture.create(notes.clone()).await;

    let found = fixture.collections.get_collection(&notes.name).await;
    assert_eq!(found.unwrap(), Some(notes));
}

#[tokio::test]
async fn empty_type_attributes_are_kept() {
    let fixture = Fixture::new();
    let notes = CollectionInfo {
        attributes: TypeAttributes(Map::new()),
        ..collection("notes")
    };

    fixture.create(notes.clone()).await;

    let found = fixture.collections.get_collection(&notes.name).await;
    assert_eq!(found.unwrap(), Some(notes));
}

// This value must not be refused here, so that the other operations can still
// be used on a collection that another version of mdya made.
#[tokio::test]
async fn a_collection_type_this_mdya_does_not_know_is_created_and_answered() {
    let fixture = Fixture::new();
    let future = CollectionInfo {
        collection_type: CollectionType("from-a-later-version".to_owned()),
        ..collection("future")
    };

    fixture.create(future.clone()).await;

    let found = fixture.collections.get_collection(&future.name).await;
    assert_eq!(found.unwrap(), Some(future.clone()));
    let listed = fixture.collections.list_collections().await;
    assert_eq!(listed.unwrap(), vec![future]);
}

#[tokio::test]
async fn a_name_against_the_rules_is_refused_before_anything_is_touched() {
    let fixture = Fixture::new();

    let result = fixture
        .collections
        .create_collection(collection("my notes"))
        .await;

    assert!(matches!(result, Err(CollectionError::NameInvalid)));
    assert!(fixture.hooks.calls().is_empty());
    assert!(!fixture.data_dir.exists());
}

#[tokio::test]
async fn a_second_collection_of_the_same_name_is_refused_and_the_first_is_kept() {
    let fixture = Fixture::new();
    let first = collection("notes");
    fixture.create(first.clone()).await;
    let second = CollectionInfo {
        description: Some("another".to_owned()),
        ..collection("notes")
    };

    let result = fixture.collections.create_collection(second).await;

    assert!(matches!(result, Err(CollectionError::AlreadyExists)));
    let listed = fixture.collections.list_collections().await;
    assert_eq!(listed.unwrap(), vec![first]);
}

#[tokio::test]
async fn names_differing_only_in_case_are_different_collections() {
    let fixture = Fixture::new();

    fixture.create(collection("notes")).await;
    fixture.create(collection("Notes")).await;

    assert_eq!(fixture.names().await, ["Notes", "notes"]);
}

#[tokio::test]
async fn the_list_is_in_the_code_point_order_of_the_names() {
    let fixture = Fixture::new();

    for text in ["a", "_", "B", "0", "-", "Z", "b"] {
        fixture.create(collection(text)).await;
    }

    assert_eq!(fixture.names().await, ["-", "0", "B", "Z", "_", "a", "b"]);
}

#[tokio::test]
async fn the_list_holds_every_collection() {
    let fixture = Fixture::new();

    for number in 0..25 {
        fixture.create(collection(&format!("c{number:02}"))).await;
    }

    assert_eq!(fixture.names().await.len(), 25);
}

#[tokio::test]
async fn a_name_of_no_collection_is_answered_with_none() {
    let fixture = Fixture::holding_notes().await;

    let found = fixture.collections.get_collection(&name("papers")).await;

    assert_eq!(found.unwrap(), None);
}

// ---------------------------------------- what is in the data directory

#[test]
fn making_the_component_opens_nothing() {
    let fixture = Fixture::new();

    assert!(fixture.hooks.calls().is_empty());
    assert!(!fixture.data_dir.exists());
}

// A directory that is not there yet means no collections, and the functions
// that answer change nothing.
#[tokio::test]
async fn without_a_data_directory_nothing_is_answered_and_nothing_is_made() {
    let fixture = Fixture::new();

    let listed = fixture.collections.list_collections().await;
    let found = fixture.collections.get_collection(&name("notes")).await;

    assert!(listed.unwrap().is_empty());
    assert_eq!(found.unwrap(), None);
    assert!(!fixture.data_dir.exists());
    assert!(fixture.hooks.calls().is_empty());
}

#[tokio::test]
async fn without_the_table_nothing_is_answered() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.lancedb_dir()).unwrap();

    let listed = fixture.collections.list_collections().await;
    let found = fixture.collections.get_collection(&name("notes")).await;

    assert!(listed.unwrap().is_empty());
    assert_eq!(found.unwrap(), None);
}

#[tokio::test]
async fn creating_makes_the_data_directory_and_the_table_in_lancedb() {
    let fixture = Fixture::new();

    fixture.create(collection("notes")).await;

    let table = fixture.lancedb_dir().join(format!("{TABLE}.lance"));
    assert_eq!(LANCEDB_DIR, "lancedb");
    assert_eq!(TABLE, "collection");
    assert!(table.is_dir(), "{}", table.display());
}

// LanceDB takes a path that begins with "db" for a LanceDB server. Tests run in
// the package's directory, so the data directory is made there, and is named
// relative to it.
#[tokio::test]
async fn a_relative_data_directory_beginning_with_db_opens_the_lancedb_in_it() {
    let data_dir = tempfile::Builder::new()
        .prefix("db")
        .tempdir_in(".")
        .unwrap();
    let relative = PathBuf::from(data_dir.path().file_name().unwrap());
    let hooks = Arc::new(TestHooks::default());
    let collections = component(&relative, Arc::new(TestIndex::new()), &hooks);

    collections
        .create_collection(collection("notes"))
        .await
        .unwrap();

    assert!(data_dir.path().join(LANCEDB_DIR).is_dir());
    assert_eq!(collections.list_collections().await.unwrap().len(), 1);
}

// The reader has already opened the LanceDB when the second collection is
// written.
#[tokio::test]
async fn what_another_connection_wrote_is_seen_by_the_next_call() {
    let writer = Fixture::new();
    let hooks = Arc::new(TestHooks::default());
    let reader = component(&writer.data_dir, Arc::new(TestIndex::new()), &hooks);
    writer.create(collection("first")).await;
    assert_eq!(reader.list_collections().await.unwrap().len(), 1);

    writer.create(collection("second")).await;

    assert_eq!(reader.list_collections().await.unwrap().len(), 2);
    let found = reader.get_collection(&name("second")).await;
    assert_eq!(found.unwrap(), Some(collection("second")));
}

// A name the rules would refuse can still be passed to the functions that take
// a name, and it must not change the condition.
#[tokio::test]
async fn a_name_holding_sql_characters_picks_only_its_own_row() {
    let fixture = Fixture::new();
    fixture.create(collection("notes")).await;
    let store = &fixture.collections.store;
    let table = store.table_to_write().await.unwrap();
    let quoted = collection("it's");
    store.add(&table, &quoted).await.unwrap();

    let found = fixture.collections.get_collection(&name("it's")).await;
    assert_eq!(found.unwrap(), Some(quoted));
    for other in ["x' OR '1'='1", "%", "_otes", ""] {
        let found = fixture.collections.get_collection(&name(other)).await;
        assert_eq!(found.unwrap(), None, "{other:?}");
    }
}

// -------------------------------------------------------- failures

#[tokio::test]
async fn the_functions_that_answer_report_a_failed_call_as_cannot_read() {
    for call in [
        LanceCall::Connect,
        LanceCall::OpenTable,
        LanceCall::ReadRows,
    ] {
        let fixture = Fixture::holding_notes().await;
        fixture.hooks.plan(call, Plan::Fail);
        let listed = fixture.collections.list_collections().await.unwrap_err();
        assert!(
            matches!(listed, CollectionError::CannotRead { .. }),
            "{call:?}"
        );
        assert_eq!(cause_of(&listed), planned_failure(call));

        let fixture = fixture.reopened();
        fixture.hooks.plan(call, Plan::Fail);
        let found = fixture.collections.get_collection(&name("notes")).await;
        let found = found.unwrap_err();
        assert!(
            matches!(found, CollectionError::CannotRead { .. }),
            "{call:?}"
        );
        assert_eq!(cause_of(&found), planned_failure(call));
    }
}

#[tokio::test]
async fn creating_reports_each_failed_call_as_its_kind_of_failure() {
    for (call, is_read) in [
        (LanceCall::Connect, false),
        (LanceCall::OpenTable, true),
        (LanceCall::ReadRows, true),
        (LanceCall::AddRow, false),
    ] {
        let fixture = Fixture::holding_notes().await;
        fixture.hooks.plan(call, Plan::Fail);

        let result = fixture
            .collections
            .create_collection(collection("papers"))
            .await;

        let error = result.unwrap_err();
        if is_read {
            assert!(
                matches!(error, CollectionError::CannotRead { .. }),
                "{call:?}"
            );
        } else {
            assert!(
                matches!(error, CollectionError::CannotWrite { .. }),
                "{call:?}"
            );
        }
        assert_eq!(cause_of(&error), planned_failure(call));
        assert_eq!(fixture.names().await, ["notes"], "{call:?}");
    }
}

#[tokio::test]
async fn creating_reports_a_failure_to_make_the_table_as_cannot_write() {
    let fixture = Fixture::new();
    fixture.hooks.plan(LanceCall::CreateTable, Plan::Fail);

    let result = fixture
        .collections
        .create_collection(collection("notes"))
        .await;

    let error = result.unwrap_err();
    assert!(matches!(error, CollectionError::CannotWrite { .. }));
    assert_eq!(cause_of(&error), planned_failure(LanceCall::CreateTable));
}

#[tokio::test]
async fn a_data_directory_that_cannot_be_made_cannot_be_written() {
    let fixture = Fixture::new();
    fixture.make_data_dir_a_file();

    let result = fixture
        .collections
        .create_collection(collection("notes"))
        .await;

    assert!(matches!(result, Err(CollectionError::CannotWrite { .. })));
}

// Windows answers that the path does not exist, which is not a failure.
#[cfg(unix)]
#[tokio::test]
async fn a_data_directory_that_cannot_be_looked_into_cannot_be_read() {
    let fixture = Fixture::new();
    fixture.make_data_dir_a_file();

    let listed = fixture.collections.list_collections().await;

    assert!(matches!(listed, Err(CollectionError::CannotRead { .. })));
}

/// The files directly in the directory `part` of the table.
fn files_of_the_table(fixture: &Fixture, part: &str) -> Vec<PathBuf> {
    let table = fixture.lancedb_dir().join(format!("{TABLE}.lance"));
    let entries = std::fs::read_dir(table.join(part)).unwrap();
    entries.map(|entry| entry.unwrap().path()).collect()
}

// A damaged table must not be answered as a table without collections.
#[tokio::test]
async fn a_table_whose_versions_are_damaged_cannot_be_read() {
    let fixture = Fixture::holding_notes().await;
    for file in files_of_the_table(&fixture, "_versions") {
        std::fs::write(file, "damaged").unwrap();
    }

    let listed = fixture.collections.list_collections().await;

    assert!(matches!(listed, Err(CollectionError::CannotRead { .. })));
    assert!(!fixture.hooks.calls().contains(&LanceCall::ReadRows));
}

#[tokio::test]
async fn a_table_that_lost_its_data_files_cannot_be_read() {
    let fixture = Fixture::holding_notes().await;
    for file in files_of_the_table(&fixture, "data") {
        std::fs::remove_file(file).unwrap();
    }

    let found = fixture.collections.get_collection(&name("notes")).await;

    assert!(matches!(found, Err(CollectionError::CannotRead { .. })));
}

#[tokio::test]
async fn attributes_that_are_not_json_cannot_be_read() {
    let fixture = Fixture::new();
    fixture
        .write_table_directly(row_with_attributes("not json"))
        .await;

    let listed = fixture.collections.list_collections().await;

    assert!(matches!(listed, Err(CollectionError::CannotRead { .. })));
}

#[tokio::test]
async fn attributes_that_are_not_a_json_object_cannot_be_read() {
    let fixture = Fixture::new();
    fixture
        .write_table_directly(row_with_attributes("[1, 2]"))
        .await;

    let found = fixture.collections.get_collection(&name("notes")).await;

    assert!(matches!(found, Err(CollectionError::CannotRead { .. })));
}

#[tokio::test]
async fn a_table_without_the_expected_columns_cannot_be_read() {
    let fixture = Fixture::new();
    let schema = Schema::new(vec![Field::new("name", DataType::Utf8, false)]);
    let rows = RecordBatch::try_new(
        Arc::new(schema),
        vec![Arc::new(StringArray::from(vec!["notes"]))],
    )
    .unwrap();
    fixture.write_table_directly(rows).await;

    let listed = fixture.collections.list_collections().await;

    assert!(matches!(listed, Err(CollectionError::CannotRead { .. })));
}

#[tokio::test]
async fn a_failed_connection_is_not_kept() {
    let fixture = Fixture::holding_notes().await;
    fixture.hooks.plan(LanceCall::Connect, Plan::Fail);
    let failed = fixture.collections.list_collections().await;
    assert!(failed.is_err());

    assert_eq!(fixture.names().await, ["notes"]);

    let connects = fixture.hooks.calls().into_iter();
    let connects = connects.filter(|call| *call == LanceCall::Connect);
    assert_eq!(connects.count(), 2);
}

#[tokio::test]
async fn the_connection_is_opened_once() {
    let fixture = Fixture::holding_notes().await;

    fixture.names().await;
    fixture.names().await;

    let connects = fixture.hooks.calls().into_iter();
    let connects = connects.filter(|call| *call == LanceCall::Connect);
    assert_eq!(connects.count(), 1);
}

/// Runs `create_collection` for `text` in a task of its own, and expects it to
/// panic.
async fn create_panics(fixture: &Fixture, text: &str) {
    let collections = fixture.collections.clone();
    let info = collection(text);
    let task = tokio::spawn(async move { collections.create_collection(info).await });
    assert!(task.await.unwrap_err().is_panic());
}

#[tokio::test]
async fn a_panic_while_connecting_leaves_the_component_usable() {
    let fixture = Fixture::holding_notes().await;
    fixture.hooks.plan(LanceCall::Connect, Plan::Panic);

    create_panics(&fixture, "papers").await;

    assert_eq!(fixture.names().await, ["notes"]);
}

// The panic leaves the lock on writing released.
#[tokio::test]
async fn a_panic_while_writing_leaves_the_component_usable() {
    let fixture = Fixture::holding_notes().await;
    fixture.hooks.plan(LanceCall::AddRow, Plan::Panic);

    create_panics(&fixture, "papers").await;

    fixture.create(collection("papers")).await;
    assert_eq!(fixture.names().await, ["notes", "papers"]);
}

// A path with a byte that is not UTF-8: macOS refuses to make it, and Linux
// makes it and the connection cannot be given it. Windows paths are not bytes.
#[cfg(unix)]
#[tokio::test]
async fn a_data_directory_whose_path_is_not_unicode_cannot_be_written() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let parent = tempfile::tempdir().unwrap();
    let data_dir = parent.path().join(OsStr::from_bytes(b"data\xff"));
    let hooks = Arc::new(TestHooks::default());
    let collections = component(&data_dir, Arc::new(TestIndex::new()), &hooks);

    let result = collections.create_collection(collection("notes")).await;

    assert!(matches!(result, Err(CollectionError::CannotWrite { .. })));
}

// ------------------------------------------------------------- changing

fn new_description(text: Option<&str>) -> CollectionChange {
    CollectionChange {
        description: Some(text.map(str::to_owned)),
        attributes: None,
    }
}

fn new_attributes(value: serde_json::Value) -> CollectionChange {
    CollectionChange {
        description: None,
        attributes: Some(attributes(value)),
    }
}

#[tokio::test]
async fn a_new_description_is_kept_and_returned() {
    let fixture = Fixture::holding_notes().await;
    let expected = CollectionInfo {
        description: Some("renamed".to_owned()),
        ..collection("notes")
    };

    let changed = fixture
        .collections
        .change_collection(&name("notes"), new_description(Some("renamed")))
        .await;

    assert_eq!(changed.unwrap(), expected);
    assert_eq!(fixture.get("notes").await, Some(expected));
}

#[tokio::test]
async fn removing_the_description_leaves_none() {
    let fixture = Fixture::holding_notes().await;
    let expected = CollectionInfo {
        description: None,
        ..collection("notes")
    };

    let changed = fixture
        .collections
        .change_collection(&name("notes"), new_description(None))
        .await;

    assert_eq!(changed.unwrap(), expected);
    assert_eq!(fixture.get("notes").await, Some(expected));
}

#[tokio::test]
async fn new_attributes_replace_the_old_ones_and_keep_the_description() {
    let fixture = Fixture::holding_notes().await;
    let expected = CollectionInfo {
        attributes: attributes(json!({ "other": [1, 2] })),
        ..collection("notes")
    };

    let changed = fixture
        .collections
        .change_collection(&name("notes"), new_attributes(json!({ "other": [1, 2] })))
        .await;

    assert_eq!(changed.unwrap(), expected);
    assert_eq!(fixture.get("notes").await, Some(expected));
}

#[tokio::test]
async fn a_change_of_both_changes_both() {
    let fixture = Fixture::holding_notes().await;
    let change = CollectionChange {
        description: Some(Some("renamed".to_owned())),
        attributes: Some(attributes(json!({ "other": true }))),
    };
    let expected = CollectionInfo {
        description: Some("renamed".to_owned()),
        attributes: attributes(json!({ "other": true })),
        ..collection("notes")
    };

    let changed = fixture
        .collections
        .change_collection(&name("notes"), change)
        .await;

    assert_eq!(changed.unwrap(), expected);
    assert_eq!(fixture.get("notes").await, Some(expected));
}

#[tokio::test]
async fn a_change_leaves_the_other_collections_as_they_are() {
    let fixture = Fixture::holding_notes().await;
    fixture.create(collection("papers")).await;

    fixture
        .collections
        .change_collection(&name("notes"), new_description(Some("renamed")))
        .await
        .unwrap();

    assert_eq!(fixture.get("papers").await, Some(collection("papers")));
    assert_eq!(fixture.names().await, ["notes", "papers"]);
}

#[tokio::test]
async fn the_description_of_a_collection_type_this_mdya_does_not_know_can_be_changed() {
    let fixture = Fixture::new();
    let future = CollectionInfo {
        collection_type: CollectionType("from-a-later-version".to_owned()),
        ..collection("future")
    };
    fixture.create(future.clone()).await;
    let expected = CollectionInfo {
        description: Some("renamed".to_owned()),
        ..future
    };

    let changed = fixture
        .collections
        .change_collection(&name("future"), new_description(Some("renamed")))
        .await;

    assert_eq!(changed.unwrap(), expected);
}

#[tokio::test]
async fn changing_a_name_of_no_collection_is_not_found() {
    let fixture = Fixture::holding_notes().await;

    let result = fixture
        .collections
        .change_collection(&name("papers"), new_description(Some("renamed")))
        .await;

    assert!(matches!(result, Err(CollectionError::NotFound)));
    assert_eq!(fixture.names().await, ["notes"]);
}

#[tokio::test]
async fn changing_without_a_data_directory_is_not_found_and_makes_nothing() {
    let fixture = Fixture::new();

    let result = fixture
        .collections
        .change_collection(&name("notes"), new_description(Some("renamed")))
        .await;

    assert!(matches!(result, Err(CollectionError::NotFound)));
    assert!(!fixture.data_dir.exists());
}

#[tokio::test]
async fn changing_a_name_holding_sql_characters_changes_only_its_row() {
    let fixture = Fixture::holding_notes().await;
    let quoted = collection("x' OR '1'='1");
    fixture.add_directly(&quoted).await;

    fixture
        .collections
        .change_collection(&quoted.name, new_description(Some("renamed")))
        .await
        .unwrap();

    assert_eq!(fixture.get("notes").await, Some(collection("notes")));
    let changed = fixture.get("x' OR '1'='1").await.unwrap();
    assert_eq!(changed.description.as_deref(), Some("renamed"));
    assert_eq!(fixture.names().await.len(), 2);
}

#[tokio::test]
async fn changing_reports_each_failed_call_as_its_kind_of_failure() {
    for (call, is_read) in [
        (LanceCall::Connect, true),
        (LanceCall::OpenTable, true),
        (LanceCall::ReadRows, true),
        (LanceCall::ReplaceRow, false),
    ] {
        let fixture = Fixture::holding_notes().await;
        fixture.hooks.plan(call, Plan::Fail);

        let result = fixture
            .collections
            .change_collection(&name("notes"), new_description(Some("renamed")))
            .await;

        let error = result.unwrap_err();
        if is_read {
            assert!(
                matches!(error, CollectionError::CannotRead { .. }),
                "{call:?}"
            );
        } else {
            assert!(
                matches!(error, CollectionError::CannotWrite { .. }),
                "{call:?}"
            );
        }
        assert_eq!(cause_of(&error), planned_failure(call));
        assert_eq!(fixture.get("notes").await, Some(collection("notes")));
    }
}

// The panic leaves the lock on writing released.
#[tokio::test]
async fn a_panic_while_changing_leaves_the_component_usable() {
    let fixture = Fixture::holding_notes().await;
    fixture.hooks.plan(LanceCall::ReplaceRow, Plan::Panic);
    let collections = fixture.collections.clone();
    let task = tokio::spawn(async move {
        collections
            .change_collection(&name("notes"), new_description(Some("renamed")))
            .await
    });
    assert!(task.await.unwrap_err().is_panic());

    let changed = fixture
        .collections
        .change_collection(&name("notes"), new_description(Some("again")))
        .await;

    assert_eq!(changed.unwrap().description.as_deref(), Some("again"));
}

// One row stays, holding one of the changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_changes_leave_one_row() {
    let fixture = Fixture::holding_notes().await;
    let descriptions: Vec<String> = (0..8).map(|number| format!("change {number}")).collect();
    let mut tasks = Vec::new();
    for description in &descriptions {
        let collections = fixture.collections.clone();
        let change = new_description(Some(description));
        tasks.push(tokio::spawn(async move {
            collections.change_collection(&name("notes"), change).await
        }));
    }

    for task in tasks {
        task.await.unwrap().unwrap();
    }

    assert_eq!(fixture.names().await, ["notes"]);
    let description = fixture.get("notes").await.unwrap().description.unwrap();
    assert!(descriptions.contains(&description), "{description:?}");
}

// ------------------------------------------------------------- deleting

fn delete_others_call(text: &str) -> IndexCall {
    IndexCall::DeleteOthers {
        collection: name(text),
        keep: HashSet::new(),
        unreadable_dirs: Vec::new(),
    }
}

fn index_failure() -> PartialDelete {
    PartialDelete {
        deleted: 3,
        error: IndexError::CannotWrite {
            source: "planned failure of the index".into(),
        },
    }
}

#[tokio::test]
async fn deleting_has_the_index_delete_every_document_and_deletes_the_collection() {
    let fixture = Fixture::holding_notes().await;

    fixture
        .collections
        .delete_collection(&name("notes"))
        .await
        .unwrap();

    assert_eq!(fixture.index_calls(), [delete_others_call("notes")]);
    assert_eq!(fixture.get("notes").await, None);
    assert!(fixture.names().await.is_empty());
}

#[tokio::test]
async fn a_deleted_collection_can_be_made_again() {
    let fixture = Fixture::holding_notes().await;
    fixture
        .collections
        .delete_collection(&name("notes"))
        .await
        .unwrap();

    fixture.create(collection("notes")).await;

    assert_eq!(fixture.get("notes").await, Some(collection("notes")));
}

#[tokio::test]
async fn deleting_leaves_the_other_collections_as_they_are() {
    let fixture = Fixture::holding_notes().await;
    fixture.create(collection("papers")).await;

    fixture
        .collections
        .delete_collection(&name("notes"))
        .await
        .unwrap();

    assert_eq!(fixture.names().await, ["papers"]);
    assert_eq!(fixture.get("papers").await, Some(collection("papers")));
}

#[tokio::test]
async fn a_collection_type_this_mdya_does_not_know_can_be_deleted() {
    let fixture = Fixture::new();
    let future = CollectionInfo {
        collection_type: CollectionType("from-a-later-version".to_owned()),
        ..collection("future")
    };
    fixture.create(future).await;

    fixture
        .collections
        .delete_collection(&name("future"))
        .await
        .unwrap();

    assert_eq!(fixture.get("future").await, None);
}

// The documents go first, so a failure leaves the collection, which can be
// deleted again.
#[tokio::test]
async fn an_error_of_the_index_keeps_the_collection_and_is_returned() {
    let index = TestIndex::new().on_delete_others(|_, _, _| Err(index_failure()));
    let fixture = Fixture::holding_notes_with(index).await;

    let result = fixture.collections.delete_collection(&name("notes")).await;

    let error = result.unwrap_err();
    assert!(matches!(
        error,
        CollectionError::Index(IndexError::CannotWrite { .. })
    ));
    assert!(error.source().unwrap().is::<IndexError>());
    assert_eq!(fixture.get("notes").await, Some(collection("notes")));
}

#[tokio::test]
async fn deleting_again_after_an_error_of_the_index_goes_on() {
    let attempts = AtomicUsize::new(0);
    let index = TestIndex::new().on_delete_others(move |_, _, _| {
        match attempts.fetch_add(1, Ordering::SeqCst) {
            0 => Err(index_failure()),
            _ => Ok(1),
        }
    });
    let fixture = Fixture::holding_notes_with(index).await;
    let first = fixture.collections.delete_collection(&name("notes")).await;
    assert!(first.is_err());

    fixture
        .collections
        .delete_collection(&name("notes"))
        .await
        .unwrap();

    assert_eq!(
        fixture.index_calls(),
        [delete_others_call("notes"), delete_others_call("notes")]
    );
    assert_eq!(fixture.get("notes").await, None);
}

#[tokio::test]
async fn deleting_a_name_of_no_collection_is_not_found_and_asks_nothing_of_the_index() {
    let fixture = Fixture::holding_notes().await;

    let result = fixture.collections.delete_collection(&name("papers")).await;

    assert!(matches!(result, Err(CollectionError::NotFound)));
    assert!(fixture.index_calls().is_empty());
    assert_eq!(fixture.names().await, ["notes"]);
}

#[tokio::test]
async fn deleting_without_a_data_directory_is_not_found_and_makes_nothing() {
    let fixture = Fixture::new();

    let result = fixture.collections.delete_collection(&name("notes")).await;

    assert!(matches!(result, Err(CollectionError::NotFound)));
    assert!(fixture.index_calls().is_empty());
    assert!(!fixture.data_dir.exists());
}

#[tokio::test]
async fn deleting_a_name_holding_sql_characters_deletes_only_its_row() {
    let fixture = Fixture::holding_notes().await;
    let quoted = collection("x' OR '1'='1");
    fixture.add_directly(&quoted).await;

    fixture
        .collections
        .delete_collection(&quoted.name)
        .await
        .unwrap();

    assert_eq!(fixture.names().await, ["notes"]);
}

#[tokio::test]
async fn deleting_reports_a_failed_read_before_asking_anything_of_the_index() {
    for call in [
        LanceCall::Connect,
        LanceCall::OpenTable,
        LanceCall::ReadRows,
    ] {
        let fixture = Fixture::holding_notes().await;
        fixture.hooks.plan(call, Plan::Fail);

        let result = fixture.collections.delete_collection(&name("notes")).await;

        let error = result.unwrap_err();
        assert!(
            matches!(error, CollectionError::CannotRead { .. }),
            "{call:?}"
        );
        assert_eq!(cause_of(&error), planned_failure(call));
        assert!(fixture.index_calls().is_empty(), "{call:?}");
        assert_eq!(fixture.names().await, ["notes"], "{call:?}");
    }
}

#[tokio::test]
async fn deleting_reports_a_failed_delete_of_the_row_as_cannot_write() {
    let fixture = Fixture::holding_notes().await;
    fixture.hooks.plan(LanceCall::DeleteRow, Plan::Fail);

    let result = fixture.collections.delete_collection(&name("notes")).await;

    let error = result.unwrap_err();
    assert!(matches!(error, CollectionError::CannotWrite { .. }));
    assert_eq!(cause_of(&error), planned_failure(LanceCall::DeleteRow));
    assert_eq!(fixture.index_calls(), [delete_others_call("notes")]);
    assert_eq!(fixture.names().await, ["notes"]);
}

// The panic reaches the caller as a panic, and leaves the lock on writing
// released.
#[tokio::test]
async fn a_panic_in_the_index_reaches_the_caller_and_leaves_the_component_usable() {
    let attempts = AtomicUsize::new(0);
    let index = TestIndex::new().on_delete_others(move |_, _, _| {
        match attempts.fetch_add(1, Ordering::SeqCst) {
            0 => panic!("planned panic in the index"),
            _ => Ok(0),
        }
    });
    let fixture = Fixture::holding_notes_with(index).await;
    let collections = fixture.collections.clone();
    let task = tokio::spawn(async move { collections.delete_collection(&name("notes")).await });
    assert!(task.await.unwrap_err().is_panic());

    fixture
        .collections
        .delete_collection(&name("notes"))
        .await
        .unwrap();

    assert_eq!(fixture.get("notes").await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deletions_delete_once() {
    let fixture = Fixture::holding_notes().await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let collections = fixture.collections.clone();
        tasks.push(tokio::spawn(async move {
            collections.delete_collection(&name("notes")).await
        }));
    }

    let mut deleted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(()) => deleted += 1,
            Err(CollectionError::NotFound) => {}
            Err(other) => panic!("{other:?}"),
        }
    }

    assert_eq!(deleted, 1);
    assert_eq!(fixture.index_calls(), [delete_others_call("notes")]);
}

// ------------------------------------------------------- concurrent calls

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creations_of_one_name_make_it_once() {
    let fixture = Fixture::new();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let collections = fixture.collections.clone();
        tasks.push(tokio::spawn(async move {
            collections.create_collection(collection("notes")).await
        }));
    }

    let mut created = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(()) => created += 1,
            Err(CollectionError::AlreadyExists) => {}
            Err(other) => panic!("{other:?}"),
        }
    }

    assert_eq!(created, 1);
    assert_eq!(fixture.names().await, ["notes"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creations_of_different_names_make_them_all() {
    let fixture = Fixture::new();
    let mut tasks = Vec::new();
    for number in 0..8 {
        let collections = fixture.collections.clone();
        tasks.push(tokio::spawn(async move {
            let info = collection(&format!("c{number}"));
            collections.create_collection(info).await
        }));
    }

    for task in tasks {
        task.await.unwrap().unwrap();
    }

    assert_eq!(fixture.names().await.len(), 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_during_a_creation_succeed() {
    let fixture = Fixture::holding_notes().await;
    let collections = fixture.collections.clone();
    let creating =
        tokio::spawn(async move { collections.create_collection(collection("papers")).await });

    let mut readers = Vec::new();
    for _ in 0..8 {
        let collections = fixture.collections.clone();
        readers.push(tokio::spawn(
            async move { collections.list_collections().await },
        ));
    }

    creating.await.unwrap().unwrap();
    for reader in readers {
        reader.await.unwrap().unwrap();
    }
}
