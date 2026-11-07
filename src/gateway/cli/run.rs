//! Carries out a command line operation: asks for confirmation, reads the
//! content to put, shows the progress, writes the result to stdout and the
//! messages and errors to stderr, and decides the exit code. What it writes
//! is what `formatter` made, unchanged.

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde::Serialize;

use crate::index::{DimensionResult, IndexStatus};

use super::super::error_report::GatewayError;
use super::super::formatter::{self, Message, ModelUnknown, Prompt, UnsafeNumber};
use super::super::log_progress::{Console, ProgressKind};
use super::super::operations::{
    ChangeCollectionInput, ChangeSettingInput, Confirmation, ContentReader, CreateCollectionInput,
    DeleteCollectionInput, DeleteCollectionOutcome, DeleteDocumentInput, DeleteDocumentOutcome,
    GatewayProgressSink, GetDocumentInput, MODEL_CACHE_DIR_FIX, OperationError, Operations,
    OptimizeInput, OutcomeAnomaly, PutInput, RebuildInput, SearchInput, SettingChangeOutcome,
    UpdateAllInput, UpdateInput, model_cache_dir_undetermined,
};
use super::{CliError, CliOutput, ContentSource, Operation, Output};

#[cfg(test)]
mod tests;

/// Why an operation stopped writing.
#[derive(Debug)]
pub(super) enum Stop {
    /// An error, to be explained on stderr. Boxed, as the errors are large.
    Error(Box<GatewayError>),
    /// What reads stdout closed it: nothing more is written, and the exit code
    /// is 0.
    StdoutClosed,
}

impl From<OperationError> for Stop {
    fn from(error: OperationError) -> Self {
        Stop::Error(Box::new(GatewayError::Operation(error)))
    }
}

/// Writes `text` to stdout and flushes it. A closed stdout stops the
/// operation quietly; any other failure is an error.
pub(super) fn write_stdout(stdout: &mut dyn Write, text: &str) -> Result<(), Stop> {
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(Stop::StdoutClosed),
        Err(source) => Err(Stop::Error(Box::new(GatewayError::Cli(
            CliError::StdoutUnwritable { source },
        )))),
    }
}

/// Explains `error` on stderr and returns its exit code.
pub(super) fn report(error: &GatewayError, output: &CliOutput) -> u8 {
    let explanation = output.error_report.explanation(error);
    output.console.write_stderr(&format!(
        "{}\n",
        formatter::human_explanation(&explanation, output.stderr_color)
    ));
    output.error_report.exit_code(error)
}

/// The exit code once the operation has stopped: 0 for a closed stdout, and
/// otherwise the error's, after its explanation (and, for an update that
/// stopped, the counts up to where it stopped).
pub(super) fn stopped(stop: Stop, output: &CliOutput) -> u8 {
    match stop {
        Stop::StdoutClosed => 0,
        Stop::Error(error) => {
            let code = report(&error, output);
            if let GatewayError::Operation(OperationError::UpdateStopped { counts, .. }) = &*error {
                message(output, &Message::UpdateStopped(counts));
            }
            code
        }
    }
}

fn message(output: &CliOutput, message: &Message) {
    output
        .console
        .write_stderr(&formatter::human_message(message));
}

/// What `formatter` made. A number that it refuses to write is a bug in
/// mdya: an unexpected failure, which ends the process as a panic does.
#[expect(
    clippy::panic,
    reason = "a number that the output cannot hold is an unexpected failure"
)]
fn made<T>(made: Result<T, UnsafeNumber>) -> T {
    match made {
        Ok(made) => made,
        Err(error) => panic!("{error}"),
    }
}

/// The result as the output format asks: JSON on one line, or the
/// human-readable output.
fn result_text<T: Serialize>(
    result: &T,
    output: Output,
    human: impl FnOnce(&T) -> String,
) -> String {
    match output {
        Output::Json => format!("{}\n", made(formatter::cli_json(result))),
        Output::Human => human(result),
    }
}

