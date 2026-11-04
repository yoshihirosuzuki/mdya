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

/// Runs the MCP server over the given reader and writer. Once the exchange
/// started, it returns `Ok`, after the tool calls in progress end, when the
/// client closes the connection or the reader cannot be read. A failure to
/// write after the exchange started either lets it go on or ends it in the
/// same way. Returns [`McpError::Transport`] when the exchange could not be
/// started, including when the client closed the connection before it
/// started.
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

    /// Runs the MCP server (HTTP) until the process ends. A failure in
    /// accepting a connection or in the exchange with one MCP client does
    /// not stop it and is not returned. A test stops it by aborting its task.
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
}

/// `host` and `port` as one address, with an IPv6 address (which `host`
/// holds without brackets) in brackets: `127.0.0.1:8000`, `[::1]:8000`.
pub(crate) fn socket_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        return format!("[{host}]:{port}");
    }
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::McpError;

    fn unavailable(host: &str) -> McpError {
        McpError::AddressUnavailable {
            host: host.to_owned(),
            port: 8000,
            source: io::Error::new(io::ErrorKind::AddrInUse, "address in use"),
        }
    }

    #[test]
    fn an_unavailable_address_is_shown_with_only_an_ipv6_host_in_brackets() {
        assert_eq!(
            unavailable("::1").to_string(),
            "cannot listen on [::1]:8000"
        );
        assert_eq!(
            unavailable("127.0.0.1").to_string(),
            "cannot listen on 127.0.0.1:8000"
        );
    }
}
