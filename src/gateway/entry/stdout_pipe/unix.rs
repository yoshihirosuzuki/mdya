//! The pipe on file descriptor 1, which Rust's standard library and the C
//! library both write to.

use std::fs::File;
use std::io;
use std::os::fd::AsFd;

use super::AttachedPipe;

#[expect(
    clippy::disallowed_methods,
    reason = "the process's stdout is duplicated for mdya's own writes, before the pipe goes on it"
)]
pub(super) fn attach() -> io::Result<AttachedPipe> {
    let original = std::io::stdout().as_fd().try_clone_to_owned()?;
    let (reader, writer) = io::pipe()?;
    rustix::stdio::dup2_stdout(&writer)?;
    // Descriptor 1 holds the writing end now; this copy of it is not needed.
    drop(writer);
    Ok(AttachedPipe {
        stdout: Box::new(File::from(original)),
        reader,
    })
}

pub(super) fn detach() -> io::Result<()> {
    let null = File::options().write(true).open("/dev/null")?;
    rustix::stdio::dup2_stdout(&null)?;
    Ok(())
}
