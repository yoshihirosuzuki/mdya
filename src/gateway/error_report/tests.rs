use std::io;
use std::sync::Arc;

use serde_json::json;

use crate::collection::CollectionError;
use crate::config::{ConfigError, SettingName};
use crate::embedding::EmbeddingError;
use crate::format::FormatError;
use crate::index::IndexError;
use crate::ingest::IngestError;
use crate::path::MakePathAbsoluteError;
use crate::query::QueryError;
use crate::types::{CollectionName, CollectionType, EmbeddingModel, Key};

use super::super::cli::CliError;
use super::super::entry::EntryError;
use super::super::log_progress::InvalidLogLevel;
use super::super::mcp::McpError;
use super::super::operations::{
    ErrorContext, ModelCacheDirUndetermined, OperationError, OutcomeAnomaly, OutputPart,
};
use super::super::write_lock::WriteLockError;
use super::{ErrorReport, Explanation, GatewayError, McpErrorCode, error_report};

fn report() -> Arc<dyn ErrorReport> {
    error_report()
}

fn name(name: &str) -> CollectionName {
    CollectionName(name.to_owned())
}

fn key(key: &str) -> Key {
    Key(key.to_owned())
}

fn in_collection(collection: &str) -> ErrorContext {
    ErrorContext {
        collection: Some(name(collection)),
        key: None,
    }
}

fn of_document(collection: &str, document: &str) -> ErrorContext {
    ErrorContext {
        collection: Some(name(collection)),
        key: Some(key(document)),
    }
}

fn operation(error: OperationError) -> GatewayError {
    GatewayError::Operation(error)
}

/// The explanation, written as one string in the layout of the command line's
/// error explanation, without color, so that a test reads as the user sees
/// it. Laying out is not this submodule's job: this only makes the tests easy
/// to read.
fn written(explanation: &Explanation) -> String {
    let mut text = format!("Error: {}", explanation.message);
    if !explanation.causes.is_empty() {
        text.push_str("\n\nCaused by:");
        for (number, cause) in explanation.causes.iter().enumerate() {
            text.push_str(&format!("\n    {number}: {cause}"));
        }
    }
    text.push_str("\n\nHow to fix:");
    for line in &explanation.fix {
        text.push_str(&format!("\n    {line}"));
    }
    text
}

fn explained(error: &GatewayError) -> String {
    written(&report().explanation(error))
}

fn mcp(error: &GatewayError) -> McpErrorCode {
    report().mcp_error(error)
}

