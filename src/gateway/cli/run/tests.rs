#![expect(
    clippy::result_large_err,
    reason = "the operation handlers return the operations' error, as the trait does"
)]

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::collection::{CollectionChange, CollectionInfo};
use crate::config::{self, SettingChange};
use crate::embedding::EmbeddingError;
use crate::index::{DimensionResult, IndexError, IndexStatus, ModelStatus};
use crate::ingest::{IngestError, UpdateReport};
use crate::path::MakePathAbsoluteError;
use crate::query::{ChunkRangeRequest, SearchMode, SearchUnit};
use crate::types::{
    CollectionName, CollectionType, DocumentOutcome, EmbeddingModel, Key, TypeAttributes,
};

use super::super::super::error_report::test_double::{ReportCall, TestErrorReport};
use super::super::super::error_report::{Explanation, GatewayError};
use super::super::super::formatter::{self, UnsafeNumber};
use super::super::super::log_progress::ProgressKind;
use super::super::super::log_progress::test_double::{ConsoleEvent, DisplayEvent, TestConsole};
use super::super::super::operations::test_double::TestOperations;
use super::super::super::operations::{
    CollectionEntry, CollectionList, DeleteCollectionOutcome, DeleteDocumentOutcome, DocumentText,
    ErrorContext, ModelCacheDirUndetermined, OperationError, PutOutcome, RebuildOutcome,
    SafeInteger, SafeJsonObject, SafeJsonValue, SearchLimit, SearchModeName, SearchOutput,
    SearchUnitName, SettingChangeOutcome, SettingsOutput, StatusOutput, UpdateOutcome, version,
};
use super::super::{
    CliError, CliHelp, CliOperation, CliOutput, CliStreams, ContentSource, Operation, Output,
    Terminals, check_confirmation, report_error, report_listening, report_panic, run_operation,
    show_help, show_version,
};
use super::made;

/// Keeps what is written to it, for the test to read.
#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl Written {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Written {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Fails every write with `kind`.
struct Failing(io::ErrorKind);

impl Write for Failing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What a run wrote and received.
struct Ran {
    code: u8,
    stdout: String,
    console: Arc<TestConsole>,
    report: Arc<TestErrorReport>,
    operations: Arc<TestOperations>,
}

impl Ran {
    fn stderr(&self) -> String {
        self.console.stderr()
    }
}

fn output(console: &Arc<TestConsole>, report: &Arc<TestErrorReport>) -> CliOutput {
    CliOutput {
        console: console.clone(),
        error_report: report.clone(),
        stderr_color: false,
    }
}

async fn run_into(
    operation: Operation,
    operations: TestOperations,
    stdin: &'static str,
    stdout: Box<dyn Write + Send>,
) -> (
    u8,
    Arc<TestConsole>,
    Arc<TestErrorReport>,
    Arc<TestOperations>,
) {
    let console = Arc::new(TestConsole::new());
    let report = Arc::new(TestErrorReport::new());
    let operations = Arc::new(operations);
    let code = run_operation(
        CliOperation(operation),
        operations.clone(),
        Some(1024),
        CliStreams {
            stdin: Box::new(stdin.as_bytes()),
            stdout,
            terminals: Terminals {
                stdin: true,
                stdout: true,
                stderr: true,
            },
        },
        &output(&console, &report),
    )
    .await;
    (code, console, report, operations)
}

async fn run_with_stdin(
    operation: Operation,
    operations: TestOperations,
    stdin: &'static str,
) -> Ran {
    let stdout = Written::default();
    let (code, console, report, operations) =
        run_into(operation, operations, stdin, Box::new(stdout.clone())).await;
    Ran {
        code,
        stdout: stdout.text(),
        console,
        report,
        operations,
    }
}

async fn run(operation: Operation, operations: TestOperations) -> Ran {
    run_with_stdin(operation, operations, "").await
}

fn name(name: &str) -> CollectionName {
    CollectionName(name.to_owned())
}

fn key(key: &str) -> Key {
    Key(key.to_owned())
}

