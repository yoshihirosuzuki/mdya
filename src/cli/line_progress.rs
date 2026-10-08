//! Status lines for an ingest run that cannot show progress bars.
//!
//! The progress bars (`IndicatifProgress`) redraw in place, so `indicatif`
//! hides them when stderr is a file, a pipe or a CI log, or when `TERM` is
//! `dumb` (or, outside Windows, unset), and such a run used to print
//! nothing until its final summary. `LineProgress` prints the run's state as one line when the
//! first collection starts and then every [`STATUS_INTERVAL`], so a long
//! run shows that it is still moving. The lines are status notices rather
//! than diagnostics, so they go to stderr directly instead of through
//! `tracing`.
//!
//! A dedicated thread prints the lines on a fixed clock. Printing from the
//! progress callbacks instead would go quiet whenever one embedding batch
//! takes longer than the interval, since no callback arrives meanwhile.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::ingest::{FileOutcome, IngestProgress};

/// Shown once every file is processed, while the ingest run builds or
/// refreshes the search indexes. Shared with the progress bars.
pub(super) const INDEX_MAINTENANCE_MESSAGE: &str = "Updating search indexes";

/// How often a status line is printed after the first one. Fixed rather
/// than configurable; the value is a judgement call, not a measurement.
const STATUS_INTERVAL: Duration = Duration::from_secs(10);

/// `IngestProgress` that prints the run's state as periodic status lines.
pub(crate) struct LineProgress {
    /// Updated by the progress callbacks, read by the printing thread.
    status: Arc<Mutex<RunStatus>>,
    ticker: Mutex<Ticker>,
}

impl LineProgress {
    /// Print to stderr, every [`STATUS_INTERVAL`].
    pub(crate) fn stderr() -> Self {
        Self::new(Box::new(io::stderr()), STATUS_INTERVAL)
    }

    fn new(out: Box<dyn Write + Send>, interval: Duration) -> Self {
        Self {
            status: Arc::new(Mutex::new(RunStatus::default())),
            ticker: Mutex::new(Ticker::Idle { out, interval }),
        }
    }

    fn update(&self, change: impl FnOnce(&mut RunStatus)) {
        change(&mut self.status.lock().expect("status mutex poisoned"));
    }

    /// Stop the printing thread and wait for it, so no line follows.
    fn stop_printing(&self) {
        // Called from `Drop`, so a poisoned lock must not panic again
        // while unwinding. The state is swapped out whole, so it is safe
        // to use whatever the panicking holder left.
        let ticker = {
            let mut guard = self.ticker.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::replace(&mut *guard, Ticker::Stopped)
        };
        // The lock is released before the join.
        ticker.stop();
    }
}

impl IngestProgress for LineProgress {
    fn start_collection(&self, name: &str, total_files: usize) {
        self.update(|status| status.start_collection(name, total_files));
        let mut ticker = self.ticker.lock().expect("ticker mutex poisoned");
        let idle = std::mem::replace(&mut *ticker, Ticker::Stopped);
        *ticker = idle.start(&self.status);
    }

    fn start_file(&self, path: &Path) {
        self.update(|status| status.start_file(path));
    }

    fn chunks_embedded(&self, path: &Path, done: usize, total: usize) {
        self.update(|status| status.chunks_embedded(path, done, total));
    }

    fn finish_file(&self, path: &Path, _outcome: FileOutcome) {
        // A failed file is already reported by its own warning line.
        self.update(|status| status.finish_file(path));
    }

    fn start_index_maintenance(&self) {
        self.update(RunStatus::start_index_maintenance);
    }

    fn finish(&self) {
        self.stop_printing();
    }
}

impl Drop for LineProgress {
    /// A run that fails before `finish` still stops the printing thread.
    fn drop(&mut self) {
        self.stop_printing();
    }
}

/// The thread printing the status lines, from the first collection on.
enum Ticker {
    /// Not started: holds what the thread will take.
    Idle {
        out: Box<dyn Write + Send>,
        interval: Duration,
    },
    /// Printing until `stop` is dropped.
    Running {
        stop: Sender<()>,
        thread: JoinHandle<()>,
    },
    Stopped,
}

impl Ticker {
    /// Print the first line now, then hand the output to a thread that
    /// prints one every `interval`. Anything but `Idle` is returned as is.
    fn start(self, status: &Arc<Mutex<RunStatus>>) -> Self {
        let Self::Idle { mut out, interval } = self else {
            return self;
        };
        print_status_line(status, &mut out);
        let (stop, stopped) = mpsc::channel();
        let status = Arc::clone(status);
        let thread = thread::Builder::new()
            .name("mdya-progress-lines".to_string())
            .spawn(move || print_status_lines(&status, &mut out, interval, &stopped))
            .expect("spawn the progress line thread");
        Self::Running { stop, thread }
    }

    fn stop(self) {
        let Self::Running { stop, thread } = self else {
            return;
        };
        // Dropping the sender disconnects the channel, which wakes the
        // thread at once instead of at the end of its interval.
        drop(stop);
        // A panic on the thread has already printed its message.
        let _ = thread.join();
    }
}

fn print_status_lines(
    status: &Mutex<RunStatus>,
    out: &mut Box<dyn Write + Send>,
    interval: Duration,
    stopped: &Receiver<()>,
) {
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(interval) {
        print_status_line(status, out);
    }
}

fn print_status_line(status: &Mutex<RunStatus>, out: &mut Box<dyn Write + Send>) {
    let line = format!(
        "{}\n",
        status.lock().expect("status mutex poisoned").render()
    );
    // One `write_all` keeps the line whole next to log lines on stderr.
    // A failed write (a closed pipe, say) only loses this status line.
    let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
}

