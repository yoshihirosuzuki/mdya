//! What rmcp calls: the server's information and instructions, the list of
//! tools, and the tool calls.

use rmcp::ErrorData;
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use serde::Serialize;
use serde_json::json;
use tokio_util::task::TaskTracker;

use super::super::error_report::GatewayError;
use super::super::formatter::{self, McpToolError, UnsafeNumber};
use super::super::operations::{OperationError, VersionOutput, version};
use super::tools::{self, Tool};
use super::{McpError, McpServerParts};

/// The instructions the server returns when the client initializes it.
pub(super) const INSTRUCTIONS: &str = "mdya searches the user's own documents, Markdown \
     and PDF, that mdya holds on this computer. Use it to find and read what the user's \
     notes and documents say. Call list_collections to see the collections, search to \
     find documents or chunks that match a query, and get_document with a hit's \
     collection, key, and chunk_index to read the text that the chunk covers.";

/// The MCP server for one exchange (stdio), or for one session or one request
/// (HTTP).
#[derive(Clone)]
pub(super) struct McpServer {
    parts: McpServerParts,
    /// The tool calls in progress, so that the server can wait for them.
    calls: TaskTracker,
}

impl McpServer {
    pub(super) fn new(parts: McpServerParts, calls: TaskTracker) -> Self {
        Self { parts, calls }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "mdya",
                server_version(self.parts.show_version),
            ))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::definitions()))
    }

    /// Runs the tool in a task of its own, so that it runs to the end even
    /// when the client gives up on the call, and so that a panic, as any
    /// other unexpected failure in the call, fails only this call.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // A fixed message, without the name the client sent: what the server
        // answers with is either what mdya decided or what `formatter` made.
        let Some(tool) = Tool::named(&request.name) else {
            return Err(ErrorData::invalid_params(
                "there is no tool of this name",
                None,
            ));
        };
        let parts = self.parts.clone();
        let call = self
            .calls
            .spawn(async move { call(tool, request.arguments, &parts).await });
        let result = match call.await {
            Ok(result) => result,
            // A panic, or the runtime shutting down under the call: either way
            // an unexpected failure.
            Err(_) => unexpected_failure(&self.parts),
        };
        Ok(result.into())
    }
}

/// Reads the tool's input, runs the operation, and turns what it returns
/// into the tool's result.
async fn call(
    tool: Tool,
    arguments: Option<rmcp::model::JsonObject>,
    parts: &McpServerParts,
) -> CallToolResult {
    let arguments = arguments.unwrap_or_default();
    let operations = &parts.operations;
    match tool {
        Tool::ListCollections => match tools::no_input(&arguments) {
            Ok(()) => answer(operations.list_collections().await, parts),
            Err(error) => invalid_input(error, parts),
        },
        Tool::Search => match tools::search_input(&arguments) {
            Ok(input) => answer(operations.search(input).await, parts),
            Err(error) => invalid_input(error, parts),
        },
        Tool::GetDocument => match tools::get_document_input(&arguments, parts.size_limit) {
            Ok(input) => answer(operations.get_document(input).await, parts),
            Err(error) => invalid_input(error, parts),
        },
        Tool::GetVersion => match tools::no_input(&arguments) {
            Ok(()) => success(&tool_version(parts.show_version)),
            Err(error) => invalid_input(error, parts),
        },
    }
}

/// The operation's result, or its error as a tool error.
fn answer<T: Serialize>(
    result: Result<T, OperationError>,
    parts: &McpServerParts,
) -> CallToolResult {
    match result {
        Ok(output) => success(&output),
        Err(error) => tool_error(&GatewayError::Operation(error), parts),
    }
}

fn invalid_input(error: McpError, parts: &McpServerParts) -> CallToolResult {
    tool_error(&GatewayError::Mcp(error), parts)
}

/// The result, as `formatter` makes it, as structured content, and the same
/// JSON as text.
fn success<T: Serialize>(output: &T) -> CallToolResult {
    CallToolResult::structured(made(formatter::mcp_tool_result(output)))
}

/// The error as `error_report` shows it, made by `formatter`.
fn tool_error(error: &GatewayError, parts: &McpServerParts) -> CallToolResult {
    let report = &parts.error_report;
    error_result(made(formatter::mcp_tool_error(
        report.mcp_error(error),
        &report.explanation(error),
    )))
}

/// What `formatter` made. A number that it refuses to write is a bug in
/// mdya, so the call panics, and fails as any other unexpected failure in a
/// tool call does.
#[expect(
    clippy::panic,
    reason = "a number that JSON cannot hold safely is an unexpected failure, which \
              fails only this tool call"
)]
fn made<T>(made: Result<T, UnsafeNumber>) -> T {
    match made {
        Ok(made) => made,
        Err(error) => panic!("{error}"),
    }
}

/// The tool error for a call that failed unexpectedly (code `internal`),
/// which asks for a bug report. Called outside the call's task, after it
/// failed.
#[expect(
    clippy::expect_used,
    reason = "the error of an unexpected failure has no details, so it holds no \
              number for `formatter` to refuse"
)]
fn unexpected_failure(parts: &McpServerParts) -> CallToolResult {
    let report = &parts.error_report;
    let error = formatter::mcp_tool_error(report.mcp_panic_error(), &report.panic_explanation())
        .expect("the error of an unexpected failure holds no number");
    error_result(error)
}

/// The error as structured content, and the same JSON as text: an object
/// with its `code`, `message`, and `details`.
fn error_result(error: McpToolError) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "code": error.code,
        "message": error.message,
        "details": error.details,
    }))
}

/// The version that the `get_version` tool returns: `mdya 1.2.3`, or only
/// `mdya` when the version is not to be shown.
fn tool_version(show_version: bool) -> VersionOutput {
    if show_version {
        return version();
    }
    VersionOutput {
        version: "mdya".to_owned(),
    }
}

/// The version in the server's information: `1.2.3`, or `mdya` when the
/// version is not to be shown.
fn server_version(show_version: bool) -> String {
    let shown = tool_version(show_version).version;
    match shown.strip_prefix("mdya ") {
        Some(number) => number.to_owned(),
        None => shown,
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::success;

    /// A result that the operations' result types would never hold.
    #[derive(Serialize)]
    struct NotFinite {
        score: f64,
    }

    // In a tool call, the panic fails only that call (see the stdio tests).
    #[test]
    #[should_panic(expected = "the number NaN is not finite")]
    fn a_result_with_a_number_that_json_cannot_hold_is_an_unexpected_failure() {
        success(&NotFinite { score: f64::NAN });
    }
}
