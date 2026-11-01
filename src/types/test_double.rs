//! A test double for [`ProgressSink`], for the unit tests of the functions
//! that report progress.

// Parts of the double are for tests that the components using it have not
// written yet.
#![allow(dead_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Progress, ProgressSink};

/// Records every progress it receives. Unlike the sink `gateway` makes, it
/// drops no report, so a test can check each one.
pub(crate) struct TestProgressSink {
    reports: Mutex<Vec<Progress>>,
    embedded_chunks: AtomicU64,
}

impl TestProgressSink {
    pub(crate) fn new() -> Self {
        Self {
            reports: Mutex::new(Vec::new()),
            embedded_chunks: AtomicU64::new(0),
        }
    }

    /// Every reported progress, in the order received.
    pub(crate) fn reports(&self) -> Vec<Progress> {
        self.reports.lock().unwrap().clone()
    }

    /// The sum of every count passed to `add_embedded_chunks`.
    pub(crate) fn embedded_chunks(&self) -> u64 {
        self.embedded_chunks.load(Ordering::SeqCst)
    }
}

impl ProgressSink for TestProgressSink {
    fn report(&self, progress: Progress) {
        self.reports.lock().unwrap().push(progress);
    }

    fn add_embedded_chunks(&self, count: u64) {
        self.embedded_chunks.fetch_add(count, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn reports_passed_through_the_trait_are_kept_in_order() {
        let double = Arc::new(TestProgressSink::new());
        let sink: Arc<dyn ProgressSink> = double.clone();

        sink.report(Progress {
            done: 0,
            total: Some(2),
        });
        sink.report(Progress {
            done: 1,
            total: None,
        });

        assert_eq!(
            double.reports(),
            vec![
                Progress {
                    done: 0,
                    total: Some(2)
                },
                Progress {
                    done: 1,
                    total: None
                },
            ]
        );
    }

    #[test]
    fn embedded_chunks_are_summed() {
        let double = TestProgressSink::new();
        let sink: &dyn ProgressSink = &double;

        sink.add_embedded_chunks(3);
        sink.add_embedded_chunks(4);

        assert_eq!(double.embedded_chunks(), 7);
    }

    #[test]
    fn a_new_double_has_no_reports_and_no_embedded_chunks() {
        let double = TestProgressSink::new();

        assert_eq!(double.reports(), Vec::new());
        assert_eq!(double.embedded_chunks(), 0);
    }

    #[test]
    fn calls_from_several_threads_at_once_are_all_kept() {
        let double = TestProgressSink::new();
        let sink: &dyn ProgressSink = &double;

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for done in 0..100 {
                        sink.report(Progress { done, total: None });
                        sink.add_embedded_chunks(1);
                    }
                });
            }
        });

        assert_eq!(double.reports().len(), 800);
        assert_eq!(double.embedded_chunks(), 800);
    }
}
