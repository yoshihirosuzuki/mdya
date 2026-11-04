//! Decides, for every error, the command line's exit code, the MCP error, and
//! the explanation shown to the user, in one place.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the rest of `gateway` does not use these yet")
)]

use std::error::Error;
use std::sync::Arc;

use nu_ansi_term::Color;
use serde::Serialize;

use crate::collection::CollectionError;
use crate::config::ConfigError;
use crate::embedding::EmbeddingError;
use crate::format::FormatError;
use crate::index::IndexError;
use crate::ingest::IngestError;
use crate::query::QueryError;

use super::cli::CliError;
use super::entry::EntryError;
use super::escape::Escape;
use super::mcp::{McpError, socket_address};
use super::operations::{
    ErrorContext, MODEL_CACHE_DIR_FIX, OperationError, OutcomeAnomaly, OutputPart,
    model_cache_dir_undetermined,
};
use super::write_lock::WriteLockError;

#[cfg(test)]
pub(crate) mod test_double;
#[cfg(test)]
mod tests;

/// An error to report, wrapped by the submodule that found it: the same
/// component error may be shown differently depending on where it was found.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GatewayError {
    #[error(transparent)]
    Operation(OperationError),
    #[error(transparent)]
    Entry(EntryError),
    #[error(transparent)]
    Cli(CliError),
    #[error(transparent)]
    Mcp(McpError),
}

// The errors are kept as the source of other errors and cross task
// boundaries, so each must be an `Error` that is `Send + Sync + 'static`.
const _: () = {
    const fn shareable<T: std::error::Error + Send + Sync + 'static>() {}
    shareable::<GatewayError>();
    shareable::<OperationError>();
    shareable::<EntryError>();
    shareable::<CliError>();
    shareable::<McpError>();
};

/// How to write an explanation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExplainStyle {
    pub(crate) color: bool,
    pub(crate) replace_controls: bool,
}

/// The content of an MCP tool error: its `code`, `message`, and `details`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct McpErrorBody {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) details: serde_json::Map<String, serde_json::Value>,
}

/// What the other submodules use from this one. A trait, so that their tests
/// can replace it. Every function only computes.
pub(crate) trait ErrorReport: Send + Sync {
    /// The command line's exit code for the error.
    fn exit_code(&self, error: &GatewayError) -> u8;

    /// The command line's exit code for an operation that ended without an
    /// error: 0 when there is no anomaly.
    fn outcome_exit_code(&self, anomalies: &[OutcomeAnomaly]) -> u8;

    /// The explanation: the error message, the chain of causes, and how to
    /// fix it, laid out in one fixed form, without a final line break.
    fn explain(&self, error: &GatewayError, style: ExplainStyle) -> String;

    /// The MCP error. Its `message` is the explanation, without color and
    /// without replacing control characters or the characters that change
    /// the direction of text.
    fn mcp_error(&self, error: &GatewayError) -> McpErrorBody;

    /// The explanation of an unexpected failure, asking for a bug report.
    fn explain_panic(&self, style: ExplainStyle) -> String;

    /// The MCP error for a tool call that panicked (code `internal`).
    fn mcp_panic_error(&self) -> McpErrorBody;

    /// The explanation of going over the memory limit of `limit_mb` MB, which
    /// the assembly gives the watchdog to write.
    fn explain_memory_limit(&self, limit_mb: u64, style: ExplainStyle) -> String;
}

/// Creates the real one. It replaces control characters and the characters
/// that change the direction of text with `escape`.
pub(crate) fn error_report(escape: Arc<dyn Escape>) -> Arc<dyn ErrorReport> {
    Arc::new(Report { escape })
}

struct Report {
    escape: Arc<dyn Escape>,
}

impl ErrorReport for Report {
    fn exit_code(&self, error: &GatewayError) -> u8 {
        shown(error, &|value| value.to_owned()).exit_code
    }

    fn outcome_exit_code(&self, anomalies: &[OutcomeAnomaly]) -> u8 {
        if anomalies.is_empty() { 0 } else { 1 }
    }