fn info(description: Option<&str>) -> CollectionInfo {
    let mut attributes = serde_json::Map::new();
    attributes.insert("path".to_owned(), json!("/Users/me/notes"));
    CollectionInfo {
        name: name("notes"),
        collection_type: CollectionType("directory".to_owned()),
        attributes: TypeAttributes(attributes),
        description: description.map(str::to_owned),
    }
}

fn not_found() -> OperationError {
    OperationError::CollectionNotFound {
        collection: name("notes"),
    }
}

/// The explanation that the error report double gives for `error`, as the
/// command line writes it.
fn explained(error: &GatewayError) -> String {
    let explanation = TestErrorReport::explanation_of(error);
    format!("{}\n", formatter::human_explanation(&explanation, false))
}

#[tokio::test]
async fn a_created_collection_is_told_in_a_message() {
    let ran = run(
        Operation::CreateCollection {
            name: name("notes"),
            collection_type: CollectionType("directory".to_owned()),
            description: Some("my notes".to_owned()),
            attributes: TypeAttributes::default(),
        },
        TestOperations::new().on_create_collection(|_| Ok(info(Some("my notes")))),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.stderr(),
        "Created the collection \"notes\" (directory).\n  path: /Users/me/notes\n  description: my notes\n"
    );
    assert_eq!(ran.stdout, "");
    let received = ran.operations.received();
    assert_eq!(
        received.create_collection[0].description.as_deref(),
        Some("my notes")
    );
}

fn delete_collection(yes: bool) -> Operation {
    Operation::DeleteCollection {
        name: name("notes"),
        yes,
        optimize: true,
    }
}

/// Deletes the collection when the confirmation answers yes.
fn deleting_collection() -> TestOperations {
    TestOperations::new().on_delete_collection_async(|input| {
        Box::pin(async move {
            Ok(if input.confirmation.confirm().await {
                DeleteCollectionOutcome::Deleted {
                    collection: input.name.clone(),
                }
            } else {
                DeleteCollectionOutcome::Declined
            })
        })
    })
}

#[tokio::test]
async fn removing_a_collection_asks_and_removes_it_on_yes() {
    for answer in ["y\n", "Y\n", "yes\n", " YES \n"] {
        let ran = run_with_stdin(delete_collection(false), deleting_collection(), answer).await;

        assert_eq!(ran.code, 0, "{answer:?}");
        assert_eq!(
            ran.stderr(),
            "Remove the collection \"notes\" and its documents from the index? [y/N] Removed the collection \"notes\".\n",
            "{answer:?}"
        );
    }
}

#[tokio::test]
async fn removing_a_collection_is_declined_on_anything_but_yes() {
    for answer in ["n\n", "\n", "", "yess\n"] {
        let ran = run_with_stdin(delete_collection(false), deleting_collection(), answer).await;

        assert_eq!(ran.code, 1, "{answer:?}");
        assert!(
            ran.stderr()
                .ends_with("[y/N] Did not remove the collection \"notes\".\n"),
            "{answer:?}: {}",
            ran.stderr()
        );
    }
}

#[tokio::test]
async fn removing_a_collection_with_yes_does_not_ask() {
    let ran = run(delete_collection(true), deleting_collection()).await;

    assert_eq!(ran.code, 0);
    assert_eq!(ran.stderr(), "Removed the collection \"notes\".\n");
    assert!(ran.operations.received().delete_collection[0].optimize);
}

fn collection_list() -> CollectionList {
    CollectionList {
        collections: vec![CollectionEntry {
            name: "memo".to_owned(),
            collection_type: "raw".to_owned(),
            description: None,
            document_count: SafeInteger::new(3),
            attributes: SafeJsonObject::new(serde_json::Map::new()),
        }],
    }
}

#[tokio::test]
async fn the_collections_are_written_to_stdout_in_the_output_format_asked() {
    let human = run(
        Operation::ListCollections {
            output: Output::Human,
        },
        TestOperations::new().on_list_collections(|_| Ok(collection_list())),
    )
    .await;
    let json = run(
        Operation::ListCollections {
            output: Output::Json,
        },
        TestOperations::new().on_list_collections(|_| Ok(collection_list())),
    )
    .await;

    assert_eq!(human.code, 0);
    assert_eq!(human.stdout, "memo (raw, 3 documents)\n");
    assert_eq!(
        json.stdout,
        format!("{}\n", formatter::cli_json(&collection_list()).unwrap())
    );
    assert_eq!(human.stderr(), "");
}

