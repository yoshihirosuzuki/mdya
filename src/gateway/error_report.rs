//! Decides, for every error, the command line's exit code, the MCP error, and
//! the explanation shown to the user, in one place.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::sync::Arc;

use serde::Serialize;

use super::cli::CliError;
use super::entry::EntryError;
use super::escape::Escape;
use super::mcp::McpError;
use super::operations::{OperationError, OutcomeAnomaly};

#[cfg(test)]
pub(crate) mod test_double;

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
    /// fix it, laid out in one fixed form.
    fn explain(&self, error: &GatewayError, style: ExplainStyle) -> String;

    /// The MCP error. Its `message` is the explanation, without color and
    /// without replacing control characters or the characters that change
    /// the direction of text.
    fn mcp_error(&self, error: &GatewayError) -> McpErrorBody;

    /// The explanation of an unexpected failure, asking for a bug report.
    fn explain_panic(&self, style: ExplainStyle) -> String;

    /// The MCP error for a tool call that panicked (code `internal`).
    fn mcp_panic_error(&self) -> McpErrorBody;
}

/// Creates the real one. It replaces control characters and the characters
/// that change the direction of text with `escape`.
pub(crate) fn error_report(escape: Arc<dyn Escape>) -> Arc<dyn ErrorReport> {
    let _ = escape;
    todo!()
}