    fn explain(&self, error: &GatewayError, style: ExplainStyle) -> String {
        let value = |text: &str| self.text(text, style);
        let shown = shown(error, &value);
        let causes: Vec<String> = causes(shown.causes_of)
            .map(|cause| self.text(&cause.to_string(), style))
            .collect();
        lay_out(&shown.message, &causes, &shown.fix, style.color)
    }

    fn mcp_error(&self, error: &GatewayError) -> McpErrorBody {
        let shown = shown(error, &|value| value.to_owned());
        let causes: Vec<String> = causes(shown.causes_of)
            .map(|cause| cause.to_string())
            .collect();
        McpErrorBody {
            code: shown.code,
            message: lay_out(&shown.message, &causes, &shown.fix, false),
            details: shown.details,
        }
    }

    fn explain_panic(&self, style: ExplainStyle) -> String {
        lay_out(PANIC_MESSAGE, &[], &[PANIC_FIX.to_owned()], style.color)
    }

    fn mcp_panic_error(&self) -> McpErrorBody {
        McpErrorBody {
            code: INTERNAL,
            message: lay_out(PANIC_MESSAGE, &[], &[PANIC_FIX.to_owned()], false),
            details: serde_json::Map::new(),
        }
    }

    fn explain_memory_limit(&self, limit_mb: u64, style: ExplainStyle) -> String {
        lay_out(
            &format!("mdya's memory usage went over the limit of {limit_mb} MB"),
            &[],
            &[MEMORY_LIMIT_FIX.to_owned()],
            style.color,
        )
    }
}

impl Report {
    /// `text` from the user or the ingested data, replaced if `style` says
    /// so.
    fn text(&self, text: &str, style: ExplainStyle) -> String {
        if style.replace_controls {
            return self.escape.replace_controls(text).into_owned();
        }
        text.to_owned()
    }
}

/// Lays out the explanation: the message, the causes numbered from the
/// nearest, and the fix, one line each.
fn lay_out(message: &str, causes: &[String], fix: &[String], color: bool) -> String {
    let heading = |text: &str, paint: Color| {
        if color {
            paint.paint(text).to_string()
        } else {
            text.to_owned()
        }
    };
    let mut explanation = format!("{} {message}", heading("Error:", Color::Red));
    if !causes.is_empty() {
        explanation.push_str(&format!("\n\n{}", heading("Caused by:", Color::Yellow)));
        for (number, cause) in causes.iter().enumerate() {
            explanation.push_str(&format!("\n    {number}: {cause}"));
        }
    }
    explanation.push_str(&format!("\n\n{}", heading("How to fix:", Color::Green)));
    for line in fix {
        explanation.push_str(&format!("\n    {line}"));
    }
    explanation
}

/// The causes of `error`: its source, the source's source, and so on.
fn causes<'a>(
    error: Option<&'a (dyn Error + 'static)>,
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(error.and_then(|error| error.source()), |&cause| {
        cause.source()
    })
}

/// What an error is shown as.
struct Shown<'a> {
    exit_code: u8,
    code: &'static str,
    details: serde_json::Map<String, serde_json::Value>,
    message: String,
    /// The error whose causes are listed: the one the message stands for.
    causes_of: Option<&'a (dyn Error + 'static)>,
    fix: Vec<String>,
}

impl<'a> Shown<'a> {
    fn new(exit_code: u8, message: String, fix: &[&str]) -> Self {
        Self {
            exit_code,
            code: INTERNAL,
            details: serde_json::Map::new(),
            message,
            causes_of: None,
            fix: fix.iter().map(|&line| line.to_owned()).collect(),
        }
    }

    fn code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }

    fn detail(mut self, name: &str, value: impl Into<serde_json::Value>) -> Self {
        self.details.insert(name.to_owned(), value.into());
        self
    }

    fn causes_of(mut self, error: &'a (dyn Error + 'static)) -> Self {
        self.causes_of = Some(error);
        self
    }

    fn invalid_input(self, input: &str) -> Self {
        self.code(INVALID_INPUT).detail("input", input)
    }

    fn with_fix(mut self, line: String) -> Self {
        self.fix.push(line);
        self
    }
}

