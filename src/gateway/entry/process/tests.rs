use std::backtrace::Backtrace;
use std::ffi::OsStr;
use std::process::{Command, Output};

use crate::test_support::log_receiver::capture;

use super::*;

/// The test that replaces the panic hook, which runs only in a process of
/// its own: the hook is the whole process's.
const CHILD: &str = "replaces_the_panic_hook_and_panics_in_a_process_of_its_own";

/// Set on the process of its own, so that the test runs only there.
const CHILD_MARK: &str = "MDYA_TEST_PANIC_HOOK_CHILD";

/// Runs the test `child` of this module, which is ignored, in a process of
/// its own with `mark` set, and returns how it ended.
fn output_of_child(child: &str, mark: &str) -> Output {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    Command::new(std::env::current_exe().unwrap())
        .args([
            format!("{module}::{child}").as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(mark, "1")
        .env_remove("RUST_BACKTRACE")
        .output()
        .unwrap()
}

#[test]
fn the_replaced_panic_hook_writes_the_version_the_location_the_message_and_the_backtrace() {
    let output = output_of_child(CHILD, CHILD_MARK);

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

/// The test that runs mdya as `run` does with a component replaced, which
/// runs only in a process of its own: it replaces the panic hook and puts a
/// pipe on stdout, which are the whole process's.
const REPLACING_CHILD: &str = "runs_as_run_does_with_a_component_replaced_in_a_process_of_its_own";

/// Set on the process of its own, so that the test runs only there.
const REPLACING_CHILD_MARK: &str = "MDYA_TEST_RUN_REPLACING_CHILD";

#[test]
fn run_replacing_runs_as_run_does_with_the_components_that_it_replaces() {
    let output = output_of_child(REPLACING_CHILD, REPLACING_CHILD_MARK);

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The test in the process of its own checks the exit code.
    assert!(output.status.success(), "{stderr}");
    // The replaced component ran, and its panic went through the panic hook
    // that `run` puts.
    assert!(
        stderr.contains(&format!("{} panicked at src", version().version)),
        "{stderr}"
    );
    assert!(stderr.contains(":\nthe replaced config\n"), "{stderr}");
    assert!(
        stderr.contains("It happened while preparing to run mdya config list.\n"),
        "{stderr}"
    );
    // What the replaced component wrote to stdout went through the pipe that
    // `run` puts on stdout, into the logs.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains(LIBRARY_LINE), "{stdout}");
    let warning = stderr
        .lines()
        .find(|line| line.contains("a library wrote a line to stdout"));
    assert!(
        warning.is_some_and(|line| line.contains(LIBRARY_LINE)),
        "{stderr}"
    );
}

/// What the replaced component writes to stdout, as a library might.
const LIBRARY_LINE: &str = "written by the replaced config";

#[test]
#[ignore = "run in a process of its own by the test above"]
#[expect(
    clippy::disallowed_methods,
    reason = "the test runs only where the test above set the mark on its process"
)]
fn runs_as_run_does_with_a_component_replaced_in_a_process_of_its_own() {
    if std::env::var_os(REPLACING_CHILD_MARK).as_deref() != Some(OsStr::new("1")) {
        return;
    }
    let data_dir = tempfile::tempdir().unwrap();
    let args = vec![
        OsString::from("mdya"),
        OsString::from("--data-dir"),
        data_dir.path().as_os_str().to_owned(),
        OsString::from("config"),
        OsString::from("list"),
    ];

    let code = run_replacing(args, |factories| {
        // The function that `replace` gets sets no environment variable.
        (factories.set_process_environment)(vec![(
            OsString::from(UNSET_VARIABLE),
            OsString::from("1"),
        )]);
        factories.config = Box::new(|_| {
            println!("{LIBRARY_LINE}");
            panic!("the replaced config")
        });
    });

    assert_eq!(code, ExitCode::from(101));
    assert_eq!(std::env::var_os(UNSET_VARIABLE), None);
}

/// The environment variable that the test in a process of its own asks the
/// function that sets environment variables to set.
const UNSET_VARIABLE: &str = "MDYA_TEST_RUN_REPLACING_UNSET";

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