pub(super) struct Run<'a> {
    pub(super) operations: Arc<dyn Operations>,
    pub(super) output: &'a CliOutput,
    pub(super) stdin: Box<dyn BufRead + Send>,
    pub(super) stdout: Box<dyn Write + Send>,
    /// The command line's output size limit in bytes. `None` applies none.
    pub(super) size_limit: Option<u64>,
}

impl Run<'_> {
    /// Carries out the operation. Returns the exit code when it ended without
    /// an error.
    pub(super) async fn operation(self, operation: Operation) -> Result<u8, Stop> {
        let Run {
            operations,
            output,
            stdin,
            mut stdout,
            size_limit,
        } = self;
        let report = output.error_report.as_ref();
        let code = |anomalies: Vec<OutcomeAnomaly>| report.outcome_exit_code(&anomalies);
        match operation {
            Operation::CreateCollection {
                name,
                collection_type,
                description,
                attributes,
            } => {
                let info = operations
                    .create_collection(CreateCollectionInput {
                        name,
                        collection_type,
                        description,
                        attributes,
                    })
                    .await?;
                message(output, &Message::CollectionCreated(&info));
                Ok(0)
            }
            Operation::DeleteCollection {
                name,
                yes,
                optimize,
            } => {
                let prompt = formatter::human_prompt(&Prompt::RemoveCollection(&name));
                let outcome = operations
                    .delete_collection(DeleteCollectionInput {
                        name: name.clone(),
                        confirmation: confirmation(yes, prompt, output, stdin),
                        optimize,
                    })
                    .await?;
                match &outcome {
                    DeleteCollectionOutcome::Deleted { collection } => {
                        message(output, &Message::CollectionRemoved(collection));
                    }
                    DeleteCollectionOutcome::Declined => {
                        message(output, &Message::CollectionNotRemoved(&name));
                    }
                }
                Ok(code(outcome.anomalies()))
            }
            Operation::ListCollections { output: format } => {
                let list = operations.list_collections().await?;
                let text = result_text(&list, format, formatter::human_collection_list);
                write_stdout(&mut stdout, &text)?;
                Ok(0)
            }
            Operation::ChangeCollection { name, change } => {
                let info = operations
                    .change_collection(ChangeCollectionInput { name, change })
                    .await?;
                message(output, &Message::CollectionChanged(&info));
                Ok(0)
            }
            Operation::Update {
                collection,
                overwrite,
                optimize,
            } => {
                let display = output.console.start_progress(ProgressKind::Update);
                let progress: Arc<dyn GatewayProgressSink> = display.clone();
                let result = operations
                    .update_collection(UpdateInput {
                        collection: collection.clone(),
                        overwrite,
                        progress,
                        optimize,
                    })
                    .await;
                display.finish();
                let outcome = result?;
                message(
                    output,
                    &Message::CollectionUpdated(&collection, &outcome.counts),
                );
                Ok(code(outcome.anomalies()))
            }
            Operation::UpdateAll {
                overwrite,
                optimize,
            } => {
                let display = output.console.start_progress(ProgressKind::Update);
                let progress: Arc<dyn GatewayProgressSink> = display.clone();
                let result = operations
                    .update_all(UpdateAllInput {
                        overwrite,
                        progress,
                        optimize,
                    })
                    .await;
                display.finish();
                let outcome = result?;
                message(
                    output,
                    &Message::CollectionsUpdated(outcome.updated_collections, &outcome.counts),
                );
                Ok(code(outcome.anomalies()))
            }
            Operation::Put {
                collection,
                key,
                media_type,
                content,
                overwrite,
                optimize,
            } => {
                let outcome = operations
                    .put_document(PutInput {
                        collection,
                        key,
                        media_type,
                        content: content_reader(content, stdin),
                        overwrite,
                        optimize,
                    })
                    .await?;
                message(
                    output,
                    &Message::DocumentPut(&outcome.collection, &outcome.key, outcome.outcome),
                );
                Ok(code(outcome.anomalies()))
            }
            Operation::DeleteDocument {
                collection,
                key,
                yes,
                optimize,
            } => {
                let prompt = formatter::human_prompt(&Prompt::RemoveDocument(&collection, &key));
                let outcome = operations
                    .delete_document(DeleteDocumentInput {
                        collection: collection.clone(),
                        key: key.clone(),
                        confirmation: confirmation(yes, prompt, output, stdin),
                        optimize,
                    })
                    .await?;
                match &outcome {
                    DeleteDocumentOutcome::Deleted {
                        collection,
                        key,
                        supports_update,
                    } => message(
                        output,
                        &Message::DocumentRemoved {
                            collection,
                            key,
                            may_come_back: *supports_update,
                        },
                    ),
                    DeleteDocumentOutcome::Declined => {
                        message(output, &Message::DocumentNotRemoved(&collection, &key));
                    }
                }
                Ok(code(outcome.anomalies()))
            }
            Operation::Search {
                query,
                mode,
                collections,
                limit,
                unit,
                output: format,
            } => {
                let result = operations
                    .search(SearchInput {
                        query,
                        mode,
                        collections,
                        limit,
                        unit,
                    })
                    .await?;
                let text = result_text(&result, format, |result| {
                    made(formatter::human_search(result))
                });
                write_stdout(&mut stdout, &text)?;
                Ok(0)
            }
            Operation::Get {
                collection,
                key,
                range,
                size_limit_off,
            } => {
                let document = operations
                    .get_document(GetDocumentInput {
                        collection,
                        key,
                        range,
                        size_limit: if size_limit_off { None } else { size_limit },
                    })
                    .await?;
                write_stdout(&mut stdout, formatter::human_document_text(&document))?;
                Ok(0)
            }
            Operation::Status { output: format } => {
                let status = operations.status().await?;
                let text = result_text(&status, format, formatter::human_status);
                write_stdout(&mut stdout, &text)?;
                Ok(0)
            }
            Operation::ListSettings { output: format } => {
                let settings = operations.list_settings().await?;
                let text = result_text(&settings, format, formatter::human_settings);
                write_stdout(&mut stdout, &text)?;
                Ok(0)
            }
            Operation::SetSetting { name, value } => {
                let outcome = operations
                    .change_setting(ChangeSettingInput::Set { name, value })
                    .await?;
                setting_changed(&outcome, output);
                Ok(code(outcome.anomalies()))
            }
            Operation::ResetSetting { name } => {
                let outcome = operations
                    .change_setting(ChangeSettingInput::Reset { name })
                    .await?;
                setting_changed(&outcome, output);
                Ok(code(outcome.anomalies()))
            }
            Operation::RebuildEmbeddings { force, optimize } => {
                let display = output
                    .console
                    .start_progress(ProgressKind::RebuildEmbeddings);
                let progress: Arc<dyn GatewayProgressSink> = display.clone();
                let result = operations
                    .rebuild_embeddings(RebuildInput {
                        include_configured_model: force,
                        progress,
                        optimize,
                    })
                    .await;
                display.finish();
                let outcome = result?;
                if outcome.rebuilt_chunks == 0 && !force {
                    message(output, &Message::NothingRebuilt);
                } else {
                    message(output, &Message::EmbeddingsRebuilt(outcome.rebuilt_chunks));
                }
                Ok(code(outcome.anomalies()))
            }
            Operation::Optimize => {
                let display = output.console.start_progress(ProgressKind::Optimize);
                let progress: Arc<dyn GatewayProgressSink> = display.clone();
                let result = operations.optimize(OptimizeInput { progress }).await;
                display.finish();
                result?;
                message(output, &Message::IndexMaintained);
                Ok(0)
            }
        }
    }
}