/// Turns user or ingested text into what the explanation shows.
type Value<'v> = dyn Fn(&str) -> String + 'v;

/// The one place that maps every error to what is shown.
fn shown<'a>(error: &'a GatewayError, value: &Value<'_>) -> Shown<'a> {
    match error {
        GatewayError::Operation(error) => operation(error, value),
        GatewayError::Entry(error) => entry(error, value),
        GatewayError::Cli(error) => cli(error, value),
        GatewayError::Mcp(error) => mcp(error, value),
    }
}

fn operation<'a>(error: &'a OperationError, value: &Value<'_>) -> Shown<'a> {
    match error {
        OperationError::CollectionNotFound { collection } => {
            collection_not_found(Some(&collection.0), value)
        }
        OperationError::UpdateNotSupported { collection } => {
            update_not_supported(Some(&collection.0), value)
        }
        OperationError::PutNotSupported { collection } => {
            put_not_supported(Some(&collection.0), value)
        }
        OperationError::DocumentNotFound { collection, key } => {
            document_not_found(Some(&collection.0), Some(&key.0), value)
        }
        OperationError::WriteLock(error) => write_lock(error),
        OperationError::OutputTooLarge {
            part,
            size_bytes,
            limit_bytes,
        } => Shown::new(
            1,
            format!(
                "{} too large to return: {size_bytes} bytes, over the limit of {limit_bytes} bytes",
                match part {
                    OutputPart::Body => "the document's body is",
                    OutputPart::Chunk => "the chunk is",
                    OutputPart::ChunkRange => "the chunk range is",
                }
            ),
            &TOO_LARGE_FIX,
        )
        .code(OUTPUT_TOO_LARGE)
        .detail("size_bytes", *size_bytes)
        .detail("limit_bytes", *limit_bytes),
        OperationError::UnknownCollectionType {
            collection,
            collection_type,
        } => Shown::new(
            1,
            format!(
                "this version of mdya does not know the collection type \"{}\" of the collection \"{}\"",
                value(&collection_type.0),
                value(&collection.0)
            ),
            &[UNKNOWN_TYPE_FIX],
        ),
        OperationError::ContentUnreadable { .. } => Shown::new(
            1,
            "the document content cannot be read".to_owned(),
            &[CONTENT_UNREADABLE_FIX],
        )
        .causes_of(error),
        OperationError::Collection { context, source } => collection(context, source, value),
        OperationError::Index { context, source } => index(context, source, ReadFor::Other, value),
        OperationError::Query { context, source } => query(context, source, value),
        OperationError::Ingest { context, source }
        | OperationError::UpdateStopped {
            context, source, ..
        } => ingest(context, source, value),
        OperationError::Config(error) => config(error, value),
    }
}

fn entry<'a>(error: &'a EntryError, value: &Value<'_>) -> Shown<'a> {
    match error {
        EntryError::InvalidLogLevel(level) => Shown::new(
            2,
            format!(
                "the log level \"{}\" is not valid: {}",
                value(&level.value),
                value(&level.detail)
            ),
            &[LOG_LEVEL_FIX],
        ),
        EntryError::Config(error) => config(error, value),
        EntryError::DataDirUndetermined(_) => Shown::new(
            1,
            "the settings and index directory cannot be determined".to_owned(),
            &DATA_DIR_FIX,
        )
        .causes_of(error),
    }
}

