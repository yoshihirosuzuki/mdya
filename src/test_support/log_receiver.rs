//! A test double for the log receiver. The components log through the
//! `tracing` macros, and `gateway` sets the receiver that writes the logs out.
//! In a unit test, [`capture`] and [`capture_async`] set a receiver that
//! records the events instead, so that the test can check them.
//!
//! The receiver is set for the current thread only: while the closure runs,
//! or while the future is polled. Tests that run at the same time on other
//! threads therefore do not see each other's events, and an event that the
//! code under test emits on another thread (a thread it spawns, or a
//! `tokio::task::spawn_blocking` task) is not recorded.

// Parts of the double are for tests that the components using it have not
// written yet.
#![allow(dead_code)]

use std::fmt;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing::subscriber::NoSubscriber;
use tracing::{Dispatch, Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

/// A receiver that is set nowhere and kept for the rest of the process.
///
/// `tracing` decides once per callsite, when a thread first reaches it,
/// whether to pass the callsite's events on. Unless the last receiver created
/// had another one alive at the time, it asks only the receiver set for that
/// thread. A thread with none gets the answer "never" (unit tests set no
/// global receiver), and the answer stays until the answers are rebuilt, as
/// they are whenever a receiver is created. Until then, a capture on another
/// thread loses the callsite's events
/// (<https://github.com/tokio-rs/tracing/issues/3611>).
///
/// [`Records::dispatch`] creates this receiver before the first capture's, so
/// every capture's receiver is created with this one alive, and `tracing`
/// asks all the receivers alive instead. A callsite that another thread first
/// reaches while the first capture of the process is creating its receiver
/// can still get "never".
static IDLE_RECEIVER: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(NoSubscriber::new()));

/// An event that the code under test logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogRecord {
    pub(crate) level: Level,
    /// The formatted message.
    pub(crate) message: String,
    /// The other fields, in the order the event gives them, each value
    /// formatted with `Display` (`%value`), `Debug` (`?value`), or as given
    /// (a string).
    pub(crate) fields: Vec<(String, String)>,
}

/// Runs `f` with a receiver that records the events emitted on this thread,
/// and returns `f`'s result with the events, in order.
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<LogRecord>) {
    let records = Records::default();
    let output = tracing::dispatcher::with_default(&records.dispatch(), f);
    (output, records.take())
}

/// Like [`capture`], for a future: records the events emitted while the future
/// is polled.
pub(crate) async fn capture_async<F: Future>(future: F) -> (F::Output, Vec<LogRecord>) {
    let records = Records::default();
    let output = future.with_subscriber(records.dispatch()).await;
    (output, records.take())
}

/// The events one capture has recorded.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<Vec<LogRecord>>>);

impl Records {
    fn dispatch(&self) -> Dispatch {
        LazyLock::force(&IDLE_RECEIVER);
        Dispatch::new(Registry::default().with(self.clone()))
    }

    fn take(&self) -> Vec<LogRecord> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl<S: Subscriber> Layer<S> for Records {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = FieldsVisitor::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push(LogRecord {
            level: *event.metadata().level(),
            message: fields.message,
            fields: fields.others,
        });
    }
}

#[derive(Default)]
struct FieldsVisitor {
    message: String,
    others: Vec<(String, String)>,
}

impl FieldsVisitor {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
            return;
        }
        self.others.push((field.name().to_owned(), value));
    }
}

impl Visit for FieldsVisitor {
    // Overridden so that a string value is kept as given; the default would
    // format it with `Debug`, in quotes.
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;

