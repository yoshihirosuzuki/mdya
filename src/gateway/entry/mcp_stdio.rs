//! Runs the MCP server (stdio) over the stdin and stdout that `run_with`
//! received. They block, and the MCP server reads and writes without
//! blocking, so one thread copies stdin to the server, and another copies
//! what the server writes to stdout.

use std::io::{self, BufRead, ErrorKind, Write};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use super::super::error_report::GatewayError;
use super::super::mcp::{self, McpServerParts};
use super::EntryError;

/// The most bytes on their way between a thread and the MCP server at once,
/// each way.
const BUFFER_BYTES: usize = 64 * 1024;

/// Runs the MCP server (stdio) until it ends, as `mcp::serve_stdio` does, and
/// until what it wrote has been copied to `stdout`, for at most
/// [`STDOUT_COPY_LIMIT`] more. An error if the exchange could not be started,
/// or if a thread could not be started.
pub(super) async fn serve(
    parts: McpServerParts,
    stdin: Box<dyn BufRead + Send>,
    stdout: Box<dyn Write + Send>,
) -> Result<(), GatewayError> {
    serve_within(parts, stdin, stdout, STDOUT_COPY_LIMIT).await
}

/// [`serve`], waiting at most `copy_limit` for the copy to stdout at the end.
async fn serve_within(
    parts: McpServerParts,
    stdin: Box<dyn BufRead + Send>,
    stdout: Box<dyn Write + Send>,
    copy_limit: Duration,
) -> Result<(), GatewayError> {
    let handle = Handle::current();
    let (to_server, server_reader) = tokio::io::duplex(BUFFER_BYTES);
    let (server_writer, from_server) = tokio::io::duplex(BUFFER_BYTES);
    // Not waited for: reading stdin may block until the process ends.
    spawn("mdya-mcp-stdin", {
        let handle = handle.clone();
        move || copy_stdin(stdin, to_server, &handle)
    })
    .map_err(resources_unavailable)?;
    let (copied, stdout_copied) = oneshot::channel();
    spawn("mdya-mcp-stdout", move || {
        copy_stdout(from_server, stdout, &handle);
        let _ = copied.send(());
    })
    .map_err(resources_unavailable)?;
    let served = mcp::serve_stdio(parts, server_reader, server_writer).await;
    // The server has dropped its writer, so the copy ends once it has
    // written everything, or once stdout cannot be written. A write to a
    // stdout that nobody reads waits, so the wait has a limit.
    let _ = tokio::time::timeout(copy_limit, stdout_copied).await;
    served.map_err(GatewayError::Mcp)
}

/// How long the server's end waits for what it wrote to be copied to stdout.
const STDOUT_COPY_LIMIT: Duration = Duration::from_secs(5);

/// Starts a thread of the given name.
fn spawn(name: &str, run: impl FnOnce() + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map(drop)
}

/// A thread that could not be started, as the error to report.
fn resources_unavailable(error: io::Error) -> GatewayError {
    GatewayError::Entry(EntryError::ResourcesUnavailable(error))
}

/// Copies stdin to the server until stdin ends or cannot be read, or the
/// server no longer reads. Then the server sees the end of its input.
fn copy_stdin(mut stdin: Box<dyn BufRead + Send>, mut to_server: DuplexStream, handle: &Handle) {
    loop {
        let length = match stdin.fill_buf() {
            Ok([]) => break,
            Ok(read) => {
                if handle.block_on(to_server.write_all(read)).is_err() {
                    break;
                }
                read.len()
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        stdin.consume(length);
    }
}

/// Copies what the server writes to stdout, flushing after each piece, until
/// the server stops writing or stdout cannot be written. Then the server's
/// next write fails, as a write to stdout would.
fn copy_stdout(mut from_server: DuplexStream, mut stdout: Box<dyn Write + Send>, handle: &Handle) {
    let mut buffer = vec![0; BUFFER_BYTES];
    loop {
        let read = match handle.block_on(from_server.read(&mut buffer)) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let written = stdout
            .write_all(&buffer[..read])
            .and_then(|()| stdout.flush());
        if written.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