fn cli<'a>(error: &'a CliError, value: &Value<'_>) -> Shown<'a> {
    match error {
        CliError::Usage { detail } => Shown::new(
            2,
            format!("the arguments are not valid: {}", value(detail)),
            &[USAGE_FIX],
        ),
        CliError::NothingToChange => Shown::new(
            2,
            "nothing to change was given".to_owned(),
            &[NOTHING_TO_CHANGE_FIX],
        ),
        CliError::UnsupportedFileType { value: file_type } => {
            unsupported_file_type(Some(file_type), value)
        }
        CliError::Collection { name, source } => {
            let context = ErrorContext {
                collection: Some(name.clone()),
                key: None,
            };
            collection(&context, source, value)
        }
        CliError::Ingest {
            collection_type,
            source,
        } => {
            if let IngestError::AttributesMissingOrExtra = source {
                return Shown::new(
                    2,
                    format!(
                        "the type-specific attributes are missing or unexpected for the collection type {}",
                        value(&collection_type.0)
                    ),
                    &[ATTRIBUTES_FIX],
                );
            }
            ingest(&ErrorContext::default(), source, value)
        }
        CliError::Query(error) => query(&ErrorContext::default(), error, value),
        CliError::Config(error) => config(error, value),
        CliError::ConfirmationUnavailable => Shown::new(
            2,
            "a confirmation is needed, but there is no terminal to ask in and --yes was not given"
                .to_owned(),
            &[CONFIRMATION_FIX],
        ),
    }
}

fn mcp<'a>(error: &'a McpError, value: &Value<'_>) -> Shown<'a> {
    match error {
        McpError::AddressUnavailable { host, port, .. } => Shown::new(
            1,
            format!("cannot listen on {}", socket_address(&value(host), *port)),
            &[ADDRESS_FIX],
        )
        .causes_of(error),
        McpError::Transport { .. } => Shown::new(
            1,
            "the connection with the MCP client cannot be started".to_owned(),
            &[MCP_START_FIX],
        )
        .causes_of(error),
        McpError::InvalidToolInput { input, detail } => Shown::new(
            2,
            format!(
                "the tool input {} is not valid: {}",
                value(input),
                value(detail)
            ),
            &[],
        )
        .with_fix(format!(
            "Fix the input {} and call the tool again.",
            value(input)
        ))
        .invalid_input(input),
    }
}

/// The collection error, with the context only borrowed for the names.
fn collection<'a>(
    context: &ErrorContext,
    error: &'a CollectionError,
    value: &Value<'_>,
) -> Shown<'a> {
    let name = context.collection.as_ref().map(|name| name.0.as_str());
    match error {
        CollectionError::NameInvalid => name_invalid(name, value),
        CollectionError::AlreadyExists => Shown::new(
            1,
            match name {
                Some(name) => format!("a collection named \"{}\" already exists", value(name)),
                None => "a collection already exists".to_owned(),
            },
            &[ALREADY_EXISTS_FIX],
        ),
        CollectionError::NotFound => collection_not_found(name, value),
        CollectionError::CannotRead { .. } => Shown::new(
            1,
            "the collection information cannot be read".to_owned(),
            &INDEX_UNREADABLE_FIX,
        )
        .causes_of(error),
        CollectionError::CannotWrite { .. } => Shown::new(
            1,
            "the collection information cannot be written".to_owned(),
            &[WRITE_FIX],
        )
        .causes_of(error),
        CollectionError::Index(error) => index(context, error, ReadFor::Other, value),
    }
}

/// What the index was read for. The index version a search was reading may
/// have been removed by maintenance in another process; the error does not
/// tell that apart from an index that cannot be read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadFor {
    Search,
    Other,
}