fn details(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

/// Checks the exit code, the explanation (written as in [`written`]), and the
/// MCP error code and details of `error`.
fn assert_shown(
    error: GatewayError,
    exit_code: u8,
    explanation: &str,
    code: &str,
    expected_details: serde_json::Value,
) {
    let report = report();
    assert_eq!(report.exit_code(&error), exit_code, "exit code");
    assert_eq!(
        written(&report.explanation(&error)),
        explanation,
        "explanation"
    );
    let mcp = report.mcp_error(&error);
    assert_eq!(mcp.code, code, "MCP code");
    assert_eq!(mcp.details, details(expected_details), "MCP details");
}

/// Checks the exit code and the MCP error code of `error`.
fn assert_codes(error: &GatewayError, exit_code: u8, code: &str) {
    assert_eq!(report().exit_code(error), exit_code, "exit code");
    assert_eq!(mcp(error).code, code, "MCP code");
}

/// A cause made in the test, with a cause of its own or none, so that the
/// tests of the form do not depend on the texts of other errors.
#[derive(Debug, thiserror::Error)]
#[error("{text}")]
struct TestCause {
    text: &'static str,
    #[source]
    source: Option<Box<TestCause>>,
}

fn two_causes() -> TestCause {
    TestCause {
        text: "the nearest cause",
        source: Some(Box::new(TestCause {
            text: "its cause",
            source: None,
        })),
    }
}

// What the explanation holds.

#[test]
fn an_error_without_a_cause_has_the_message_no_causes_and_the_fix() {
    let error = operation(OperationError::WriteLock(WriteLockError::InProgress));

    assert_eq!(
        report().explanation(&error),
        Explanation {
            message: "another write operation is in progress".to_owned(),
            causes: Vec::new(),
            fix: vec![
                "Wait until the other write operation ends, then run the operation again."
                    .to_owned(),
                "A write operation that stopped partway (when interrupted, for example) does not keep the next one from starting."
                    .to_owned(),
            ],
        }
    );
}

#[test]
fn an_error_with_causes_holds_them_from_the_nearest() {
    let error = operation(OperationError::Index {
        context: ErrorContext::default(),
        source: IndexError::CannotWrite {
            source: Box::new(two_causes()),
        },
    });

    let explanation = report().explanation(&error);

    assert_eq!(explanation.message, "the index cannot be written");
    assert_eq!(explanation.causes, ["the nearest cause", "its cause"]);
    assert_codes(&error, 1, "internal");
}

#[test]
fn the_values_and_the_causes_are_kept_as_they_are() {
    let error = operation(OperationError::Index {
        context: in_collection("no\ntes"),
        source: IndexError::CannotWrite {
            source: "disk\u{1b}full".into(),
        },
    });
    let error_with_a_name = operation(OperationError::CollectionNotFound {
        collection: name("no\u{202e}tes"),
    });

    let explanation = report().explanation(&error);
    let with_a_name = report().explanation(&error_with_a_name);

    assert_eq!(explanation.causes, ["disk\u{1b}full"]);
    assert_eq!(
        with_a_name.message,
        "the collection \"no\u{202e}tes\" does not exist"
    );
}

// Exit codes of operations that ended without an error.

#[test]
fn an_outcome_without_anomalies_exits_with_0() {
    assert_eq!(report().outcome_exit_code(&[]), 0);
}

#[test]
fn an_outcome_with_any_anomaly_exits_with_1() {
    for anomaly in [
        OutcomeAnomaly::Declined,
        OutcomeAnomaly::DocumentsFailed,
        OutcomeAnomaly::SourcesUnreadable,
        OutcomeAnomaly::StatusUnreadable,
    ] {
        assert_eq!(report().outcome_exit_code(&[anomaly]), 1, "{anomaly:?}");
    }
}

// Confirmation unavailable

#[test]
fn a_confirmation_without_a_terminal_or_yes_is_a_usage_error() {
    assert_shown(
        GatewayError::Cli(CliError::ConfirmationUnavailable),
        2,
        "Error: a confirmation is needed, but there is no terminal to ask in and --yes was not given\n\
         \n\
         How to fix:\n    \
         Run the command in a terminal and answer the confirmation, or add --yes.",
        "internal",
        json!({}),
    );
}

// Output too large

fn too_large(part: OutputPart) -> GatewayError {
    operation(OperationError::OutputTooLarge {
        part,
        size_bytes: 2_000_000,
        limit_bytes: 1_048_576,
    })
}

const TOO_LARGE_FIX: &str = "\n\nHow to fix:\n    \
    Get the document in parts by giving a range of chunks.\n    \
    On the command line, you can also remove the limit with --no-size-limit.";

#[test]
fn a_body_too_large_gives_its_size_and_the_limit() {
    assert_shown(
        too_large(OutputPart::Body),
        1,
        &format!(
            "Error: the document's body is too large to return: 2000000 bytes, over the limit of 1048576 bytes{TOO_LARGE_FIX}"
        ),
        "output_too_large",
        json!({ "size_bytes": 2_000_000, "limit_bytes": 1_048_576 }),
    );
}

#[test]
fn a_chunk_too_large_is_said_to_be_a_chunk() {
    assert!(explained(&too_large(OutputPart::Chunk)).starts_with(
        "Error: the chunk is too large to return: 2000000 bytes, over the limit of 1048576 bytes\n"
    ));
    assert_codes(&too_large(OutputPart::Chunk), 1, "output_too_large");
}

#[test]
fn a_chunk_range_too_large_is_said_to_be_a_chunk_range() {
    assert!(explained(&too_large(OutputPart::ChunkRange)).starts_with(
        "Error: the chunk range is too large to return: 2000000 bytes, over the limit of 1048576 bytes\n"
    ));
    assert_codes(&too_large(OutputPart::ChunkRange), 1, "output_too_large");
}

// Invalid input

#[test]
fn an_existing_collection_name_is_named() {
    assert_shown(
        operation(OperationError::Collection {
            context: in_collection("notes"),
            source: CollectionError::AlreadyExists,
        }),
        1,
        "Error: a collection named \"notes\" already exists\n\
         \n\
         How to fix:\n    \
         Use a different collection name.",
        "internal",
        json!({}),
    );
}

const NAME_RULES_FIX: &str = "\n\nHow to fix:\n    \
    Use a name of 1 to 64 characters made of ASCII letters, digits, \"-\", and \"_\".";

#[test]
fn a_collection_name_against_the_rules_from_the_command_line_is_a_usage_error() {
    assert_shown(
        GatewayError::Cli(CliError::Collection {
            name: name("no tes"),
            source: CollectionError::NameInvalid,
        }),
        2,
        &format!(
            "Error: the collection name \"no tes\" does not follow the naming rules{NAME_RULES_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn a_collection_name_against_the_rules_from_an_operation_is_also_a_usage_error() {
    let error = operation(OperationError::Collection {
        context: in_collection("no tes"),
        source: CollectionError::NameInvalid,
    });

    assert!(explained(&error).ends_with(NAME_RULES_FIX));
    assert_codes(&error, 2, "internal");
}

const ATTRIBUTES_FIX: &str = "\n\nHow to fix:\n    \
    Give the type-specific attributes the collection type takes: a location for a directory or archive collection, and none for a raw collection.";

#[test]
fn attributes_missing_when_creating_name_the_type_and_are_a_usage_error() {
    assert_shown(
        GatewayError::Cli(CliError::Ingest {
            collection_type: CollectionType("directory".to_owned()),
            source: IngestError::AttributesMissingOrExtra,
        }),
        2,
        &format!(
            "Error: the type-specific attributes are missing or unexpected for the collection type directory{ATTRIBUTES_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn attributes_missing_when_changing_do_not_name_the_type_and_exit_with_1() {
    assert_shown(
        operation(OperationError::Ingest {
            context: in_collection("notes"),
            source: IngestError::AttributesMissingOrExtra,
        }),
        1,
        &format!(
            "Error: the type-specific attributes are missing or unexpected for the collection's type{ATTRIBUTES_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn a_location_not_found_gives_the_reason_and_the_three_fixes() {
    assert_shown(
        operation(OperationError::Ingest {
            context: ErrorContext::default(),
            source: IngestError::LocationNotFound {
                source: "no such file or directory".into(),
            },
        }),
        1,
        "Error: the location was not found\n\
         \n\
         Caused by:\n    \
         0: no such file or directory\n\
         \n\
         How to fix:\n    \
         Give a location that exists.\n    \
         If mdya could not read whether the location is a directory or a file, fix the permissions so that mdya can read it, or give a location mdya can read.\n    \
         If the location could not be made absolute, give an absolute path, or move to a working directory that mdya can read, and run the command again.",
        "internal",
        json!({}),
    );
}

#[test]
fn a_location_of_the_wrong_kind_names_what_each_type_takes() {
    assert_shown(
        operation(OperationError::Ingest {
            context: ErrorContext::default(),
            source: IngestError::LocationTypeMismatch,
        }),
        1,
        "Error: the location does not fit the collection type\n\
         \n\
         How to fix:\n    \
         Give a directory for a directory collection, and a .zip, .tar, .tar.gz, or .tgz file for an archive collection.",
        "internal",
        json!({}),
    );
}

const COLLECTION_NOT_FOUND_FIX: &str = "\n\nHow to fix:\n    \
    Check the collection name in the list of collections (mdya collection list, or the list_collections tool).";

#[test]
fn a_missing_collection_is_named_in_the_message_and_the_mcp_details() {
    assert_shown(
        operation(OperationError::CollectionNotFound {
            collection: name("notes"),
        }),
        1,
        &format!("Error: the collection \"notes\" does not exist{COLLECTION_NOT_FOUND_FIX}"),
        "collection_not_found",
        json!({ "collection": "notes" }),
    );
}

#[test]
fn a_missing_collection_from_the_component_without_a_name_is_not_named() {
    assert_shown(
        operation(OperationError::Collection {
            context: ErrorContext::default(),
            source: CollectionError::NotFound,
        }),
        1,
        &format!("Error: the collection does not exist{COLLECTION_NOT_FOUND_FIX}"),
        "collection_not_found",
        json!({}),
    );
}

#[test]
fn nothing_to_change_is_a_usage_error() {
    assert_shown(
        GatewayError::Cli(CliError::NothingToChange),
        2,
        "Error: nothing to change was given\n\
         \n\
         How to fix:\n    \
         Give a description (--description or --no-description), or, for a directory or archive collection, a location to change.",
        "internal",
        json!({}),
    );
}

const UPDATE_NOT_SUPPORTED_FIX: &str = "\n\nHow to fix:\n    \
    Change the documents of a raw collection with mdya put.";

#[test]
fn a_collection_that_cannot_be_updated_is_named() {
    assert_shown(
        operation(OperationError::UpdateNotSupported {
            collection: name("memos"),
        }),
        1,
        &format!(
            "Error: the collection \"memos\" is of a type that cannot be updated{UPDATE_NOT_SUPPORTED_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn a_collection_that_cannot_be_updated_from_the_ingest_implementation_uses_the_context() {
    let error = operation(OperationError::Ingest {
        context: in_collection("memos"),
        source: IngestError::UpdateNotSupported,
    });

    assert!(
        explained(&error)
            .starts_with("Error: the collection \"memos\" is of a type that cannot be updated\n")
    );
    assert_codes(&error, 1, "internal");
}

#[test]
fn a_collection_that_does_not_take_content_is_named() {
    assert_shown(
        operation(OperationError::PutNotSupported {
            collection: name("notes"),
        }),
        1,
        "Error: the collection \"notes\" is of a type that does not take document content\n\
         \n\
         How to fix:\n    \
         Change the documents of a directory or archive collection by changing its source, then run mdya update.",
        "internal",
        json!({}),
    );
}

#[test]
fn a_collection_that_does_not_take_content_without_a_name_is_not_named() {
    let error = operation(OperationError::Ingest {
        context: ErrorContext::default(),
        source: IngestError::PutNotSupported,
    });

    assert!(
        explained(&error).starts_with(
            "Error: the collection is of a type that does not take document content\n"
        )
    );
    assert_codes(&error, 1, "internal");
}

const FILE_TYPE_FIX: &str = "\n\nHow to fix:\n    \
    Give --file-type text/markdown (or markdown, md) for Markdown, or application/pdf (or pdf) for PDF.";

#[test]
fn an_unsupported_file_type_from_the_command_line_names_it() {
    assert_shown(
        GatewayError::Cli(CliError::UnsupportedFileType {
            value: "text/html".to_owned(),
        }),
        2,
        &format!("Error: the file type \"text/html\" is not supported{FILE_TYPE_FIX}"),
        "internal",
        json!({}),
    );
}

#[test]
fn an_unsupported_file_type_from_the_ingest_implementation_is_a_usage_error_without_the_value() {
    assert_shown(
        operation(OperationError::Ingest {
            context: in_collection("memos"),
            source: IngestError::UnsupportedFileType,
        }),
        2,
        &format!("Error: the file type is not supported{FILE_TYPE_FIX}"),
        "internal",
        json!({}),
    );
}

const DOCUMENT_NOT_FOUND_FIX: &str = "\n\nHow to fix:\n    \
    Check the key, for example in the search results.";

#[test]
fn a_missing_document_is_named_with_its_collection() {
    assert_shown(
        operation(OperationError::DocumentNotFound {
            collection: name("notes"),
            key: key("a.md"),
        }),
        1,
        &format!(
            "Error: the document \"a.md\" does not exist in the collection \"notes\"{DOCUMENT_NOT_FOUND_FIX}"
        ),
        "document_not_found",
        json!({ "collection": "notes", "key": "a.md" }),
    );
}

#[test]
fn a_missing_document_from_the_query_uses_the_context() {
    assert_shown(
        operation(OperationError::Query {
            context: of_document("notes", "a.md"),
            source: QueryError::DocumentNotFound,
        }),
        1,
        &format!(
            "Error: the document \"a.md\" does not exist in the collection \"notes\"{DOCUMENT_NOT_FOUND_FIX}"
        ),
        "document_not_found",
        json!({ "collection": "notes", "key": "a.md" }),
    );
}

#[test]
fn a_missing_document_from_the_index_without_a_context_is_not_named() {
    assert_shown(
        operation(OperationError::Index {
            context: ErrorContext::default(),
            source: IndexError::DocumentNotFound,
        }),
        1,
        &format!("Error: the document does not exist{DOCUMENT_NOT_FOUND_FIX}"),
        "document_not_found",
        json!({}),
    );
}

fn query(source: QueryError) -> GatewayError {
    operation(OperationError::Query {
        context: ErrorContext::default(),
        source,
    })
}

#[test]
fn an_empty_query_is_a_usage_error_about_the_query_input() {
    assert_shown(
        query(QueryError::EmptyQuery),
        2,
        "Error: the query is empty\n\
         \n\
         How to fix:\n    \
         Give a query that is not just white space.",
        "invalid_input",
        json!({ "input": "query" }),
    );
}

#[test]
fn a_zero_limit_is_a_usage_error_about_the_limit_input() {
    assert_shown(
        query(QueryError::ZeroLimit),
        2,
        "Error: the limit is zero\n\
         \n\
         How to fix:\n    \
         Give a limit of 1 or more.",
        "invalid_input",
        json!({ "input": "limit" }),
    );
}

#[test]
fn a_missing_chunk_exits_with_1_and_is_about_the_chunk_index_input() {
    assert_shown(
        query(QueryError::ChunkNotFound),
        1,
        "Error: the chunk does not exist\n\
         \n\
         How to fix:\n    \
         Give a start chunk index (--chunk-index, or chunk_index in MCP) smaller than the document's chunk count (chunk_count in the search results).",
        "invalid_input",
        json!({ "input": "chunk_index" }),
    );
}

#[test]
fn an_invalid_chunk_range_is_a_usage_error_about_the_end_chunk_index_input() {
    assert_shown(
        query(QueryError::InvalidChunkRange),
        2,
        "Error: the chunk range is not valid\n\
         \n\
         How to fix:\n    \
         Give the start chunk index (--chunk-index, or chunk_index in MCP), and an end chunk index (--end-chunk-index, or end_chunk_index in MCP) that is not smaller than the start.",
        "invalid_input",
        json!({ "input": "end_chunk_index" }),
    );
}

#[test]
fn a_query_error_found_by_the_command_line_has_the_same_exit_code() {
    let error = GatewayError::Cli(CliError::Query(QueryError::ZeroLimit));

    assert!(explained(&error).starts_with("Error: the limit is zero\n"));
    assert_codes(&error, 2, "invalid_input");
}

#[test]
fn an_unknown_setting_name_is_named_as_given() {
    assert_shown(
        GatewayError::Cli(CliError::Config(ConfigError::UnknownName {
            name: "embeding.model".to_owned(),
        })),
        2,
        "Error: there is no setting named \"embeding.model\"\n\
         \n\
         How to fix:\n    \
         Check the setting names with mdya config list.",
        "internal",
        json!({}),
    );
}

#[test]
fn an_invalid_setting_value_names_the_setting_as_written_and_what_it_accepts() {
    assert_shown(
        operation(OperationError::Config(ConfigError::InvalidValue {
            name: SettingName::EmbeddingModel,
            accepted: "a preset name or ollama:<model>".to_owned(),
        })),
        2,
        "Error: the value is not valid for the setting embedding.model\n\
         \n\
         How to fix:\n    \
         Give a value the setting accepts: a preset name or ollama:<model>.",
        "internal",
        json!({}),
    );
}

#[test]
fn a_tool_input_against_its_schema_is_about_that_input() {
    assert_shown(
        GatewayError::Mcp(McpError::InvalidToolInput {
            input: "mode".to_owned(),
            detail: "unknown variant `fast`".to_owned(),
        }),
        2,
        "Error: the tool input mode is not valid: unknown variant `fast`\n\
         \n\
         How to fix:\n    \
         Fix the input mode and call the tool again.",
        "invalid_input",
        json!({ "input": "mode" }),
    );
}

#[test]
fn a_zero_limit_given_to_the_mcp_server_is_shown_as_one_the_search_finds() {
    assert_shown(
        GatewayError::Mcp(McpError::ZeroLimit),
        2,
        "Error: the limit is zero\n\
         \n\
         How to fix:\n    \
         Give a limit of 1 or more.",
        "invalid_input",
        json!({ "input": "limit" }),
    );
}

#[test]
fn an_invalid_log_level_names_the_value_and_is_a_usage_error() {
    assert_shown(
        GatewayError::Entry(EntryError::InvalidLogLevel(InvalidLogLevel {
            value: "loud".to_owned(),
            detail: "error parsing level filter".to_owned(),
        })),
        2,
        "Error: the log level \"loud\" is not valid: error parsing level filter\n\
         \n\
         How to fix:\n    \
         Give --log-level a level (error, warn, info, debug, or trace), or levels for each part of mdya or of the libraries it uses, such as mdya=debug.",
        "internal",
        json!({}),
    );
}

#[test]
fn invalid_arguments_point_to_the_help() {
    assert_shown(
        GatewayError::Cli(CliError::Usage {
            detail: "unexpected argument '--fast'".to_owned(),
        }),
        2,
        "Error: the arguments are not valid: unexpected argument '--fast'\n\
         \n\
         How to fix:\n    \
         See mdya --help, or run the command with --help.",
        "internal",
        json!({}),
    );
}

// Document that cannot be read

#[test]
fn content_that_cannot_be_read_gives_the_reason() {
    assert_shown(
        operation(OperationError::ContentUnreadable {
            source: io::Error::other("permission denied"),
        }),
        1,
        "Error: the document content cannot be read\n\
         \n\
         Caused by:\n    \
         0: permission denied\n\
         \n\
         How to fix:\n    \
         Make the content readable (for example, fix the permissions), then run mdya put again.",
        "internal",
        json!({}),
    );
}

#[test]
fn a_body_that_cannot_be_extracted_gives_the_reason() {
    assert_shown(
        operation(OperationError::Ingest {
            context: of_document("memos", "a.pdf"),
            source: IngestError::CannotExtract(FormatError::CannotExtract {
                source: "the PDF is broken".into(),
            }),
        }),
        1,
        "Error: the document's body cannot be extracted\n\
         \n\
         Caused by:\n    \
         0: the PDF is broken\n\
         \n\
         How to fix:\n    \
         Give content that is not broken and is of the given file type (for example, Markdown in UTF-8), then run mdya put again.",
        "internal",
        json!({}),
    );
}

// Source that cannot be read

#[test]
fn a_source_that_cannot_be_read_gives_the_reason() {
    assert_shown(
        operation(OperationError::Ingest {
            context: in_collection("notes"),
            source: IngestError::SourceUnreadable {
                source: "no such directory".into(),
            },
        }),
        1,
        "Error: the source cannot be read\n\
         \n\
         Caused by:\n    \
         0: no such directory\n\
         \n\
         How to fix:\n    \
         Make the source readable (connect the disk, fix the location, and so on), then run mdya update again.\n    \
         If the source moved, change the location with mdya collection set.",
        "internal",
        json!({}),
    );
}

// Embedding model that cannot be used

#[test]
fn an_unusable_embedding_model_gives_the_reason_and_the_general_fix() {
    assert_shown(
        operation(OperationError::Index {
            context: in_collection("notes"),
            source: IndexError::Embedding(EmbeddingError::ModelUnavailable {
                source: "Ollama is not running".into(),
            }),
        }),
        1,
        "Error: the embedding model cannot be used\n\
         \n\
         Caused by:\n    \
         0: Ollama is not running\n\
         \n\
         How to fix:\n    \
         Fix the cause shown above (connect to the network, set the Hugging Face token in the environment variable HF_TOKEN, start Ollama, and so on), then run the operation again.",
        "embedding_model_unavailable",
        json!({}),
    );
}

#[test]
fn an_embedding_model_without_a_cache_directory_gives_the_cache_directory_fix_instead() {
    let error = query(QueryError::Embedding(EmbeddingError::ModelUnavailable {
        source: Box::new(ModelCacheDirUndetermined {
            source: MakePathAbsoluteError::HomeDirUnknown,
        }),
    }));

    assert_eq!(
        explained(&error),
        "Error: the embedding model cannot be used\n\
         \n\
         Caused by:\n    \
         0: the embedding model cache directory cannot be determined\n    \
         1: the home directory is unknown, so a leading `~` cannot be expanded\n\
         \n\
         How to fix:\n    \
         Give an absolute path for the embedding model cache directory with --model-cache-dir, then run the operation again.\n    \
         If the working directory cannot be read, you can also move to a working directory that mdya can read, and run the operation again.\n    \
         For the MCP server, start it again after either of these."
    );
    assert_codes(&error, 1, "embedding_model_unavailable");
}

// Index or settings that cannot be read

const INDEX_FIX: &str = "\n\nHow to fix:\n    \
    Delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.\n    \
    Then give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.";

#[test]
fn settings_that_cannot_be_read_ask_to_undo_an_edit_first() {
    assert_shown(
        GatewayError::Entry(EntryError::Config(ConfigError::CannotRead {
            source: "expected a table".into(),
        })),
        1,
        "Error: the settings cannot be read\n\
         \n\
         Caused by:\n    \
         0: expected a table\n\
         \n\
         How to fix:\n    \
         If you edited the settings file directly, undo the edit or fix the file so that it can be read.\n    \
         Otherwise, or if that does not fix it, delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.\n    \
         In that case, give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.",
        "internal",
        json!({}),
    );
}

#[test]
fn an_index_that_cannot_be_read_outside_a_search_asks_to_create_it_again() {
    assert_shown(
        operation(OperationError::Index {
            context: in_collection("notes"),
            source: IndexError::CannotRead {
                source: "corrupt file".into(),
            },
        }),
        1,
        &format!("Error: the index cannot be read\n\nCaused by:\n    0: corrupt file{INDEX_FIX}"),
        "internal",
        json!({}),
    );
}

const SEARCH_INDEX_FIX: &str = "\n\nHow to fix:\n    \
    If the index was being optimized while you searched, search again, as the index is not broken then.\n    \
    If searching again fails in the same way, delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.\n    \
    In that case, give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.";

#[test]
fn an_index_that_cannot_be_read_in_a_search_asks_to_search_again_first() {
    assert_shown(
        query(QueryError::CannotRead {
            source: "version not found".into(),
        }),
        1,
        &format!(
            "Error: the index cannot be read\n\nCaused by:\n    0: version not found{SEARCH_INDEX_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn an_index_that_cannot_be_read_through_the_query_in_a_search_also_asks_to_search_again() {
    let error = query(QueryError::Index(IndexError::CannotRead {
        source: "version not found".into(),
    }));

    assert!(explained(&error).ends_with(SEARCH_INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn an_index_that_cannot_be_read_with_a_collection_but_no_key_is_taken_as_a_search() {
    let error = operation(OperationError::Query {
        context: in_collection("notes"),
        source: QueryError::CannotRead {
            source: "version not found".into(),
        },
    });

    assert!(explained(&error).ends_with(SEARCH_INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn an_index_that_cannot_be_read_through_the_collections_asks_to_create_it_again() {
    let error = operation(OperationError::Collection {
        context: in_collection("notes"),
        source: CollectionError::Index(IndexError::CannotRead {
            source: "corrupt file".into(),
        }),
    });

    assert!(explained(&error).ends_with(INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn an_index_that_cannot_be_read_through_an_ingest_asks_to_create_it_again() {
    let error = operation(OperationError::Ingest {
        context: in_collection("notes"),
        source: IngestError::Index(IndexError::CannotRead {
            source: "corrupt file".into(),
        }),
    });

    assert!(explained(&error).ends_with(INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn an_index_that_cannot_be_read_when_getting_a_document_asks_to_create_it_again() {
    let error = operation(OperationError::Query {
        context: of_document("notes", "a.md"),
        source: QueryError::CannotRead {
            source: "corrupt file".into(),
        },
    });

    assert!(explained(&error).ends_with(INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn collection_information_that_cannot_be_read_asks_to_create_the_index_again() {
    assert_shown(
        operation(OperationError::Collection {
            context: ErrorContext::default(),
            source: CollectionError::CannotRead {
                source: "corrupt file".into(),
            },
        }),
        1,
        &format!(
            "Error: the collection information cannot be read\n\nCaused by:\n    0: corrupt file{INDEX_FIX}"
        ),
        "internal",
        json!({}),
    );
}

// Memory limit exceeded

#[test]
fn going_over_the_memory_limit_gives_the_limit_and_the_two_settings() {
    assert_eq!(
        written(&report().memory_limit_explanation(8192)),
        "Error: mdya's memory usage went over the limit of 8192 MB\n\
         \n\
         How to fix:\n    \
         Raise the memory limit (mdya config set resources.memory_limit_mb) or lower the parallelism (mdya config set resources.embedding_parallelism), then run the operation again."
    );
}

// Address that cannot be listened on

const ADDRESS_FIX: &str = "\n\nHow to fix:\n    \
    Give a different address or port with --address, or, if another process is using the address, stop it.";

#[test]
fn an_address_in_use_gives_the_address_and_the_reason() {
    assert_shown(
        GatewayError::Mcp(McpError::AddressUnavailable {
            host: "127.0.0.1".to_owned(),
            port: 8000,
            source: io::Error::new(io::ErrorKind::AddrInUse, "address in use"),
        }),
        1,
        &format!(
            "Error: cannot listen on 127.0.0.1:8000\n\nCaused by:\n    0: address in use{ADDRESS_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn an_ipv6_address_is_written_in_brackets() {
    let error = GatewayError::Mcp(McpError::AddressUnavailable {
        host: "::1".to_owned(),
        port: 8000,
        source: io::Error::new(io::ErrorKind::AddrInUse, "address in use"),
    });

    assert!(explained(&error).starts_with("Error: cannot listen on [::1]:8000\n"));
    assert_codes(&error, 1, "internal");
}

// Another write operation in progress (also the first test of the form)

#[test]
fn another_write_operation_in_progress_exits_with_1() {
    let error = operation(OperationError::WriteLock(WriteLockError::InProgress));

    assert_codes(&error, 1, "internal");
}

// Unexpected failure

#[test]
fn a_panic_asks_for_a_bug_report() {
    assert_eq!(
        written(&report().panic_explanation()),
        "Error: an unexpected failure occurred, which is a bug in mdya\n\
         \n\
         How to fix:\n    \
         Report the bug at https://github.com/yoshihirosuzuki/mdya/issues, with what you ran, the output of mdya --version, and the messages mdya printed on stderr."
    );
}

#[test]
fn a_panic_in_an_mcp_tool_call_is_internal_without_details() {
    let mcp = report().mcp_panic_error();

    assert_eq!(mcp.code, "internal");
    assert!(mcp.details.is_empty());
}

// Query longer than the embedding model's context length

#[test]
fn a_query_too_long_for_the_model_is_about_the_query_input() {
    assert_shown(
        query(QueryError::Embedding(
            EmbeddingError::QueryExceedsContextLength,
        )),
        1,
        "Error: the query is longer than the embedding model's context length\n\
         \n\
         How to fix:\n    \
         Shorten the query, or use full-text search (--mode fts, or mode fts in MCP).\n    \
         To search with a longer query, set an embedding model with a longer context length (mdya config set embedding.model) and run mdya embedding rebuild.\n    \
         For the MCP server, start it again after changing the setting.",
        "invalid_input",
        json!({ "input": "query" }),
    );
}

// Index or settings that cannot be written

const WRITE_FIX: &str = "\n\nHow to fix:\n    \
    Fix the cause shown above (free up disk space, fix the write permissions, and so on), then run the operation again.";

#[test]
fn settings_that_cannot_be_written_give_the_reason() {
    assert_shown(
        operation(OperationError::Config(ConfigError::CannotWrite {
            source: "disk full".into(),
        })),
        1,
        &format!(
            "Error: the settings cannot be written\n\nCaused by:\n    0: disk full{WRITE_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn collection_information_that_cannot_be_written_gives_the_reason() {
    let error = operation(OperationError::Collection {
        context: in_collection("notes"),
        source: CollectionError::CannotWrite {
            source: "disk full".into(),
        },
    });

    assert_eq!(
        explained(&error),
        format!(
            "Error: the collection information cannot be written\n\nCaused by:\n    0: disk full{WRITE_FIX}"
        )
    );
    assert_codes(&error, 1, "internal");
}

#[test]
fn a_write_lock_that_cannot_be_taken_gives_the_reason() {
    assert_shown(
        operation(OperationError::WriteLock(WriteLockError::Unavailable {
            source: Box::new(io::Error::other("permission denied")),
        })),
        1,
        &format!(
            "Error: the write lock cannot be taken\n\nCaused by:\n    0: permission denied{WRITE_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn an_index_error_through_the_collection_component_is_shown_as_the_index_error() {
    let error = operation(OperationError::Collection {
        context: in_collection("notes"),
        source: CollectionError::Index(IndexError::CannotWrite {
            source: "disk full".into(),
        }),
    });

    assert!(explained(&error).starts_with("Error: the index cannot be written\n"));
    assert_codes(&error, 1, "internal");
}

// Embeddings not rebuilt

const REBUILD_FIX: &str = "\n\nHow to fix:\n    \
    Rebuild the embeddings with mdya embedding rebuild, then run the operation again.";

fn model_mismatch(configured_dimension: Option<usize>) -> GatewayError {
    operation(OperationError::Ingest {
        context: in_collection("notes"),
        source: IngestError::Index(IndexError::ModelMismatch {
            configured_name: "all-minilm".to_owned(),
            configured_dimension,
            indexed: EmbeddingModel {
                name: "all-minilm".to_owned(),
                dimension: 768,
            },
        }),
    })
}

#[test]
fn a_model_mismatch_gives_both_models_with_their_dimensions() {
    assert_shown(
        model_mismatch(Some(384)),
        1,
        &format!(
            "Error: the configured embedding model all-minilm (384 dimensions) differs from the one the index's embeddings were made with, all-minilm (768 dimensions){REBUILD_FIX}"
        ),
        "internal",
        json!({}),
    );
}

#[test]
fn a_model_mismatch_with_an_unchecked_dimension_leaves_it_out() {
    assert!(explained(&model_mismatch(None)).starts_with(
        "Error: the configured embedding model all-minilm differs from the one the index's embeddings were made with, all-minilm (768 dimensions)\n"
    ));
    assert_codes(&model_mismatch(None), 1, "internal");
}

#[test]
fn a_rebuild_stopped_while_switching_over_asks_to_rebuild() {
    assert_shown(
        operation(OperationError::UpdateStopped {
            counts: Default::default(),
            context: in_collection("notes"),
            source: IngestError::Index(IndexError::RebuildIncomplete),
        }),
        1,
        &format!(
            "Error: a rebuild of the embeddings was stopped while switching over to the new embedding model{REBUILD_FIX}"
        ),
        "internal",
        json!({}),
    );
}

// Settings and index directory that cannot be determined

#[test]
fn a_settings_and_index_directory_that_cannot_be_determined_gives_the_reason() {
    assert_shown(
        GatewayError::Entry(EntryError::DataDirUndetermined(
            MakePathAbsoluteError::HomeDirUnknown,
        )),
        1,
        "Error: the settings and index directory cannot be determined\n\
         \n\
         Caused by:\n    \
         0: the home directory is unknown, so a leading `~` cannot be expanded\n\
         \n\
         How to fix:\n    \
         Give an absolute path with --data-dir.\n    \
         If the working directory cannot be read, you can also move to a working directory that mdya can read, and run the command again.",
        "internal",
        json!({}),
    );
}

// Exchange with the MCP client that cannot be started

#[test]
fn an_mcp_exchange_that_cannot_start_gives_the_reason() {
    assert_shown(
        GatewayError::Mcp(McpError::Transport {
            source: "connection closed before initialization".into(),
        }),
        1,
        "Error: the connection with the MCP client cannot be started\n\
         \n\
         Caused by:\n    \
         0: connection closed before initialization\n\
         \n\
         How to fix:\n    \
         Check the MCP client's settings, then start the MCP server again from the MCP client.",
        "internal",
        json!({}),
    );
}

// Collection of a type this version of mdya does not know

#[test]
fn a_collection_of_an_unknown_type_names_the_collection_and_the_type() {
    assert_shown(
        operation(OperationError::UnknownCollectionType {
            collection: name("notes"),
            collection_type: CollectionType("future".to_owned()),
        }),
        1,
        "Error: this version of mdya does not know the collection type \"future\" of the collection \"notes\"\n\
         \n\
         How to fix:\n    \
         Use a version of mdya that can handle the collection type, or delete the collection with mdya collection remove.",
        "internal",
        json!({}),
    );
}

// Branches for errors without the values the messages name.

#[test]
fn a_collection_name_against_the_rules_without_a_name_is_not_named() {
    let error = operation(OperationError::Collection {
        context: ErrorContext::default(),
        source: CollectionError::NameInvalid,
    });

    assert!(
        explained(&error)
            .starts_with("Error: the collection name does not follow the naming rules\n")
    );
    assert_codes(&error, 2, "internal");
}

#[test]
fn an_existing_collection_without_a_name_is_not_named() {
    let error = operation(OperationError::Collection {
        context: ErrorContext::default(),
        source: CollectionError::AlreadyExists,
    });

    assert!(explained(&error).starts_with("Error: a collection already exists\n"));
    assert_codes(&error, 1, "internal");
}

#[test]
fn a_missing_document_with_only_a_collection_name_names_the_collection() {
    let error = operation(OperationError::Index {
        context: in_collection("notes"),
        source: IndexError::DocumentNotFound,
    });

    assert!(
        explained(&error)
            .starts_with("Error: the document does not exist in the collection \"notes\"\n")
    );
    assert_eq!(
        mcp(&error).details,
        details(json!({ "collection": "notes" }))
    );
    assert_codes(&error, 1, "document_not_found");
}

// Command line errors other than the ones only it finds.

#[test]
fn a_collection_error_found_by_the_command_line_names_the_collection_it_was_given() {
    let error = GatewayError::Cli(CliError::Collection {
        name: name("notes"),
        source: CollectionError::AlreadyExists,
    });

    assert!(explained(&error).starts_with("Error: a collection named \"notes\" already exists\n"));
    assert_codes(&error, 1, "internal");
}

#[test]
fn an_ingest_error_found_by_the_command_line_is_shown_like_the_operations_one() {
    let error = GatewayError::Cli(CliError::Ingest {
        collection_type: CollectionType("archive".to_owned()),
        source: IngestError::LocationTypeMismatch,
    });

    assert!(
        explained(&error).starts_with("Error: the location does not fit the collection type\n")
    );
    assert_codes(&error, 1, "internal");
}

// The remaining branches.

#[test]
fn an_index_that_cannot_be_read_through_the_query_when_getting_a_document_asks_to_create_it_again()
{
    let error = operation(OperationError::Query {
        context: of_document("notes", "a.md"),
        source: QueryError::Index(IndexError::CannotRead {
            source: "corrupt file".into(),
        }),
    });

    assert!(explained(&error).ends_with(INDEX_FIX));
    assert_codes(&error, 1, "internal");
}

#[test]
fn a_collection_that_cannot_be_updated_without_a_name_is_not_named() {
    let error = operation(OperationError::Ingest {
        context: ErrorContext::default(),
        source: IngestError::UpdateNotSupported,
    });

    assert!(
        explained(&error)
            .starts_with("Error: the collection is of a type that cannot be updated\n")
    );
    assert_codes(&error, 1, "internal");
}