#[tokio::test]
async fn a_changed_collection_is_told_in_a_message() {
    let ran = run(
        Operation::ChangeCollection {
            name: name("notes"),
            change: CollectionChange {
                description: Some(None),
                attributes: None,
            },
        },
        TestOperations::new().on_change_collection(|_| Ok(info(None))),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.stderr(),
        "Changed the collection \"notes\".\n  path: /Users/me/notes\n  description: (none)\n"
    );
}

fn update_outcome(failed: u64, updated_collections: usize) -> UpdateOutcome {
    UpdateOutcome {
        counts: UpdateReport {
            added: 3,
            failed,
            ..UpdateReport::default()
        },
        updated_collections,
        unreadable_sources: Vec::new(),
        status_error: None,
    }
}

#[tokio::test]
async fn an_update_shows_its_progress_then_the_counts() {
    let ran = run(
        Operation::Update {
            collection: name("notes"),
            overwrite: true,
            optimize: false,
        },
        TestOperations::new().on_update_collection(|input| {
            input.progress.maintenance_started();
            Ok(update_outcome(0, 1))
        }),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.console.events(),
        [
            ConsoleEvent::ProgressStarted(ProgressKind::Update),
            ConsoleEvent::Display {
                index: 0,
                event: DisplayEvent::MaintenanceStarted
            },
            ConsoleEvent::Display {
                index: 0,
                event: DisplayEvent::Finished
            },
            ConsoleEvent::Stderr(
                "Updated the collection \"notes\": 3 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n"
                    .to_owned()
            ),
        ]
    );
    let received = ran.operations.received();
    assert!(received.update_collection[0].overwrite);
    assert!(!received.update_collection[0].optimize);
}

#[tokio::test]
async fn an_update_with_failed_documents_exits_with_the_code_for_its_anomalies() {
    let ran = run(
        Operation::UpdateAll {
            overwrite: false,
            optimize: true,
        },
        TestOperations::new().on_update_all(|_| Ok(update_outcome(1, 2))),
    )
    .await;

    assert_eq!(ran.code, 1);
    assert!(ran.stderr().ends_with(
        "Updated 2 collections: 3 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 1 failed.\n"
    ));
    assert!(ran.report.calls().contains(&ReportCall::OutcomeExitCode(
        update_outcome(1, 2).anomalies()
    )));
}

#[tokio::test]
async fn an_update_that_stopped_is_explained_then_gives_the_counts_so_far() {
    let stopped = || OperationError::UpdateStopped {
        counts: UpdateReport {
            added: 2,
            ..UpdateReport::default()
        },
        context: ErrorContext {
            collection: Some(name("notes")),
            key: None,
        },
        source: IngestError::AttributesMissingOrExtra,
    };
    let ran = run(
        Operation::Update {
            collection: name("notes"),
            overwrite: false,
            optimize: true,
        },
        TestOperations::new().on_update_collection(move |_| Err(stopped())),
    )
    .await;

    assert_eq!(ran.code, 1);
    let error = GatewayError::Operation(stopped());
    assert_eq!(
        ran.stderr(),
        format!(
            "{}Before it stopped: 2 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.\n",
            explained(&error)
        )
    );
    assert_eq!(
        ran.console.events()[1],
        ConsoleEvent::Display {
            index: 0,
            event: DisplayEvent::Finished
        }
    );
}

fn put(content: ContentSource) -> Operation {
    Operation::Put {
        collection: name("notes"),
        key: key("memo.md"),
        media_type: "md".to_owned(),
        content,
        overwrite: false,
        optimize: true,
    }
}