fn index<'a>(
    context: &ErrorContext,
    error: &'a IndexError,
    read: ReadFor,
    value: &Value<'_>,
) -> Shown<'a> {
    match error {
        IndexError::DocumentNotFound => document_not_found(
            context.collection.as_ref().map(|name| name.0.as_str()),
            context.key.as_ref().map(|key| key.0.as_str()),
            value,
        ),
        IndexError::Embedding(error) => embedding(error),
        IndexError::ModelMismatch {
            configured_name,
            configured_dimension,
            indexed,
        } => {
            let configured = match configured_dimension {
                Some(dimension) => format!("{} ({dimension} dimensions)", value(configured_name)),
                None => value(configured_name),
            };
            Shown::new(
                1,
                format!(
                    "the configured embedding model {configured} differs from the one the index's embeddings were made with, {} ({} dimensions)",
                    value(&indexed.name),
                    indexed.dimension
                ),
                &[REBUILD_FIX],
            )
        }
        IndexError::RebuildIncomplete => Shown::new(
            1,
            "a rebuild of the embeddings was stopped while switching over to the new embedding model"
                .to_owned(),
            &[REBUILD_FIX],
        ),
        IndexError::CannotRead { .. } => index_unreadable(read).causes_of(error),
        IndexError::CannotWrite { .. } => {
            Shown::new(1, "the index cannot be written".to_owned(), &[WRITE_FIX]).causes_of(error)
        }
    }
}

fn index_unreadable<'a>(read: ReadFor) -> Shown<'a> {
    let message = "the index cannot be read".to_owned();
    match read {
        ReadFor::Search => Shown::new(1, message, &SEARCH_INDEX_UNREADABLE_FIX),
        ReadFor::Other => Shown::new(1, message, &INDEX_UNREADABLE_FIX),
    }
}

fn query<'a>(context: &ErrorContext, error: &'a QueryError, value: &Value<'_>) -> Shown<'a> {
    // A search has no key in its context; getting a document has one.
    let read = if context.key.is_none() {
        ReadFor::Search
    } else {
        ReadFor::Other
    };
    match error {
        QueryError::EmptyQuery => {
            Shown::new(2, "the query is empty".to_owned(), &[EMPTY_QUERY_FIX])
                .invalid_input("query")
        }
        QueryError::ZeroLimit => {
            Shown::new(2, "the limit is zero".to_owned(), &[ZERO_LIMIT_FIX]).invalid_input("limit")
        }
        QueryError::InvalidChunkRange => Shown::new(
            2,
            "the chunk range is not valid".to_owned(),
            &[CHUNK_RANGE_FIX],
        )
        .invalid_input("end_chunk_index"),
        QueryError::DocumentNotFound => document_not_found(
            context.collection.as_ref().map(|name| name.0.as_str()),
            context.key.as_ref().map(|key| key.0.as_str()),
            value,
        ),
        QueryError::ChunkNotFound => Shown::new(
            1,
            "the chunk does not exist".to_owned(),
            &[CHUNK_NOT_FOUND_FIX],
        )
        .invalid_input("chunk_index"),
        QueryError::Embedding(error) => embedding(error),
        QueryError::Index(error) => index(context, error, read, value),
        QueryError::CannotRead { .. } => index_unreadable(read).causes_of(error),
    }
}

fn embedding(error: &EmbeddingError) -> Shown<'_> {
    match error {
        EmbeddingError::ModelUnavailable { .. } => {
            let message = "the embedding model cannot be used".to_owned();
            let shown = if model_cache_dir_undetermined(error) {
                Shown::new(1, message, &MODEL_CACHE_DIR_FIX)
            } else {
                Shown::new(1, message, &[MODEL_UNAVAILABLE_FIX])
            };
            shown.code(EMBEDDING_MODEL_UNAVAILABLE).causes_of(error)
        }
        EmbeddingError::QueryExceedsContextLength => Shown::new(
            1,
            "the query is longer than the embedding model's context length".to_owned(),
            &QUERY_TOO_LONG_FIX,
        )
        .invalid_input("query"),
    }
}

