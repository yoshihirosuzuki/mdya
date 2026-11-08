//! The MCP server, over stdio or HTTP. It passes what `formatter` makes to
//! rmcp without changing it, and leaves turning it into JSON, and reading and
//! writing the exchange, to rmcp.

use std::net::SocketAddr;
use std::sync::Arc;

use rmcp::service::QuitReason;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::task::TaskTracker;

use crate::types::BoxError;

use super::error_report::ErrorReport;
use super::log_progress::Console;
use super::operations::Operations;

mod http;
mod server;
#[cfg(test)]
mod tests;
mod tools;

/// What the MCP server works with.
#[derive(Clone)]
pub(crate) struct McpServerParts {
    pub(crate) operations: Arc<dyn Operations>,
    /// Whether to show mdya's version.
    pub(crate) show_version: bool,
    /// The MCP output size limit in bytes. `None` applies none.
    pub(crate) size_limit: Option<u64>,
    pub(crate) error_report: Arc<dyn ErrorReport>,
    /// Where the name of a tool whose call panicked is written, after what
    /// the panic hook wrote.
    pub(crate) console: Arc<dyn Console>,
}

/// Runs the MCP server over the given reader and writer. Once the exchange
/// started, it returns `Ok`, after the tool calls in progress end, when the
/// client closes the connection or the reader cannot be read. A failure to
/// write after the exchange started either lets it go on or ends it in the
/// same way. Returns [`McpError::Transport`] when the exchange could not be
/// started, including when the client closed the connection before it
/// started. A panic outside the tool calls, in a task that rmcp runs for the
/// exchange, leaves the exchange unable to go on: it is passed on as a panic,
/// as an unexpected failure outside the tool calls. `reader` and `writer`
/// are handed to rmcp as they are.
///
/// The future cannot be spawned as a task: the compiler cannot prove that
/// the borrows that rmcp's futures for the exchange hold are `Send` for every
/// lifetime. Await it where it is made, as `block_on` does.
pub(crate) async fn serve_stdio<R, W>(
    parts: McpServerParts,
    reader: R,
    writer: W,
) -> Result<(), McpError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let calls = TaskTracker::new();
    let server = server::McpServer::new(parts, calls.clone());
    let ended = match rmcp::serve_server(server, (reader, writer)).await {
        Ok(running) => Ok(running.waiting().await),
        Err(error) => Err(error),
    };
    calls.close();
    calls.wait().await;
    match ended {
        Err(error) => Err(McpError::Transport {
            source: Box::new(error),
        }),
        // The task that rmcp runs the exchange in panicked, or one that it
        // sends a request to the client in did.
        Ok(Err(error) | Ok(QuitReason::JoinError(error))) if error.is_panic() => {
            std::panic::resume_unwind(error.into_panic())
        }
        // The exchange ended (the client closed the connection, or reading or
        // writing failed), or its task was cancelled because the runtime is
        // shutting down: the exchange is over either way.
        Ok(_) => Ok(()),
    }
}

/// Starts listening for the MCP server (HTTP). `host` is in the form of
/// [`super::cli::McpTransport::Http`]; a host name listens on every address
/// it resolves to.
pub(crate) async fn listen_http(host: &str, port: u16) -> Result<McpHttpListener, McpError> {
    http::listen(host, port).await
}

/// Listening, not yet serving.
#[derive(Debug)]
pub(crate) struct McpHttpListener {
    listeners: Vec<TcpListener>,
    addrs: Vec<SocketAddr>,
}

impl McpHttpListener {
    /// The addresses it listens on. With port 0, the port the OS chose.
    pub(crate) fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Runs the MCP server (HTTP) until the process ends. A failure in
    /// accepting a connection or in the exchange with one MCP client does
    /// not stop it and is not returned. A test stops it by aborting its task.
    pub(crate) async fn serve(self, parts: McpServerParts) {
        http::serve(self.listeners, parts).await;
    }
}

/// An error that only the MCP server finds.
#[derive(Debug, thiserror::Error)]
pub(crate) enum McpError {
    /// `host` and `port` are what `listen_http` received. `source` says why:
    /// the name did not resolve (including to nothing), or listening failed.
    #[error("cannot listen on {}", socket_address(.host, *.port))]
    AddressUnavailable {
        host: String,
        port: u16,
        source: std::io::Error,
    },
    /// The MCP server (stdio) could not start the exchange with the MCP
    /// client: for example, the client closed stdin or sent something other
    /// than a request first, or the reply could not be written to stdout. Not
    /// returned for a failure after the exchange started, nor by the MCP
    /// server (HTTP), which keeps running.
    #[error("the connection with the MCP client cannot be started")]
    Transport { source: BoxError },
    /// A tool's input does not fit its input schema. `input` is the name of
    /// the input; `detail` says what is wrong.
    #[error("the tool input {input} is not valid: {detail}")]
    InvalidToolInput { input: String, detail: String },
    /// The search tool's `limit` is 0. A search limit cannot be 0, so the
    /// server turns it away before it makes the search's input.
    #[error("the limit is zero")]
    ZeroLimit,
}

/// `host` and `port` as one address, with an IPv6 address (which `host`
/// holds without brackets) in brackets: `127.0.0.1:8000`, `[::1]:8000`.
pub(crate) fn socket_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        return format!("[{host}]:{port}");
    }
    format!("{host}:{port}")
}
