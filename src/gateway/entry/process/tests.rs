use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::process::Command;

use crate::test_support::log_receiver::capture;

use super::*;

/// The test that replaces the panic hook, which runs only in a process of
/// its own: the hook is the whole process's.
const CHILD: &str = "replaces_the_panic_hook_and_panics_in_a_process_of_its_own";

/// Set on the process of its own, so that the test runs only there.
const CHILD_MARK: &str = "MDYA_TEST_PANIC_HOOK_CHILD";

#[test]
fn the_replaced_panic_hook_writes_the_version_the_location_the_message_and_the_backtrace() {
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
        .env_remove("RUST_BACKTRACE")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("{} panicked at src", version().version)),
        "{stderr}"
    );
    assert!(stderr.contains("tests.rs:"), "{stderr}");
    assert!(stderr.contains(":\na bug\\u{1b}\nBacktrace:\n"), "{stderr}");
    // On Windows, the backtrace of the release build does not name mdya's
    // functions (the release profile in `Cargo.toml`).
    if !cfg!(windows) {
        assert!(stderr.contains(PANICKING_FUNCTION), "{stderr}");
    }
}

/// The function that the test in a process of its own panics in.
const PANICKING_FUNCTION: &str = "panics_in_a_function_of_its_own";

#[test]
#[ignore = "run in a process of its own by the test above"]
#[expect(
    clippy::disallowed_methods,
    reason = "the test runs only where the test above set the mark on its process"
)]
fn replaces_the_panic_hook_and_panics_in_a_process_of_its_own() {
    if std::env::var_os(CHILD_MARK).as_deref() != Some(OsStr::new("1")) {
        return;
    }
    replace_panic_hook();
    panics_in_a_function_of_its_own();
}

/// Panics in a function of its own, which the release build might otherwise
/// put inside its caller.
#[inline(never)]
fn panics_in_a_function_of_its_own() {
    panic!("a bug\u{1b}");
}

#[test]
fn the_message_of_a_panic_is_the_text_it_was_given() {
    let fixed: Box<dyn Any + Send> = Box::new("index out of bounds");
    let made: Box<dyn Any + Send> = Box::new(String::from("failed at 3"));
    let other: Box<dyn Any + Send> = Box::new(3_u8);

    assert_eq!(panic_message(fixed.as_ref()), "index out of bounds");
    assert_eq!(panic_message(made.as_ref()), "failed at 3");
    assert_eq!(panic_message(other.as_ref()), "Box<dyn Any>");
}

/// Captures a backtrace here, in a function of its own.
#[inline(never)]
fn backtrace_captured_here() -> String {
    Backtrace::force_capture().to_string()
}

/// The release build keeps the symbols, so that the backtrace that the panic
/// hook writes names the functions on Linux and macOS. `verify.yml` runs this
/// test with the release settings on each of them.
#[test]
#[cfg_attr(
    windows,
    ignore = "on Windows, the backtrace of the release build does not name mdya's functions \
              (the release profile in Cargo.toml)"
)]
fn a_backtrace_names_the_functions_it_went_through() {
    let backtrace = backtrace_captured_here();

    assert!(backtrace.contains("backtrace_captured_here"), "{backtrace}");
}

#[test]
fn what_is_read_from_the_pipe_goes_to_the_logs_until_the_pipe_ends() {
    let (reader, writer) = io::pipe().unwrap();
    let logging = Mutex::new(None);

    log_libraries_stdout(Pipe::Attached(reader), &logging);
    drop(writer);

    let started = logging.lock().unwrap().take().unwrap();
    // Returns once every writing end is closed.
    started.finish();
}

#[test]
fn a_pipe_that_could_not_be_put_on_stdout_is_warned_about() {
    let logging = Mutex::new(None);

    let ((), logs) = capture(|| {
        log_libraries_stdout(Pipe::Failed(io::Error::other("no pipes")), &logging);
    });

    assert!(logging.lock().unwrap().is_none());
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].level, tracing::Level::WARN);
    assert!(
        logs[0].message.contains("no pipe could be put on stdout"),
        "{logs:?}"
    );
}