fn ingest<'a>(context: &ErrorContext, error: &'a IngestError, value: &Value<'_>) -> Shown<'a> {
    let name = context.collection.as_ref().map(|name| name.0.as_str());
    match error {
        IngestError::AttributesMissingOrExtra => Shown::new(
            1,
            "the type-specific attributes are missing or unexpected for the collection's type"
                .to_owned(),
            &[ATTRIBUTES_FIX],
        ),
        IngestError::LocationNotFound { .. } => Shown::new(
            1,
            "the location was not found".to_owned(),
            &LOCATION_NOT_FOUND_FIX,
        )
        .causes_of(error),
        IngestError::LocationTypeMismatch => Shown::new(
            1,
            "the location does not fit the collection type".to_owned(),
            &[LOCATION_TYPE_FIX],
        ),
        IngestError::UpdateNotSupported => update_not_supported(name, value),
        IngestError::PutNotSupported => put_not_supported(name, value),
        IngestError::UnsupportedFileType => unsupported_file_type(None, value),
        IngestError::CannotExtract(format) => match format {
            FormatError::CannotExtract { .. } => Shown::new(
                1,
                "the document's body cannot be extracted".to_owned(),
                &[CANNOT_EXTRACT_FIX],
            )
            .causes_of(format),
        },
        IngestError::SourceUnreadable { .. } => Shown::new(
            1,
            "the source cannot be read".to_owned(),
            &SOURCE_UNREADABLE_FIX,
        )
        .causes_of(error),
        IngestError::Index(error) => index(context, error, ReadFor::Other, value),
    }
}

fn config<'a>(error: &'a ConfigError, value: &Value<'_>) -> Shown<'a> {
    match error {
        ConfigError::UnknownName { name } => Shown::new(
            2,
            format!("there is no setting named \"{}\"", value(name)),
            &[UNKNOWN_SETTING_FIX],
        ),
        ConfigError::InvalidValue { name, accepted } => Shown::new(
            2,
            format!("the value is not valid for the setting {}", name.as_str()),
            &[],
        )
        .with_fix(format!("Give a value the setting accepts: {accepted}.")),
        ConfigError::CannotRead { .. } => Shown::new(
            1,
            "the settings cannot be read".to_owned(),
            &SETTINGS_UNREADABLE_FIX,
        )
        .causes_of(error),
        ConfigError::CannotWrite { .. } => {
            Shown::new(1, "the settings cannot be written".to_owned(), &[WRITE_FIX])
                .causes_of(error)
        }
    }
}

fn write_lock(error: &WriteLockError) -> Shown<'_> {
    match error {
        WriteLockError::InProgress => Shown::new(
            1,
            "another write operation is in progress".to_owned(),
            &IN_PROGRESS_FIX,
        ),
        WriteLockError::Unavailable { .. } => {
            Shown::new(1, "the write lock cannot be taken".to_owned(), &[WRITE_FIX])
                .causes_of(error)
        }
    }
}

fn collection_not_found<'a>(name: Option<&str>, value: &Value<'_>) -> Shown<'a> {
    let shown = Shown::new(
        1,
        match name {
            Some(name) => format!("the collection \"{}\" does not exist", value(name)),
            None => "the collection does not exist".to_owned(),
        },
        &[COLLECTION_NOT_FOUND_FIX],
    )
    .code(COLLECTION_NOT_FOUND);
    match name {
        Some(name) => shown.detail("collection", name),
        None => shown,
    }
}

fn name_invalid<'a>(name: Option<&str>, value: &Value<'_>) -> Shown<'a> {
    Shown::new(
        2,
        match name {
            Some(name) => format!(
                "the collection name \"{}\" does not follow the naming rules",
                value(name)
            ),
            None => "the collection name does not follow the naming rules".to_owned(),
        },
        &[NAME_RULES_FIX],
    )
}

fn update_not_supported<'a>(name: Option<&str>, value: &Value<'_>) -> Shown<'a> {
    Shown::new(
        1,
        format!(
            "the collection {}is of a type that cannot be updated",
            quoted_name(name, value)
        ),
        &[UPDATE_NOT_SUPPORTED_FIX],
    )
}

fn put_not_supported<'a>(name: Option<&str>, value: &Value<'_>) -> Shown<'a> {
    Shown::new(
        1,
        format!(
            "the collection {}is of a type that does not take document content",
            quoted_name(name, value)
        ),
        &[PUT_NOT_SUPPORTED_FIX],
    )
}

