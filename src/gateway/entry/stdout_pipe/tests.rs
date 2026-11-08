use std::ffi::OsStr;
use std::io::{Read, Write};
use std::process::Command;

use super::*;

/// The test that puts the pipe on stdout, which runs only in a process of its
/// own: the pipe takes the whole process's stdout, which the test harness
/// writes to as well.
const CHILD: &str = "puts_the_pipe_on_stdout_and_writes_in_a_process_of_its_own";

/// Set on the process of its own, so that the test runs only there.
const CHILD_MARK: &str = "MDYA_TEST_STDOUT_PIPE_CHILD";

#[test]
fn what_rust_and_the_c_library_write_to_stdout_goes_to_the_pipe_and_not_to_mdyas_stdout() {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            format!("{module}::{CHILD}").as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARK, "1")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("written by mdya\n"), "{stdout}");
    assert!(!stdout.contains("written by Rust"), "{stdout}");
    assert!(!stdout.contains("written by C"), "{stdout}");
    assert!(stderr.contains("read from the pipe"), "{stderr}");
}

#[test]
#[ignore = "run in a process of its own by the test above"]
#[expect(
    clippy::disallowed_methods,
    reason = "the test runs only where the test above set the mark on its process"
)]
fn puts_the_pipe_on_stdout_and_writes_in_a_process_of_its_own() {
    if std::env::var_os(CHILD_MARK).as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let AttachedPipe {
        mut stdout,
        mut reader,
    } = attach().unwrap();

    println!("written by Rust");
    let by_c = b"written by C\n";
    // SAFETY: descriptor 1 is open, and the buffer holds the bytes.
    let written = unsafe { libc::write(1, by_c.as_ptr().cast(), by_c.len() as _) };
    stdout.write_all(b"written by mdya\n").unwrap();
    stdout.flush().unwrap();
    detach().unwrap();

    assert_eq!(usize::try_from(written).ok(), Some(by_c.len()));
    let mut read = String::new();
    reader.read_to_string(&mut read).unwrap();
    assert!(
        read.starts_with("written by Rust\n") && read.contains("written by C"),
        "{read:?}"
    );
    eprintln!("read from the pipe: {read:?}");
}