    fn record(level: Level, message: &str, fields: &[(&str, &str)]) -> LogRecord {
        LogRecord {
            level,
            message: message.to_owned(),
            fields: fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    /// Stands for component code that warns about a document it cannot read.
    fn warn_unreadable(key: &str, cause: &str) {
        tracing::warn!(key, cause = %cause, "the document cannot be read");
    }

    #[test]
    fn a_warning_from_the_code_is_recorded_with_its_level_message_and_fields() {
        let (output, records) = capture(|| {
            warn_unreadable("docs/a.md", "broken");
            42
        });

        assert_eq!(output, 42);
        assert_eq!(
            records,
            vec![record(
                Level::WARN,
                "the document cannot be read",
                &[("key", "docs/a.md"), ("cause", "broken")]
            )]
        );
    }

    #[test]
    fn every_level_is_recorded_in_order() {
        let ((), records) = capture(|| {
            tracing::error!("e");
            tracing::warn!("w");
            tracing::info!("i");
            tracing::debug!("d");
            tracing::trace!("t");
        });

        assert_eq!(
            records,
            vec![
                record(Level::ERROR, "e", &[]),
                record(Level::WARN, "w", &[]),
                record(Level::INFO, "i", &[]),
                record(Level::DEBUG, "d", &[]),
                record(Level::TRACE, "t", &[]),
            ]
        );
    }

    #[test]
    fn debug_and_number_fields_are_formatted() {
        let ((), records) = capture(|| tracing::warn!(count = 3, items = ?[1, 2], "x"));

        assert_eq!(
            records,
            vec![record(
                Level::WARN,
                "x",
                &[("count", "3"), ("items", "[1, 2]")]
            )]
        );
    }

    #[test]
    fn an_event_without_a_message_has_an_empty_message() {
        let ((), records) = capture(|| tracing::warn!(key = "docs/a.md"));

        assert_eq!(
            records,
            vec![record(Level::WARN, "", &[("key", "docs/a.md")])]
        );
    }

    #[test]
    fn an_event_inside_a_span_is_recorded() {
        let ((), records) = capture(|| {
            tracing::info_span!("update").in_scope(|| tracing::warn!("inside"));
        });

        assert_eq!(records, vec![record(Level::WARN, "inside", &[])]);
    }

    #[test]
    fn control_characters_in_a_message_are_kept_as_they_are() {
        let ((), records) = capture(|| tracing::warn!("a\u{7}b\u{1b}c"));

        assert_eq!(records, vec![record(Level::WARN, "a\u{7}b\u{1b}c", &[])]);
    }

    #[test]
    fn an_event_before_the_capture_is_not_recorded() {
        tracing::warn!("before");

        let ((), records) = capture(|| tracing::warn!("during"));

        assert_eq!(records, vec![record(Level::WARN, "during", &[])]);
    }

    #[test]
    fn the_receiver_outside_gets_the_events_again_after_the_capture_ends() {
        let ((), outer) = capture(|| {
            tracing::warn!("outer, before");
            let ((), inner) = capture(|| tracing::warn!("inner"));
            tracing::warn!("outer, after");

            assert_eq!(inner, vec![record(Level::WARN, "inner", &[])]);
        });

        assert_eq!(
            outer,
            vec![
                record(Level::WARN, "outer, before", &[]),
                record(Level::WARN, "outer, after", &[]),
            ]
        );
    }

    #[test]
    fn an_event_on_a_thread_the_code_spawns_is_not_recorded() {
        let ((), records) = capture(|| {
            std::thread::spawn(|| tracing::warn!("on another thread"))
                .join()
                .unwrap();
            tracing::warn!("on this thread");
        });

        assert_eq!(records, vec![record(Level::WARN, "on this thread", &[])]);
    }

    #[test]
    fn captures_running_at_the_same_time_on_two_threads_do_not_mix() {
        let barrier = Barrier::new(2);
        let capture_as = |name: &str| {
            capture(|| {
                for _ in 0..50 {
                    barrier.wait();
                    tracing::warn!(name, "step");
                }
            })
            .1
        };

        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| capture_as("first"));
            let second = scope.spawn(|| capture_as("second"));
            (first.join().unwrap(), second.join().unwrap())
        });

        assert_eq!(
            first,
            vec![record(Level::WARN, "step", &[("name", "first")]); 50]
        );
        assert_eq!(
            second,
            vec![record(Level::WARN, "step", &[("name", "second")]); 50]
        );
    }

    #[tokio::test]
    async fn events_emitted_while_a_future_is_polled_are_recorded() {
        let (output, records) = capture_async(async {
            tracing::warn!("before the await");
            tokio::task::yield_now().await;
            tracing::warn!("after the await");
            7
        })
        .await;

        assert_eq!(output, 7);
        assert_eq!(
            records,
            vec![
                record(Level::WARN, "before the await", &[]),
                record(Level::WARN, "after the await", &[]),
            ]
        );
    }

    #[tokio::test]
    async fn an_event_on_the_same_thread_while_the_future_is_pending_is_not_recorded() {
        let ((), outer) = capture_async(async {
            let inner = capture_async(async {
                tracing::warn!("inner, first poll");
                tokio::task::yield_now().await;
                tracing::warn!("inner, second poll");
            });
            tokio::pin!(inner);
            assert!(futures::poll!(&mut inner).is_pending());
            tracing::warn!("outer, between the polls");
            let ((), inner) = inner.await;

            assert_eq!(
                inner,
                vec![
                    record(Level::WARN, "inner, first poll", &[]),
                    record(Level::WARN, "inner, second poll", &[]),
                ]
            );
        })
        .await;

        assert_eq!(
            outer,
            vec![record(Level::WARN, "outer, between the polls", &[])]
        );
    }

    #[tokio::test]
    async fn an_event_in_a_spawn_blocking_task_is_not_recorded() {
        let ((), records) = capture_async(async {
            tokio::task::spawn_blocking(|| tracing::warn!("in the blocking task"))
                .await
                .unwrap();
            tracing::warn!("in the future");
        })
        .await;

        assert_eq!(records, vec![record(Level::WARN, "in the future", &[])]);
    }

    #[tokio::test]
    async fn futures_interleaved_on_one_thread_do_not_mix() {
        let emit = |name: &'static str| async move {
            for _ in 0..3 {
                tracing::warn!(name, "step");
                tokio::task::yield_now().await;
            }
        };

        let (((), first), ((), second)) =
            tokio::join!(capture_async(emit("first")), capture_async(emit("second")));

        assert_eq!(
            first,
            vec![record(Level::WARN, "step", &[("name", "first")]); 3]
        );
        assert_eq!(
            second,
            vec![record(Level::WARN, "step", &[("name", "second")]); 3]
        );
    }

    /// Stands for component code that one test reaches without a capture
    /// while another test captures it. No other test reaches this callsite.
    fn warn_from_two_threads() {
        tracing::warn!("reached from two threads");
    }

    // Without the idle receiver, this test fails when it runs alone in its
    // process (`cargo test --lib -- --exact
    // test_support::log_receiver::tests::an_event_is_recorded_after_a_thread_without_a_receiver_reached_its_callsite_first`).
    // Among the other tests, it can pass even then: their receivers, alive at
    // the time, make `tracing` ask every receiver.
    #[test]
    fn an_event_is_recorded_after_a_thread_without_a_receiver_reached_its_callsite_first() {
        let ((), records) = capture(|| {
            std::thread::spawn(warn_from_two_threads).join().unwrap();
            warn_from_two_threads();
        });

        assert_eq!(
            records,
            vec![record(Level::WARN, "reached from two threads", &[])]
        );
    }
}