/// `"name" ` for a name, nothing without one.
fn quoted_name(name: Option<&str>, value: &Value<'_>) -> String {
    name.map(|name| format!("\"{}\" ", value(name)))
        .unwrap_or_default()
}

fn unsupported_file_type<'a>(file_type: Option<&str>, value: &Value<'_>) -> Shown<'a> {
    Shown::new(
        2,
        match file_type {
            Some(file_type) => format!("the file type \"{}\" is not supported", value(file_type)),
            None => "the file type is not supported".to_owned(),
        },
        &[FILE_TYPE_FIX],
    )
}

fn document_not_found<'a>(
    collection: Option<&str>,
    key: Option<&str>,
    value: &Value<'_>,
) -> Shown<'a> {
    let document = key.map(|key| format!(" \"{}\"", value(key)));
    let in_collection =
        collection.map(|collection| format!(" in the collection \"{}\"", value(collection)));
    let message = format!(
        "the document{} does not exist{}",
        document.unwrap_or_default(),
        in_collection.unwrap_or_default()
    );
    let mut shown = Shown::new(1, message, &[DOCUMENT_NOT_FOUND_FIX]).code(DOCUMENT_NOT_FOUND);
    if let Some(collection) = collection {
        shown = shown.detail("collection", collection);
    }
    if let Some(key) = key {
        shown = shown.detail("key", key);
    }
    shown
}

const INTERNAL: &str = "internal";
const INVALID_INPUT: &str = "invalid_input";
const COLLECTION_NOT_FOUND: &str = "collection_not_found";
const DOCUMENT_NOT_FOUND: &str = "document_not_found";
const OUTPUT_TOO_LARGE: &str = "output_too_large";
const EMBEDDING_MODEL_UNAVAILABLE: &str = "embedding_model_unavailable";

const PANIC_MESSAGE: &str = "an unexpected failure occurred, which is a bug in mdya";
const PANIC_FIX: &str = "Report the bug at https://github.com/yoshihirosuzuki/mdya/issues, with what you ran, the output of mdya --version, and the messages mdya printed on stderr.";

const CONFIRMATION_FIX: &str =
    "Run the command in a terminal and answer the confirmation, or add --yes.";

const TOO_LARGE_FIX: [&str; 2] = [
    "Get the document in parts by giving a range of chunks.",
    "On the command line, you can also remove the limit with --no-size-limit.",
];

const ALREADY_EXISTS_FIX: &str = "Use a different collection name.";
const NAME_RULES_FIX: &str =
    "Use a name of 1 to 64 characters made of ASCII letters, digits, \"-\", and \"_\".";
const ATTRIBUTES_FIX: &str = "Give the type-specific attributes the collection type takes: a location for a directory or archive collection, and none for a raw collection.";
const LOCATION_NOT_FOUND_FIX: [&str; 3] = [
    "Give a location that exists.",
    "If mdya could not read whether the location is a directory or a file, fix the permissions so that mdya can read it, or give a location mdya can read.",
    "If the location could not be made absolute, give an absolute path, or move to a working directory that mdya can read, and run the command again.",
];
const LOCATION_TYPE_FIX: &str = "Give a directory for a directory collection, and a .zip, .tar, .tar.gz, or .tgz file for an archive collection.";
const COLLECTION_NOT_FOUND_FIX: &str = "Check the collection name in the list of collections (mdya collection list, or the list_collections tool).";
const NOTHING_TO_CHANGE_FIX: &str = "Give a description (--description or --no-description), or, for a directory or archive collection, a location to change.";
const UPDATE_NOT_SUPPORTED_FIX: &str = "Change the documents of a raw collection with mdya put.";
const PUT_NOT_SUPPORTED_FIX: &str = "Change the documents of a directory or archive collection by changing its source, then run mdya update.";
const FILE_TYPE_FIX: &str = "Give --file-type text/markdown (or markdown, md) for Markdown, or application/pdf (or pdf) for PDF.";
const DOCUMENT_NOT_FOUND_FIX: &str = "Check the key, for example in the search results.";
const EMPTY_QUERY_FIX: &str = "Give a query that is not just white space.";
const ZERO_LIMIT_FIX: &str = "Give a limit of 1 or more.";
const CHUNK_NOT_FOUND_FIX: &str = "Give a start chunk index (--chunk-index, or chunk_index in MCP) smaller than the document's chunk count (chunk_count in the search results).";
const CHUNK_RANGE_FIX: &str = "Give the start chunk index (--chunk-index, or chunk_index in MCP), and an end chunk index (--end-chunk-index, or end_chunk_index in MCP) that is not smaller than the start.";
const UNKNOWN_SETTING_FIX: &str = "Check the setting names with mdya config list.";
const LOG_LEVEL_FIX: &str = "Give --log-level a level (error, warn, info, debug, or trace), or levels for each part of mdya or of the libraries it uses, such as mdya=debug.";
const USAGE_FIX: &str = "See mdya --help, or run the command with --help.";