/// Puts the document after reading its content, which it keeps.
fn putting(read: Arc<Mutex<Vec<u8>>>) -> TestOperations {
    TestOperations::new().on_put_document_async(move |input| {
        let read = read.clone();
        Box::pin(async move {
            *read.lock().unwrap() = input.content.read().await.unwrap();
            Ok(PutOutcome {
                collection: input.collection.clone(),
                key: input.key.clone(),
                outcome: DocumentOutcome::Changed,
                status_error: None,
            })
        })
    })
}

#[tokio::test]
async fn put_reads_the_content_from_stdin() {
    let read = Arc::new(Mutex::new(Vec::new()));

    let ran = run_with_stdin(put(ContentSource::Stdin), putting(read.clone()), "# Memo\n").await;

    assert_eq!(ran.code, 0);
    assert_eq!(*read.lock().unwrap(), b"# Memo\n");
    assert_eq!(
        ran.stderr(),
        "Changed \"memo.md\" in the collection \"notes\".\n"
    );
}

#[tokio::test]
async fn put_reads_the_content_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memo.md");
    std::fs::write(&path, "from a file").unwrap();
    let read = Arc::new(Mutex::new(Vec::new()));

    let ran = run(put(ContentSource::File(path)), putting(read.clone())).await;

    assert_eq!(ran.code, 0);
    assert_eq!(*read.lock().unwrap(), b"from a file");
}

#[tokio::test]
async fn a_file_that_cannot_be_read_is_an_error_of_the_reader() {
    let read = Arc::new(Mutex::new(Vec::new()));
    let operations = TestOperations::new().on_put_document_async(move |input| {
        let read = read.clone();
        Box::pin(async move {
            let error = input.content.read().await.unwrap_err();
            *read.lock().unwrap() = error.to_string().into_bytes();
            Err(OperationError::ContentUnreadable { source: error })
        })
    });

    let ran = run(
        put(ContentSource::File(PathBuf::from("/no/such/file"))),
        operations,
    )
    .await;

    assert_eq!(ran.code, 1);
    assert!(
        ran.stderr().contains("ContentUnreadable"),
        "{}",
        ran.stderr()
    );
}

fn delete_document(yes: bool) -> Operation {
    Operation::DeleteDocument {
        collection: name("notes"),
        key: key("memo.md"),
        yes,
        optimize: true,
    }
}

fn deleting_document(supports_update: bool) -> TestOperations {
    TestOperations::new().on_delete_document_async(move |input| {
        Box::pin(async move {
            Ok(if input.confirmation.confirm().await {
                DeleteDocumentOutcome::Deleted {
                    collection: input.collection.clone(),
                    key: input.key.clone(),
                    supports_update,
                }
            } else {
                DeleteDocumentOutcome::Declined
            })
        })
    })
}

#[tokio::test]
async fn removing_a_document_asks_and_tells_whether_it_may_come_back() {
    let ran = run_with_stdin(delete_document(false), deleting_document(true), "y\n").await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.stderr(),
        "Remove \"memo.md\" from the collection \"notes\"? [y/N] Removed \"memo.md\" from the collection \"notes\".\nIf it is still in the collection's source, it comes back at the next update.\n"
    );
}

#[tokio::test]
async fn removing_a_document_declined_or_without_asking() {
    let declined = run_with_stdin(delete_document(false), deleting_document(false), "no\n").await;
    let yes = run(delete_document(true), deleting_document(false)).await;

    assert_eq!(declined.code, 1);
    assert!(
        declined
            .stderr()
            .ends_with("Did not remove \"memo.md\" from the collection \"notes\".\n")
    );
    assert_eq!(yes.code, 0);
    assert_eq!(
        yes.stderr(),
        "Removed \"memo.md\" from the collection \"notes\".\n"
    );
}

fn search_output() -> SearchOutput {
    SearchOutput {
        query: "q".to_owned(),
        mode: SearchModeName::Hybrid,
        unit: SearchUnitName::Document,
        collections: Vec::new(),
        limit: SearchLimit::new(20),
        hit_count: SafeInteger::new(0),
        hits: Vec::new(),
        warnings: vec!["a warning".to_owned()],
    }
}

