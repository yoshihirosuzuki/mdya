//! Test doubles for the types: a [`ProgressSink`], for the unit tests of the
//! functions that report progress, and a [`DirLookup`] with fixed answers, for
//! the unit tests of the functions that make paths absolute.

// Parts of the doubles are for tests that the components using them have not
// written yet.
#![allow(dead_code)]

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{DirLookup, Progress, ProgressSink};

/// A `DirLookup` whose functions give these answers every time they are
/// called: `None` for an unknown home directory, and an error of `ErrorKind`
/// for a working directory that cannot be read.
pub(crate) fn test_dir_lookup(
    home_dir: Option<&str>,
    current_dir: Result<&str, io::ErrorKind>,
) -> DirLookup {
    let home_dir = home_dir.map(PathBuf::from);
    let current_dir = current_dir.map(PathBuf::from);
    DirLookup {
        home_dir: Arc::new(move || home_dir.clone()),
        current_dir: Arc::new(move || current_dir.clone().map_err(io::Error::from)),
    }
}

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
    use super::*;

    #[test]
    fn a_test_dir_lookup_gives_its_answers_every_time() {
        let dirs = test_dir_lookup(Some("/home/me"), Ok("/work"));

        for _ in 0..2 {
            assert_eq!((dirs.home_dir)(), Some(PathBuf::from("/home/me")));
            assert_eq!((dirs.current_dir)().unwrap(), PathBuf::from("/work"));
        }
    }

    #[test]
    fn a_test_dir_lookup_can_answer_that_neither_directory_is_known() {
        let dirs = test_dir_lookup(None, Err(io::ErrorKind::NotFound));

        for _ in 0..2 {
            assert_eq!((dirs.home_dir)(), None);
            assert_eq!(
                (dirs.current_dir)().unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
    }

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