const CONTENT_UNREADABLE_FIX: &str =
    "Make the content readable (for example, fix the permissions), then run mdya put again.";
const CANNOT_EXTRACT_FIX: &str = "Give content that is not broken and is of the given file type (for example, Markdown in UTF-8), then run mdya put again.";

const SOURCE_UNREADABLE_FIX: [&str; 2] = [
    "Make the source readable (connect the disk, fix the location, and so on), then run mdya update again.",
    "If the source moved, change the location with mdya collection set.",
];

const MODEL_UNAVAILABLE_FIX: &str = "Fix the cause shown above (connect to the network, set the Hugging Face token in the environment variable HF_TOKEN, start Ollama, and so on), then run the operation again.";

const SETTINGS_UNREADABLE_FIX: [&str; 3] = [
    "If you edited the settings file directly, undo the edit or fix the file so that it can be read.",
    "Otherwise, or if that does not fix it, delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.",
    "In that case, give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.",
];
const INDEX_UNREADABLE_FIX: [&str; 2] = [
    "Delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.",
    "Then give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.",
];
const SEARCH_INDEX_UNREADABLE_FIX: [&str; 3] = [
    "If the index was being optimized while you searched, search again, as the index is not broken then.",
    "If searching again fails in the same way, delete the settings and index directory (the one given with --data-dir, or .mdya in the home directory by default), create the collections again, and ingest their documents again.",
    "In that case, give the documents of raw collections again with mdya put, and set the settings you had changed again, as they return to their defaults.",
];

const MEMORY_LIMIT_FIX: &str = "Raise the memory limit (mdya config set resources.memory_limit_mb) or lower the parallelism (mdya config set resources.embedding_parallelism), then run the operation again.";

const ADDRESS_FIX: &str = "Give a different address or port with --address, or, if another process is using the address, stop it.";

const IN_PROGRESS_FIX: [&str; 2] = [
    "Wait until the other write operation ends, then run the operation again.",
    "A write operation that stopped partway (when interrupted, for example) does not keep the next one from starting.",
];

const QUERY_TOO_LONG_FIX: [&str; 3] = [
    "Shorten the query, or use full-text search (--mode fts, or mode fts in MCP).",
    "To search with a longer query, set an embedding model with a longer context length (mdya config set embedding.model) and run mdya embedding rebuild.",
    "For the MCP server, start it again after changing the setting.",
];

const WRITE_FIX: &str = "Fix the cause shown above (free up disk space, fix the write permissions, and so on), then run the operation again.";

const REBUILD_FIX: &str =
    "Rebuild the embeddings with mdya embedding rebuild, then run the operation again.";

const DATA_DIR_FIX: [&str; 2] = [
    "Give an absolute path with --data-dir.",
    "If the working directory cannot be read, you can also move to a working directory that mdya can read, and run the command again.",
];

const MCP_START_FIX: &str =
    "Check the MCP client's settings, then start the MCP server again from the MCP client.";

const UNKNOWN_TYPE_FIX: &str = "Use a version of mdya that can handle the collection type, or delete the collection with mdya collection remove.";