fn search(output: Output) -> Operation {
    Operation::Search {
        query: "q".to_owned(),
        mode: SearchMode::FullText,
        collections: vec![name("notes")],
        limit: SearchLimit::new(5),
        unit: SearchUnit::Chunk,
        output,
    }
}

#[tokio::test]
async fn a_search_passes_its_input_and_writes_the_result() {
    let human = run(
        search(Output::Human),
        TestOperations::new().on_search(|_| Ok(search_output())),
    )
    .await;
    let json = run(
        search(Output::Json),
        TestOperations::new().on_search(|_| Ok(search_output())),
    )
    .await;

    assert_eq!(human.stdout, "(no hits)\n");
    assert_eq!(
        json.stdout,
        format!("{}\n", formatter::cli_json(&search_output()).unwrap())
    );
    let received = human.operations.received();
    let input = &received.search[0];
    assert_eq!(input.mode, SearchMode::FullText);
    assert_eq!(input.collections, [name("notes")]);
    assert_eq!(input.limit, SearchLimit::new(5));
    assert_eq!(input.unit, SearchUnit::Chunk);
}

fn get(size_limit_off: bool) -> Operation {
    Operation::Get {
        collection: name("notes"),
        key: key("a.md"),
        range: ChunkRangeRequest {
            start: Some(1),
            end: None,
        },
        size_limit_off,
    }
}

fn document() -> DocumentText {
    DocumentText {
        collection: "notes".to_owned(),
        key: "a.md".to_owned(),
        text: "a\u{1b}b\u{202e}c".to_owned(),
    }
}

#[tokio::test]
async fn a_document_is_written_as_it_is_without_a_line_break() {
    let ran = run(
        get(false),
        TestOperations::new().on_get_document(|_| Ok(document())),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(ran.stdout, "a\u{1b}b\u{202e}c");
    let received = ran.operations.received();
    assert_eq!(received.get_document[0].size_limit, Some(1024));
    assert_eq!(received.get_document[0].range.start, Some(1));
}

#[tokio::test]
async fn no_size_limit_applies_none() {
    let ran = run(
        get(true),
        TestOperations::new().on_get_document(|_| Ok(document())),
    )
    .await;

    assert_eq!(ran.operations.received().get_document[0].size_limit, None);
}

fn setting_outcome(index_status: Option<Result<IndexStatus, IndexError>>) -> SettingChangeOutcome {
    SettingChangeOutcome {
        change: SettingChange {
            name: config::check_setting("embedding.model", None).unwrap(),
            before: json!("old"),
            after: json!("new"),
        },
        index_status,
    }
}

fn index_status(
    same_as_compared: Option<bool>,
    compared_dimension: DimensionResult,
) -> IndexStatus {
    IndexStatus {
        compared_model_name: "new".to_owned(),
        compared_dimension,
        models: vec![ModelStatus {
            model: EmbeddingModel {
                name: "old".to_owned(),
                dimension: 256,
            },
            same_as_compared,
            chunk_count: 10,
            truncated_chunk_count: 0,
        }],
        collections: BTreeMap::new(),
        document_count: 1,
        chunk_count: 10,
        chunks_without_vector: 0,
    }
}

async fn change_setting(outcome: fn() -> SettingChangeOutcome) -> Ran {
    run(
        Operation::SetSetting {
            name: "embedding.model".to_owned(),
            value: "new".to_owned(),
        },
        TestOperations::new().on_change_setting(move |_| Ok(outcome())),
    )
    .await
}

#[tokio::test]
async fn a_changed_setting_gives_both_values() {
    let ran = change_setting(|| setting_outcome(None)).await;

    assert_eq!(ran.code, 0);
    assert_eq!(ran.stderr(), "Changed embedding.model from old to new.\n");
}

#[tokio::test]
async fn a_new_embedding_model_that_the_index_was_not_made_with_is_told() {
    let ran = change_setting(|| {
        setting_outcome(Some(Ok(index_status(
            Some(false),
            DimensionResult::NotChecked,
        ))))
    })
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.stderr(),
        "Changed embedding.model from old to new.\nThe index's embeddings were made with another model (old).\nUntil you run mdya embedding rebuild, vector search finds nothing, and update and put fail.\n"
    );
}

