//! The pipe on the process's standard output handle, which Rust's standard
//! library looks up for each write, and on the C runtime's descriptor 1,
//! which the C code in the build writes through. mdya writes to a console in
//! UTF-16, as the standard library does: the bytes of UTF-8 written to a
//! console would be read in the console's code page.

use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsHandle, AsRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::System::Console::{
    GetConsoleMode, STD_OUTPUT_HANDLE, SetStdHandle, WriteConsoleW,
};

use super::AttachedPipe;
use super::utf8;

/// The handle on the process's standard output handle, kept open while it is
/// there, and closed when another one takes its place.
static STANDARD_OUTPUT: Mutex<Option<OwnedHandle>> = Mutex::new(None);

#[expect(
    clippy::disallowed_methods,
    reason = "the process's stdout is duplicated for mdya's own writes, before the pipe goes on it"
)]
pub(super) fn attach() -> io::Result<AttachedPipe> {
    let original = std::io::stdout().as_handle().try_clone_to_owned()?;
    let (reader, writer) = io::pipe()?;
    put_on_stdout(OwnedHandle::from(writer))?;
    let stdout: Box<dyn Write + Send> = if is_console(&original) {
        Box::new(Console {
            handle: original,
            rest: Vec::new(),
        })
    } else {
        Box::new(File::from(original))
    };
    Ok(AttachedPipe { stdout, reader })
}

pub(super) fn detach() -> io::Result<()> {
    let null = File::options().write(true).open("NUL")?;
    put_on_stdout(OwnedHandle::from(null))
}

/// Puts `handle` on the process's standard output handle and on the C
/// runtime's descriptor 1. When the C runtime's side cannot be done, the
/// standard output handle goes back to a duplicate of the one that was there.
#[expect(
    clippy::disallowed_methods,
    reason = "a duplicate of the handle on the process's stdout is kept, to put back if the C \
              runtime's side cannot be done"
)]
fn put_on_stdout(handle: OwnedHandle) -> io::Result<()> {
    // A duplicate stays open whatever happens to the handle it duplicates,
    // which the C runtime may close on its side.
    let previous = std::io::stdout().as_handle().try_clone_to_owned()?;
    // SAFETY: the handle is open, and stays open while it is the standard
    // output handle: it is kept in `STANDARD_OUTPUT` until another one takes
    // its place.
    if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, handle.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (on_stdout, put) = match put_on_c_runtime(&handle) {
        Ok(()) => (handle, Ok(())),
        Err(error) => {
            // SAFETY: `previous` is open, and stays open while it is the
            // standard output handle: it is kept in `STANDARD_OUTPUT` below.
            let restored = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, previous.as_raw_handle()) };
            // Whichever is the standard output handle now is the one kept
            // open.
            let on_stdout = if restored != 0 { previous } else { handle };
            (on_stdout, Err(error))
        }
    };
    // The handle that was kept before is closed here, unless it is the one
    // the process started with, which mdya did not open.
    *STANDARD_OUTPUT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(on_stdout);
    put
}

/// Puts `handle` on the C runtime's descriptor 1. When it cannot be done,
/// descriptor 1 is unchanged.
fn put_on_c_runtime(handle: &OwnedHandle) -> io::Result<()> {
    let for_c_runtime = handle.try_clone()?;
    // SAFETY: the C runtime takes over the handle, which is open, and which
    // nothing else closes: `for_c_runtime` gives it up here. Flags 0 open it
    // for writing, in binary mode, so that what the C code writes is not
    // changed on its way.
    let descriptor =
        unsafe { libc::open_osfhandle(for_c_runtime.into_raw_handle() as libc::intptr_t, 0) };
    if descriptor == -1 {
        // The C runtime's documentation does not say whether it closed the
        // handle then. It is left as it is: leaking it is safer than closing
        // it twice.
        return Err(io::Error::other("the C runtime cannot take the handle"));
    }
    // SAFETY: both are descriptors of the C runtime, and `descriptor` is
    // open. Descriptor 1 is closed first, and then refers to the same handle.
    let put = unsafe { libc::dup2(descriptor, 1) };
    // SAFETY: `descriptor` is open, and this is the only place that closes it.
    unsafe { libc::close(descriptor) };
    if put == -1 {
        return Err(io::Error::other(
            "the C runtime's standard output cannot be replaced",
        ));
    }
    Ok(())
}

/// Whether `handle` is a console.
fn is_console(handle: &OwnedHandle) -> bool {
    let mut mode = 0;
    // SAFETY: the handle is open, and `mode` is where the mode is written.
    unsafe { GetConsoleMode(handle.as_raw_handle(), &mut mode) != 0 }
}

/// The most UTF-16 units written to a console at once.
const CONSOLE_CHUNK: usize = 8192;

/// A console, written in UTF-16.
struct Console {
    handle: OwnedHandle,
    /// The start of a character, written last, that is not whole yet.
    rest: Vec<u8>,
}

impl Write for Console {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut pending = std::mem::take(&mut self.rest);
        pending.extend_from_slice(bytes);
        let (whole, rest) = utf8::split_whole(&pending)?;
        write_console(&self.handle, whole)?;
        self.rest = rest.to_vec();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writes `text` to the console, in pieces that do not split a character.
fn write_console(handle: &OwnedHandle, text: &str) -> io::Result<()> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut rest = &units[..];
    while !rest.is_empty() {
        let mut count = rest.len().min(CONSOLE_CHUNK);
        if count < rest.len() && (0xD800..0xDC00).contains(&rest[count - 1]) {
            count -= 1;
        }
        let count = u32::try_from(count).map_err(io::Error::other)?;
        let mut written = 0;
        // SAFETY: the handle is an open console, the buffer holds `count`
        // units, and `written` is where the count written goes.
        let succeeded = unsafe {
            WriteConsoleW(
                handle.as_raw_handle(),
                rest.as_ptr(),
                count,
                &mut written,
                std::ptr::null(),
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        let written = usize::try_from(written).map_err(io::Error::other)?;
        if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        rest = rest.get(written..).unwrap_or_default();
    }
    Ok(())
}
