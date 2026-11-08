use std::io::{BufReader, Cursor, Read};
use std::sync::{Arc, Mutex, mpsc};

use serde_json::json;
use tokio::runtime::Runtime;

use super::super::super::error_report::test_double::TestErrorReport;
use super::super::super::log_progress::test_double::TestConsole;
use super::super::super::operations::test_double::TestOperations;
use super::super::super::test_double::captured::CapturedOutput;
use super::*;

/// Waits on every write, as a stdout that nobody reads does, until the test
/// lets it go.
struct Blocking(Mutex<mpsc::Receiver<()>>);

impl Write for Blocking {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _ = self.0.lock().unwrap().recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_wait_for_stdout_after_the_server_ends_has_a_limit() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let parts = McpServerParts {
        operations: Arc::new(TestOperations::new()),
        show_version: true,
        size_limit: None,
        error_report: Arc::new(TestErrorReport::new()),
        console: Arc::new(TestConsole::new()),
    };
    let stdin = [
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ]
    .iter()
    .map(|request| format!("{request}\n"))
    .collect::<String>();
    let (release, released) = mpsc::channel();

    // Returns although stdout never takes the answer to `initialize`.
    let served = runtime.block_on(serve_within(
        parts,
        Box::new(Cursor::new(stdin.into_bytes())),
        Box::new(Blocking(Mutex::new(released))),
        Duration::from_millis(100),
    ));

    assert!(served.is_ok());
    drop(release);
}

/// Gives what each step says, in order, then ends.
struct Steps(Vec<io::Result<&'static [u8]>>);

impl Read for Steps {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.0.is_empty() {
            return Ok(0);
        }
        let bytes = self.0.remove(0)?;
        buffer[..bytes.len()].copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

/// Gives bytes for ever.
struct Endless;

impl Read for Endless {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        buffer.fill(b'x');
        Ok(buffer.len())
    }
}

/// Fails every write.
struct Failing;

impl Write for Failing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("the disk is full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// Copies `stdin` to a server end on a thread of its own, as `serve` does,
/// and returns what the server reads until the end.
fn copied_stdin(stdin: impl Read + Send + 'static) -> Vec<u8> {
    let runtime = runtime();
    let (to_server, mut server) = tokio::io::duplex(64);
    let handle = runtime.handle().clone();
    let copying = std::thread::spawn(move || {
        copy_stdin(Box::new(BufReader::new(stdin)), to_server, &handle);
    });
    let mut read = Vec::new();
    runtime.block_on(server.read_to_end(&mut read)).unwrap();
    copying.join().unwrap();
    read
}

#[test]
fn stdin_is_copied_to_the_server_past_an_interruption_until_it_ends() {
    let stdin = Steps(vec![
        Err(io::Error::from(ErrorKind::Interrupted)),
        Ok(b"abc"),
        Ok(b"def"),
    ]);

    assert_eq!(copied_stdin(stdin), b"abcdef");
}

#[test]
fn stdin_that_cannot_be_read_ends_the_servers_input() {
    let stdin = Steps(vec![
        Ok(b"abc"),
        Err(io::Error::other("the device is gone")),
    ]);

    assert_eq!(copied_stdin(stdin), b"abc");
}

#[test]
fn the_copy_of_stdin_stops_when_the_server_no_longer_reads() {
    let runtime = runtime();
    let (to_server, server) = tokio::io::duplex(64);
    drop(server);
    let handle = runtime.handle().clone();

    // Without the stop, copying an endless stdin would never end.
    std::thread::spawn(move || copy_stdin(Box::new(BufReader::new(Endless)), to_server, &handle))
        .join()
        .unwrap();
}

#[test]
fn what_the_server_writes_is_copied_to_stdout_until_the_server_stops_writing() {
    let runtime = runtime();
    let (mut server, from_server) = tokio::io::duplex(64);
    let stdout = CapturedOutput::new();
    let handle = runtime.handle().clone();
    let copying = {
        let stdout = stdout.clone();
        std::thread::spawn(move || copy_stdout(from_server, Box::new(stdout), &handle))
    };

    runtime.block_on(async {
        server.write_all(b"{\"jsonrpc\":\"2.0\"}\n").await.unwrap();
        drop(server);
    });
    copying.join().unwrap();

    assert_eq!(stdout.text(), "{\"jsonrpc\":\"2.0\"}\n");
}

#[test]
fn when_stdout_cannot_be_written_the_servers_next_write_fails() {
    let runtime = runtime();
    let (mut server, from_server) = tokio::io::duplex(64);
    let handle = runtime.handle().clone();
    let copying = std::thread::spawn(move || copy_stdout(from_server, Box::new(Failing), &handle));

    runtime.block_on(server.write_all(b"first")).unwrap();
    copying.join().unwrap();
    let next = runtime.block_on(server.write_all(b"next"));

    assert_eq!(next.unwrap_err().kind(), ErrorKind::BrokenPipe);
}
