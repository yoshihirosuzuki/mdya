//! Puts a pipe on the process's stdout, so that what the libraries write
//! there does not reach the user's stdout: `log_progress` reads the pipe and
//! writes what it reads as warnings in the logs. mdya writes its results and
//! the MCP exchange to a duplicate of the process's stdout as it was.
//!
//! On Unix, the pipe goes on file descriptor 1, which Rust's standard library
//! and the C library both write to. On Windows, it goes on the process's
//! standard output handle, which Rust's standard library looks up for each
//! write, and on the C runtime's descriptor 1, which the C code in the build
//! writes through. What keeps a handle of its own from before, or uses
//! another copy of the C runtime, is not covered.

use std::io::{self, PipeReader, Write};

#[cfg(unix)]
mod unix;
#[cfg(any(windows, test))]
mod utf8;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as os;
#[cfg(windows)]
use windows as os;

#[cfg(test)]
mod tests;

/// The pipe on the process's stdout.
pub(super) struct AttachedPipe {
    /// Where mdya writes its results and the MCP exchange: the process's
    /// stdout as it was before the pipe.
    pub(super) stdout: Box<dyn Write + Send>,
    /// The reading end of the pipe.
    pub(super) reader: PipeReader,
}

/// Puts a pipe on the process's stdout. Call it before any other thread
/// starts, so that no write from another thread goes on either side of it
/// halfway.
pub(super) fn attach() -> io::Result<AttachedPipe> {
    os::attach()
}

/// Puts the null device on the process's stdout in place of the pipe, so
/// that the pipe's writing end closes and what reads the pipe comes to its
/// end. What is written to stdout afterwards is thrown away.
pub(super) fn detach() -> io::Result<()> {
    os::detach()
}

#[cfg(not(any(unix, windows)))]
mod os {
    use std::io;

    use super::AttachedPipe;

    pub(super) fn attach() -> io::Result<AttachedPipe> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub(super) fn detach() -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