#[tokio::test]
async fn a_new_embedding_model_that_the_index_was_made_with_needs_nothing_more() {
    let ran = change_setting(|| {
        setting_outcome(Some(Ok(index_status(
            Some(true),
            DimensionResult::Known(256),
        ))))
    })
    .await;

    assert_eq!(ran.stderr(), "Changed embedding.model from old to new.\n");
}

// The names are the same, and the dimension was not to be checked: nothing
// is known to differ, and nothing more is told.
#[tokio::test]
async fn a_same_named_model_whose_dimension_was_not_checked_needs_nothing_more() {
    let ran = change_setting(|| {
        setting_outcome(Some(Ok(index_status(None, DimensionResult::NotChecked))))
    })
    .await;

    assert_eq!(ran.stderr(), "Changed embedding.model from old to new.\n");
}

#[tokio::test]
async fn an_index_status_that_cannot_be_read_is_told_with_its_cause() {
    let ran = change_setting(|| setting_outcome(Some(Err(IndexError::RebuildIncomplete)))).await;

    assert_eq!(ran.code, 1);
    assert_eq!(
        ran.stderr(),
        format!(
            "Changed embedding.model from old to new.\nWhether the index's embeddings were made with the new model is not known: the index status could not be read.\n  cause: {}\n",
            IndexError::RebuildIncomplete
        )
    );
}

#[tokio::test]
async fn a_dimension_that_cannot_be_checked_is_told_with_the_fix_for_the_cache_directory() {
    let ran = change_setting(|| {
        setting_outcome(Some(Ok(index_status(
            None,
            DimensionResult::Unavailable(EmbeddingError::ModelUnavailable {
                source: Box::new(ModelCacheDirUndetermined {
                    source: MakePathAbsoluteError::HomeDirUnknown,
                }),
            }),
        ))))
    })
    .await;

    assert_eq!(ran.code, 0);
    let stderr = ran.stderr();
    assert!(stderr.contains("not known: its dimension could not be checked.\n  cause: the embedding model cannot be used: the embedding model cache directory cannot be determined: the home directory is unknown"), "{stderr}");
    assert!(
        stderr.ends_with("  For the MCP server, start it again after either of these.\n"),
        "{stderr}"
    );
}

#[tokio::test]
async fn a_dimension_that_cannot_be_checked_for_another_cause_has_no_fix() {
    let ran = change_setting(|| {
        setting_outcome(Some(Ok(index_status(
            None,
            DimensionResult::Unavailable(EmbeddingError::ModelUnavailable {
                source: "no network".into(),
            }),
        ))))
    })
    .await;

    assert!(
        ran.stderr()
            .ends_with("  cause: the embedding model cannot be used: no network\n"),
        "{}",
        ran.stderr()
    );
}

#[tokio::test]
async fn reset_changes_the_setting_back() {
    let ran = run(
        Operation::ResetSetting {
            name: "embedding.model".to_owned(),
        },
        TestOperations::new().on_change_setting(|_| Ok(setting_outcome(None))),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert!(matches!(
        &ran.operations.received().change_setting[0],
        super::super::super::operations::ChangeSettingInput::Reset { name } if name == "embedding.model"
    ));
}

fn rebuilt(chunks: u64) -> RebuildOutcome {
    RebuildOutcome {
        rebuilt_chunks: chunks,
        status_error: None,
    }
}

async fn rebuild(force: bool, chunks: u64) -> Ran {
    run(
        Operation::RebuildEmbeddings {
            force,
            optimize: true,
        },
        TestOperations::new().on_rebuild_embeddings(move |_| Ok(rebuilt(chunks))),
    )
    .await
}

#[tokio::test]
async fn a_rebuild_tells_how_many_chunks_or_that_it_rebuilt_none() {
    assert_eq!(
        rebuild(false, 1534).await.stderr(),
        "Rebuilt the embeddings of 1534 chunks.\n"
    );
    assert_eq!(
        rebuild(false, 0).await.stderr(),
        "Nothing was rebuilt: the embeddings were already made with the configured model.\n"
    );
    assert_eq!(
        rebuild(true, 0).await.stderr(),
        "Rebuilt the embeddings of 0 chunks.\n"
    );
    let ran = rebuild(true, 3).await;
    assert_eq!(
        ran.console.events()[0],
        ConsoleEvent::ProgressStarted(ProgressKind::RebuildEmbeddings)
    );
    assert!(ran.operations.received().rebuild_embeddings[0].include_configured_model);
}

#[tokio::test]
async fn maintaining_the_index_shows_its_progress_then_tells_it() {
    let ran = run(
        Operation::Optimize,
        TestOperations::new().on_optimize(|_| Ok(())),
    )
    .await;

    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.console.events(),
        [
            ConsoleEvent::ProgressStarted(ProgressKind::Optimize),
            ConsoleEvent::Display {
                index: 0,
                event: DisplayEvent::Finished
            },
            ConsoleEvent::Stderr("Maintained the index.\n".to_owned()),
        ]
    );
}