/// What a status line reports. The file counts belong to the collection
/// being processed and restart with each collection.
#[derive(Debug, Default)]
struct RunStatus {
    collection: String,
    files_done: usize,
    files_total: usize,
    /// In the order the files started, so the line lists them stably.
    in_flight: Vec<FileInFlight>,
    updating_indexes: bool,
}

#[derive(Debug)]
struct FileInFlight {
    path: PathBuf,
    /// `(done, total)` once the file's chunks are being embedded.
    chunks: Option<(usize, usize)>,
}

impl RunStatus {
    fn start_collection(&mut self, name: &str, total_files: usize) {
        *self = Self {
            collection: name.to_string(),
            files_total: total_files,
            ..Self::default()
        };
    }

    fn start_file(&mut self, path: &Path) {
        self.in_flight.push(FileInFlight {
            path: path.to_path_buf(),
            chunks: None,
        });
    }

    fn chunks_embedded(&mut self, path: &Path, done: usize, total: usize) {
        if let Some(file) = self.in_flight.iter_mut().find(|f| f.path == path) {
            file.chunks = Some((done, total));
        }
    }

    fn finish_file(&mut self, path: &Path) {
        self.in_flight.retain(|f| f.path != path);
        self.files_done += 1;
    }

    fn start_index_maintenance(&mut self) {
        self.updating_indexes = true;
    }

    fn render(&self) -> String {
        if self.updating_indexes {
            return INDEX_MAINTENANCE_MESSAGE.to_string();
        }
        let counts = format!(
            "Indexing '{}': {}/{} files",
            self.collection, self.files_done, self.files_total
        );
        if self.in_flight.is_empty() {
            return counts;
        }
        let files: Vec<String> = self.in_flight.iter().map(FileInFlight::render).collect();
        format!("{counts}; in progress: {}", files.join(", "))
    }
}

impl FileInFlight {
    fn render(&self) -> String {
        match self.chunks {
            Some((done, total)) => format!("{} ({done}/{total} chunks)", self.path.display()),
            None => self.path.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn status_line_names_the_collection_and_counts_its_files() {
        let mut status = RunStatus::default();
        status.start_collection("notes", 3);
        status.start_file(Path::new("a.md"));
        status.finish_file(Path::new("a.md"));
        assert_eq!(status.render(), "Indexing 'notes': 1/3 files");
    }

    #[test]
    fn status_line_lists_the_files_in_flight_with_their_chunks_embedded() {
        let mut status = RunStatus::default();
        status.start_collection("notes", 3);
        status.start_file(Path::new("big.pdf"));
        status.start_file(Path::new("b.md"));
        status.chunks_embedded(Path::new("big.pdf"), 320, 12000);
        assert_eq!(
            status.render(),
            "Indexing 'notes': 0/3 files; in progress: big.pdf (320/12000 chunks), b.md"
        );
    }

    #[test]
    fn status_line_restarts_the_file_count_for_each_collection() {
        let mut status = RunStatus::default();
        status.start_collection("first", 1);
        status.start_file(Path::new("a.md"));
        status.finish_file(Path::new("a.md"));
        status.start_collection("second", 2);
        assert_eq!(status.render(), "Indexing 'second': 0/2 files");
    }

    #[test]
    fn status_line_reports_the_index_update_after_the_files() {
        let mut status = RunStatus::default();
        status.start_collection("notes", 1);
        status.start_file(Path::new("a.md"));
        status.finish_file(Path::new("a.md"));
        status.start_index_maintenance();
        assert_eq!(status.render(), INDEX_MAINTENANCE_MESSAGE);
    }

    /// A `Write` whose bytes the test can read while the thread writes.
    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn lines(&self) -> Vec<String> {
            let bytes = self.0.lock().expect("buffer mutex poisoned");
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("buffer mutex poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn prints_one_line_when_the_first_collection_starts() {
        let out = SharedBuffer::default();
        let progress = LineProgress::new(Box::new(out.clone()), Duration::from_secs(3600));
        progress.start_collection("notes", 2);
        progress.start_collection("more", 1);
        progress.finish();
        assert_eq!(out.lines(), ["Indexing 'notes': 0/2 files"]);
    }

    #[test]
    fn prints_nothing_for_a_run_without_collections() {
        let out = SharedBuffer::default();
        let progress = LineProgress::new(Box::new(out.clone()), Duration::from_millis(1));
        progress.start_index_maintenance();
        progress.finish();
        assert!(out.lines().is_empty());
    }

    /// Wait until the thread has printed `count` lines in total.
    fn wait_for_lines(out: &SharedBuffer, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while out.lines().len() < count {
            assert!(Instant::now() < deadline, "no periodic line within 10 s");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn keeps_printing_on_its_own_clock_until_finish() {
        let out = SharedBuffer::default();
        let progress = LineProgress::new(Box::new(out.clone()), Duration::from_millis(10));
        progress.start_collection("notes", 1);
        wait_for_lines(&out, 3);
        progress.finish();
        let printed = out.lines().len();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            out.lines().len(),
            printed,
            "a line was printed after finish"
        );
    }

    #[test]
    fn stops_printing_when_dropped_without_finish() {
        let out = SharedBuffer::default();
        let progress = LineProgress::new(Box::new(out.clone()), Duration::from_millis(10));
        progress.start_collection("notes", 1);
        wait_for_lines(&out, 2);
        drop(progress);
        let printed = out.lines().len();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(out.lines().len(), printed, "a line was printed after drop");
    }
}
