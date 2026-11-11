use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::Level;

use crate::test_support::log_receiver::capture;
use crate::watchdog::WatchdogSettings;

use super::*;

/// Long enough that a test that is passing never waits for it.
const NEVER: Duration = Duration::from_secs(3600);

/// How long a test waits for the monitor's thread before it fails.
const PATIENCE: Duration = Duration::from_secs(30);

/// One `stop` that a test process was told to do.
type Stop = (String, i32);

/// A process whose memory is whatever the test says.
struct TestProcess {
    /// What each measurement gives, in order. The last one repeats.
    measurements: Vec<Option<u64>>,
    stops: Arc<Mutex<Vec<Stop>>>,
    /// Told of each measurement. Dropped with the process, which tells the
    /// test that the monitor's thread has ended.
    measured: Option<Sender<()>>,
    /// When set, each measurement waits for a value from this.
    release: Option<Receiver<()>>,
}

impl TestProcess {
    fn measuring(measurements: &[Option<u64>]) -> (Self, Arc<Mutex<Vec<Stop>>>) {
        let stops = Arc::new(Mutex::new(Vec::new()));
        let process = Self {
            measurements: measurements.to_vec(),
            stops: Arc::clone(&stops),
            measured: None,
            release: None,
        };
        (process, stops)
    }

    fn telling(mut self, measured: Sender<()>) -> Self {
        self.measured = Some(measured);
        self
    }

    fn waiting_for(mut self, release: Receiver<()>) -> Self {
        self.release = Some(release);
        self
    }
}

impl Process for TestProcess {
    fn resident_bytes(&mut self) -> Option<u64> {
        if let Some(release) = &self.release {
            release.recv_timeout(PATIENCE).unwrap();
        }
        if let Some(measured) = &self.measured {
            measured.send(()).unwrap();
        }
        if self.measurements.len() > 1 {
            return self.measurements.remove(0);
        }
        self.measurements[0]
    }

    fn stop(&mut self, message: &str, exit_code: i32) {
        // Logged so that a test can see what came before stopping.
        tracing::info!("stopped");
        self.stops
            .lock()
            .unwrap()
            .push((message.to_owned(), exit_code));
    }
}

fn settings(memory_limit_mb: u64) -> WatchdogSettings {
    WatchdogSettings {
        memory_limit_mb,
        message: "over the limit\n".to_owned(),
        exit_code: 137,
    }
}

fn stops_after_checking(memory_limit_mb: u64, resident_bytes: u64) -> Vec<Stop> {
    let (process, stops) = TestProcess::measuring(&[Some(resident_bytes)]);
    Check::new(settings(memory_limit_mb), process).run();
    stops.lock().unwrap().clone()
}

fn stopped_with_the_settings() -> Vec<Stop> {
    vec![("over the limit\n".to_owned(), 137)]
}

#[test]
fn a_process_one_byte_above_the_limit_is_stopped_with_the_message_and_the_exit_code() {
    assert_eq!(
        stops_after_checking(1, 1_048_577),
        stopped_with_the_settings()
    );
}

#[test]
fn a_process_exactly_at_the_limit_is_not_stopped() {
    assert_eq!(stops_after_checking(1, 1_048_576), []);
}

#[test]
fn a_limit_of_several_mb_counts_1_048_576_bytes_for_each() {
    assert_eq!(stops_after_checking(3, 3 * 1_048_576), []);
    assert_eq!(
        stops_after_checking(3, 3 * 1_048_576 + 1),
        stopped_with_the_settings()
    );
}

#[test]
fn the_largest_limit_that_can_be_set_is_above_any_amount_of_memory() {
    assert_eq!(stops_after_checking(9_007_199_254_740_991, u64::MAX), []);
}

#[test]
fn a_check_says_whether_it_stopped_the_process() {
    let (below, _) = TestProcess::measuring(&[Some(1)]);
    let (above, _) = TestProcess::measuring(&[Some(2_000_000)]);

    assert!(!Check::new(settings(1), below).run());
    assert!(Check::new(settings(1), above).run());
}