/// The messages after a setting changed: the change, and, after the
/// embedding model changed, whether the index's embeddings were made with
/// another model, or that it is not known.
fn setting_changed(outcome: &SettingChangeOutcome, output: &CliOutput) {
    message(output, &Message::SettingChanged(&outcome.change));
    match &outcome.index_status {
        None => {}
        Some(Ok(status)) => index_model(status, output),
        Some(Err(error)) => {
            let cause = error_chain(error);
            message(
                output,
                &Message::IndexModelUnknown {
                    why: ModelUnknown::StatusUnreadable,
                    cause: &cause,
                    fix: &[],
                },
            );
        }
    }
}

fn index_model(status: &IndexStatus, output: &CliOutput) {
    for model in &status.models {
        match model.same_as_compared {
            Some(true) => {}
            Some(false) => message(output, &Message::IndexModelDiffers(&model.model.name)),
            None => {
                let DimensionResult::Unavailable(error) = &status.compared_dimension else {
                    continue;
                };
                let cause = error_chain(error);
                let fix: &[&str] = if model_cache_dir_undetermined(error) {
                    &MODEL_CACHE_DIR_FIX
                } else {
                    &[]
                };
                message(
                    output,
                    &Message::IndexModelUnknown {
                        why: ModelUnknown::DimensionUnchecked,
                        cause: &cause,
                        fix,
                    },
                );
            }
        }
    }
}

