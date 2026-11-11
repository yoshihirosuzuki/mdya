use std::ffi::OsStr;
use std::hint::black_box;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::Duration;

use super::*;

/// Set on a process of its own, so that the tests below run only there.
const CHILD_MARK: &str = "MDYA_TEST_WATCHDOG_CHILD";

/// The command that runs the test `name` of this module in a process of its
/// own.
fn command_for_a_process_of_its_own(name: &str) -> Command {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            format!("{module}::{name}").as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARK, "1");
    command
}

fn run_in_a_process_of_its_own(name: &str) -> Output {
    command_for_a_process_of_its_own(name).output().unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the tests read the mark that the test running them set on this process"
)]
fn runs_in_a_process_of_its_own() -> bool {
    std::env::var_os(CHILD_MARK).as_deref() == Some(OsStr::new("1"))
}

#[test]
fn a_process_over_the_limit_stops_with_the_exit_code_and_writes_the_message() {
    let output = run_in_a_process_of_its_own("goes_over_a_limit_of_1_mb");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(137), "{stderr}");
    assert!(stderr.contains("over the limit of the test\n"), "{stderr}");
}

#[test]
fn a_process_over_the_limit_stops_even_when_stderr_is_closed() {
    let mut child = command_for_a_process_of_its_own("goes_over_a_limit_of_1_mb")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    drop(child.stderr.take());

    assert_eq!(child.wait().unwrap().code(), Some(137));
}

/// The process is over the limit from the start, as a test binary is larger
/// than 1 MB. The test does not finish, and the process stops all the same.
#[test]
#[ignore = "run in a process of its own by the test above"]
fn goes_over_a_limit_of_1_mb() {
    if !runs_in_a_process_of_its_own() {
        return;
    }
    let _watchdog = start(WatchdogSettings {
        memory_limit_mb: 1,
        message: "over the limit of the test\n".to_owned(),
        exit_code: 137,
    });
    sleep(Duration::from_secs(30));
    panic!("the watchdog did not stop the process");
}

#[test]
fn the_measurement_is_of_the_memory_that_is_resident() {
    let output = run_in_a_process_of_its_own("measures_memory_it_reserves_and_then_touches");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
}

/// In a process of its own, so that what other tests allocate at the same
/// time does not count.
#[test]
#[ignore = "run in a process of its own by the test above"]
fn measures_memory_it_reserves_and_then_touches() {
    if !runs_in_a_process_of_its_own() {
        return;
    }
    const BLOCK: usize = 256 * 1_048_576;
    const SMALLEST_PAGE: usize = 4096;
    let measure = || OwnProcess.resident_bytes().unwrap();

    let before = measure();
    let mut block = Vec::<u8>::with_capacity(BLOCK);
    let reserved = measure();
    for byte in block.spare_capacity_mut().iter_mut().step_by(SMALLEST_PAGE) {
        byte.write(1);
    }
    black_box(&mut block);
    let touched = measure();

    let half = (BLOCK / 2) as u64;
    assert!(
        reserved.saturating_sub(before) < half,
        "{before} -> {reserved}"
    );
    assert!(
        touched.saturating_sub(before) >= half,
        "{before} -> {touched}"
    );
}

/// A process that counts its measurements, for a limit that turns the
/// watchdog off.
struct CountingProcess(Arc<AtomicU32>);

impl Process for CountingProcess {
    fn resident_bytes(&mut self) -> Option<u64> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(u64::MAX)
    }

    fn stop(&mut self, _message: &str, _exit_code: i32) {}
}

#[test]
fn a_limit_of_0_measures_nothing_and_keeps_nothing_running() {
    let measurements = Arc::new(AtomicU32::new(0));

    let _watchdog = start_watching(
        WatchdogSettings {
            memory_limit_mb: 0,
            message: String::new(),
            exit_code: 137,
        },
        CountingProcess(Arc::clone(&measurements)),
        Duration::from_millis(1),
    );

    // Only this test still holds the count: nothing was started that holds
    // the process.
    assert_eq!(Arc::strong_count(&measurements), 1);
    assert_eq!(measurements.load(Ordering::SeqCst), 0);
}