#[test]
fn going_over_the_limit_is_logged_as_an_error_before_the_process_is_stopped() {
    let (process, _) = TestProcess::measuring(&[Some(2_000_000)]);
    let mut check = Check::new(settings(1), process);

    let (_, logs) = capture(|| check.run());

    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(logs[0].level, Level::ERROR);
    assert_eq!(logs[0].message, "the memory limit was exceeded");
    assert_eq!(
        logs[0].fields,
        [
            ("resident_bytes".to_owned(), "2000000".to_owned()),
            ("memory_limit_mb".to_owned(), "1".to_owned()),
        ]
    );
    assert_eq!(logs[1].message, "stopped");
}

#[test]
fn a_process_below_the_limit_logs_nothing() {
    let (process, _) = TestProcess::measuring(&[Some(1)]);
    let mut check = Check::new(settings(1), process);

    let (_, logs) = capture(|| check.run());

    assert_eq!(logs, []);
}

#[test]
fn a_measurement_that_fails_does_not_stop_the_process() {
    let (process, stops) = TestProcess::measuring(&[None]);

    assert!(!Check::new(settings(1), process).run());
    assert_eq!(*stops.lock().unwrap(), []);
}

#[test]
fn only_the_first_measurement_that_fails_warns() {
    let (process, _) = TestProcess::measuring(&[None]);
    let mut check = Check::new(settings(1), process);

    let (_, logs) = capture(|| {
        check.run();
        check.run();
    });

    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].level, Level::WARN);
    assert_eq!(
        logs[0].message,
        "cannot measure the memory of this process, so it may go over the memory limit"
    );
}

#[test]
fn a_process_above_the_limit_is_stopped_after_a_measurement_that_failed() {
    let (process, stops) = TestProcess::measuring(&[None, Some(2_000_000)]);
    let mut check = Check::new(settings(1), process);

    assert!(!check.run());
    assert!(check.run());
    assert_eq!(*stops.lock().unwrap(), stopped_with_the_settings());
}

#[test]
fn the_monitor_measures_at_once_and_then_again_and_again() {
    let (measured, measurements) = channel();
    let (process, _) = TestProcess::measuring(&[Some(1)]);

    let _monitor = Monitor::start(
        settings(1),
        process.telling(measured),
        Duration::from_millis(1),
    );

    for _ in 0..3 {
        measurements.recv_timeout(PATIENCE).unwrap();
    }
}

#[test]
fn starting_the_monitor_does_not_wait_for_a_measurement() {
    let (release, released) = channel();
    let (measured, measurements) = channel();
    let (process, _) = TestProcess::measuring(&[Some(1)]);

    let _monitor = Monitor::start(
        settings(1),
        process.telling(measured).waiting_for(released),
        NEVER,
    );

    // The measurement is still waiting for this, after the start has returned.
    assert_eq!(
        measurements.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    );
    release.send(()).unwrap();
    measurements.recv_timeout(PATIENCE).unwrap();
}

/// Waits until the monitor's thread has ended, which drops the process and
/// with it the sender of `measurements`.
fn assert_the_thread_ends(measurements: &Receiver<()>) {
    loop {
        match measurements.recv_timeout(PATIENCE) {
            Ok(()) => {}
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => panic!("the monitor's thread did not end"),
        }
    }
}

#[test]
fn dropping_the_monitor_ends_its_wait_and_its_thread_before_it_returns() {
    let (measured, measurements) = channel();
    let (process, _) = TestProcess::measuring(&[Some(1)]);
    let monitor = Monitor::start(settings(1), process.telling(measured), NEVER);
    measurements.recv_timeout(PATIENCE).unwrap();

    drop(monitor);

    assert_eq!(
        measurements.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected)
    );
}

#[test]
fn dropping_the_monitor_waits_for_a_measurement_in_progress() {
    let (release, released) = channel();
    let (process, _) = TestProcess::measuring(&[Some(1)]);
    let monitor = Monitor::start(settings(1), process.waiting_for(released), NEVER);

    let dropping = thread::spawn(move || drop(monitor));
    thread::sleep(Duration::from_millis(50));

    assert!(!dropping.is_finished());
    release.send(()).unwrap();
    dropping.join().unwrap();
}

#[test]
fn the_monitor_stops_the_process_from_its_thread_and_then_ends() {
    let (measured, measurements) = channel();
    let (process, stops) = TestProcess::measuring(&[Some(1), Some(2_000_000)]);

    let _monitor = Monitor::start(
        settings(1),
        process.telling(measured),
        Duration::from_millis(1),
    );

    assert_the_thread_ends(&measurements);
    assert_eq!(*stops.lock().unwrap(), stopped_with_the_settings());
}
