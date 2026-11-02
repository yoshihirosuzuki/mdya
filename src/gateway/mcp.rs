//! The MCP server, over stdio or HTTP.

#![expect(dead_code, reason = "the rest of `gateway` does not use these yet")]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::types::BoxError;

use super::error_report::ErrorReport;
use super::escape::Escape;
use super::operations::Operations;

/// What the MCP server works with.
#[derive(Clone)]
pub(crate) struct McpServerParts {
    pub(crate) operations: Arc<dyn Operations>,
    /// Whether to show mdya's version.
    pub(crate) show_version: bool,
    /// The MCP output size limit in bytes. `None` applies none.
    pub(crate) size_limit: Option<u64>,
    pub(crate) escape: Arc<dyn Escape>,
    pub(crate) error_report: Arc<dyn ErrorReport>,
}

/// Runs the MCP server over the given reader and writer. Returns when the
/// client closes the connection, after the tool calls in progress end.
pub(crate) async fn serve_stdio<R, W>(
    parts: McpServerParts,
    reader: R,
    writer: W,
) -> Result<(), McpError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let _ = (parts, reader, writer);
    todo!()
}

/// Starts listening for the MCP server (HTTP). `host` is in the form of
/// [`super::cli::McpTransport::Http`]; a host name listens on every address
/// it resolves to.
pub(crate) async fn listen_http(host: &str, port: u16) -> Result<McpHttpListener, McpError> {
    let _ = (host, port);
    todo!()
}

/// Listening, not yet serving.
#[derive(Debug)]
pub(crate) struct McpHttpListener {}

impl McpHttpListener {
    /// The addresses it listens on. With port 0, the port the OS chose.
    pub(crate) fn local_addrs(&self) -> &[SocketAddr] {
        todo!()
    }

    /// Runs the MCP server (HTTP). Returns only when it fails; otherwise it
    /// runs until the process ends. A test stops it by aborting its task.
    pub(crate) async fn serve(self, parts: McpServerParts) -> Result<(), McpError> {
        let _ = parts;
        todo!()
    }
}

/// An error that only the MCP server finds.
#[derive(Debug, thiserror::Error)]
pub(crate) enum McpError {
    /// `host` and `port` are what `listen_http` received. `source` says why:
    /// the name did not resolve (including to nothing), or listening failed.
    #[error("cannot listen on {host}:{port}")]
    AddressUnavailable {
        host: String,
        port: u16,
        source: std::io::Error,
    },
    /// Reading or writing stdio, the initialization exchange, or the HTTP
    /// server failed.
    #[error("the MCP connection failed")]
    Transport { source: BoxError },
    /// A tool's input does not fit its input schema. `input` is the name of
    /// the input; `detail` says what is wrong.
    #[error("the tool input {input} is not valid: {detail}")]
    InvalidToolInput { input: String, detail: String },
}