/// `error` and its causes, joined by `: `.
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(error), |error| error.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// What asks for confirmation: always yes with `--yes`, and otherwise the
/// user, on the terminal.
fn confirmation(
    yes: bool,
    prompt: String,
    output: &CliOutput,
    stdin: Box<dyn BufRead + Send>,
) -> Box<dyn Confirmation> {
    if yes {
        return Box::new(Confirmed);
    }
    Box::new(TerminalConfirmation {
        prompt,
        console: Arc::clone(&output.console),
        stdin: Mutex::new(Some(stdin)),
    })
}

struct Confirmed;

#[async_trait]
impl Confirmation for Confirmed {
    async fn confirm(&self) -> bool {
        true
    }
}

/// Writes the prompt to stderr and reads one line of answer from stdin. `y`
/// or `yes`, in any case, confirms; anything else, an empty line, or no
/// line declines. Reading waits for the user without giving way to other
/// tasks, as nothing else of the operation goes on while it asks.
struct TerminalConfirmation {
    prompt: String,
    console: Arc<dyn Console>,
    stdin: Mutex<Option<Box<dyn BufRead + Send>>>,
}

#[async_trait]
impl Confirmation for TerminalConfirmation {
    async fn confirm(&self) -> bool {
        self.console.write_stderr(&self.prompt);
        let Some(mut stdin) = self
            .stdin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return false;
        };
        let mut answer = String::new();
        if stdin.read_line(&mut answer).is_err() {
            return false;
        }
        let answer = answer.trim();
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }
}

fn content_reader(
    content: ContentSource,
    stdin: Box<dyn BufRead + Send>,
) -> Box<dyn ContentReader> {
    match content {
        ContentSource::File(path) => Box::new(FileContent(path)),
        ContentSource::Stdin => Box::new(StdinContent(Mutex::new(Some(stdin)))),
    }
}

struct FileContent(PathBuf);

#[async_trait]
impl ContentReader for FileContent {
    async fn read(&self) -> io::Result<Vec<u8>> {
        tokio::fs::read(&self.0).await
    }
}

/// Reads all of stdin, once. Reading waits without giving way to other
/// tasks, as the operation needs the content before it goes on.
struct StdinContent(Mutex<Option<Box<dyn BufRead + Send>>>);

#[async_trait]
impl ContentReader for StdinContent {
    async fn read(&self) -> io::Result<Vec<u8>> {
        let Some(mut stdin) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return Err(io::Error::other("stdin was already read"));
        };
        let mut content = Vec::new();
        stdin.read_to_end(&mut content)?;
        Ok(content)
    }
}