#[tokio::test]
async fn an_operation_error_is_explained_and_its_exit_code_returned() {
    let ran = run(
        Operation::ListCollections {
            output: Output::Human,
        },
        TestOperations::new().on_list_collections(|_| Err(not_found())),
    )
    .await;

    let error = GatewayError::Operation(not_found());
    assert_eq!(ran.code, 1);
    assert_eq!(ran.stderr(), explained(&error));
    assert_eq!(ran.stdout, "");
    assert_eq!(
        ran.report.calls(),
        [
            ReportCall::Explanation(format!("{error:?}")),
            ReportCall::ExitCode(format!("{error:?}"))
        ]
    );
}

#[tokio::test]
async fn a_stdout_that_was_closed_stops_quietly_with_0() {
    let (code, console, report, _) = run_into(
        Operation::ListCollections {
            output: Output::Human,
        },
        TestOperations::new().on_list_collections(|_| Ok(collection_list())),
        "",
        Box::new(Failing(io::ErrorKind::BrokenPipe)),
    )
    .await;

    assert_eq!(code, 0);
    assert_eq!(console.stderr(), "");
    assert_eq!(report.calls(), []);
}

#[tokio::test]
async fn a_failed_write_of_the_result_is_explained_with_exit_code_1() {
    let (code, console, report, _) = run_into(
        Operation::ListCollections {
            output: Output::Json,
        },
        TestOperations::new().on_list_collections(|_| Ok(collection_list())),
        "",
        Box::new(Failing(io::ErrorKind::StorageFull)),
    )
    .await;

    assert_eq!(code, 1);
    let calls = report.calls();
    assert!(
        matches!(&calls[0], ReportCall::Explanation(error) if error.contains("StdoutUnwritable")),
        "{calls:?}"
    );
    assert!(console.stderr().contains("StdoutUnwritable"));
}

#[test]
fn a_removal_without_yes_needs_a_terminal_on_stdin() {
    let terminals = |stdin| Terminals {
        stdin,
        stdout: true,
        stderr: true,
    };
    let operation = |op| CliOperation(op);

    assert!(matches!(
        check_confirmation(&operation(delete_collection(false)), terminals(false)),
        Err(CliError::ConfirmationUnavailable)
    ));
    assert!(matches!(
        check_confirmation(&operation(delete_document(false)), terminals(false)),
        Err(CliError::ConfirmationUnavailable)
    ));
    assert!(check_confirmation(&operation(delete_collection(false)), terminals(true)).is_ok());
    assert!(check_confirmation(&operation(delete_document(true)), terminals(false)).is_ok());
    assert!(check_confirmation(&operation(Operation::Optimize), terminals(false)).is_ok());
}

fn plain_output() -> (Arc<TestConsole>, Arc<TestErrorReport>) {
    (
        Arc::new(TestConsole::new()),
        Arc::new(TestErrorReport::new()),
    )
}

#[test]
fn the_help_and_the_version_are_written_to_stdout() {
    let (console, report) = plain_output();
    let help_out = Written::default();
    let version_out = Written::default();

    let help = show_help(
        CliHelp("Usage: mdya\n".to_owned()),
        Box::new(help_out.clone()),
        &output(&console, &report),
    );
    let shown = show_version(Box::new(version_out.clone()), &output(&console, &report));

    assert_eq!((help, shown), (0, 0));
    assert_eq!(help_out.text(), "Usage: mdya\n");
    assert_eq!(version_out.text(), format!("{}\n", version().version));
    assert_eq!(console.stderr(), "");
}

#[test]
fn the_help_to_a_closed_stdout_stops_quietly() {
    let (console, report) = plain_output();

    let code = show_help(
        CliHelp("Usage: mdya\n".to_owned()),
        Box::new(Failing(io::ErrorKind::BrokenPipe)),
        &output(&console, &report),
    );

    assert_eq!(code, 0);
    assert_eq!(console.stderr(), "");
}

#[test]
fn an_error_found_outside_an_operation_is_explained() {
    let (console, report) = plain_output();
    let error = GatewayError::Cli(CliError::NothingToChange);

    let code = report_error(&error, &output(&console, &report));

    assert_eq!(code, 1);
    assert_eq!(console.stderr(), explained(&error));
}

#[test]
fn an_unexpected_failure_is_explained_with_101() {
    let (console, report) = plain_output();

    let code = report_panic(&output(&console, &report));

    assert_eq!(code, 101);
    let explanation: Explanation = TestErrorReport::panic_explanation_of();
    assert_eq!(
        console.stderr(),
        format!("{}\n", formatter::human_explanation(&explanation, false))
    );
}

#[test]
fn listening_is_told_for_each_address() {
    let (console, report) = plain_output();
    let addrs: Vec<SocketAddr> = vec![
        "127.0.0.1:8000".parse().unwrap(),
        "[::1]:8000".parse().unwrap(),
    ];

    report_listening(&addrs, &output(&console, &report));

    assert_eq!(
        console.stderr(),
        "Listening on http://127.0.0.1:8000/mcp\nListening on http://[::1]:8000/mcp\n"
    );
}

fn status_output() -> StatusOutput {
    StatusOutput {
        version: "1.0.0".to_owned(),
        configured_embedding_model: "m".to_owned(),
        configured_embedding_dimension: None,
        index_embedding_models: Vec::new(),
        chunks_without_embedding_count: SafeInteger::new(0),
        collection_count: SafeInteger::new(0),
        document_count: SafeInteger::new(0),
        chunk_count: SafeInteger::new(0),
        warnings: Vec::new(),
    }
}

#[tokio::test]
async fn the_status_is_written_in_the_output_format_asked() {
    let human = run(
        Operation::Status {
            output: Output::Human,
        },
        TestOperations::new().on_status(|_| Ok(status_output())),
    )
    .await;
    let json = run(
        Operation::Status {
            output: Output::Json,
        },
        TestOperations::new().on_status(|_| Ok(status_output())),
    )
    .await;

    assert_eq!(human.stdout, formatter::human_status(&status_output()));
    assert_eq!(
        json.stdout,
        format!("{}\n", formatter::cli_json(&status_output()).unwrap())
    );
}

fn settings_output() -> SettingsOutput {
    SettingsOutput {
        entries: vec![(
            config::check_setting("mcp.show_version", None).unwrap(),
            SafeJsonValue::new(json!(true)),
        )],
    }
}

#[tokio::test]
async fn the_settings_are_written_in_the_output_format_asked() {
    let human = run(
        Operation::ListSettings {
            output: Output::Human,
        },
        TestOperations::new().on_list_settings(|_| Ok(settings_output())),
    )
    .await;
    let json = run(
        Operation::ListSettings {
            output: Output::Json,
        },
        TestOperations::new().on_list_settings(|_| Ok(settings_output())),
    )
    .await;

    assert_eq!(human.stdout, "mcp.show_version = true\n");
    assert_eq!(json.stdout, "{\"mcp.show_version\":true}\n");
}

#[test]
#[should_panic(expected = "the number NaN is not finite")]
fn a_number_that_the_formatter_refuses_is_an_unexpected_failure() {
    made::<()>(Err(UnsafeNumber::NotFinite("NaN".to_owned())));
}
