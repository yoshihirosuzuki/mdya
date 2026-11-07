//! Writes what the libraries that mdya uses write to the process's stdout as
//! warnings in the logs. `entry` is to put a pipe on the process's stdout, so
//! that what the libraries write there does not reach the user's stdout, and
//! to pass the reading end here.
//!
//! One thread reads the pipe and keeps the lines read, up to a limit, and
//! another writes them to the logs. Writing the logs may wait on a stderr
//! that nobody reads, but the reading never waits on it, so a library's
//! write to stdout never waits on mdya. Lines read while the limit is
//! reached are dropped, and the logs say how many.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use super::lock;

/// The most lines kept for the logs at once, each kept as the bytes read.
const KEPT_LINES: usize = 256;

/// The most bytes of one line, as read. A longer line is kept and written in
/// parts.
const LINE_BYTES: usize = 4096;

/// The bytes read from the pipe at once.
const READ_BYTES: usize = 8192;

/// Starts writing what is read from `pipe` to the logs, line by line, as
/// warnings, on two threads of their own. An error if a thread cannot be
/// started.
pub(super) fn log_stdout(pipe: impl Read + Send + 'static) -> io::Result<StdoutLogging> {
    let limits = Limits {
        lines: KEPT_LINES,
        line_bytes: LINE_BYTES,
    };
    start(pipe, limits, write_warning)
}

/// The two threads. Dropping this leaves them running until the pipe ends.
pub(crate) struct StdoutLogging {
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

impl StdoutLogging {
    /// Waits until the pipe ends and every line kept is written to the logs.
    /// The pipe ends only when every writing end of it is closed, the
    /// process's stdout included, so the caller closes them first; until
    /// then, this waits.
    pub(crate) fn finish(self) {
        // A panic on either thread has ended it already.
        let _ = self.reader.join();
        let _ = self.writer.join();
    }
}

/// How much is kept.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// The most lines kept at once.
    lines: usize,
    /// The most bytes of one line.
    line_bytes: usize,
}

/// What the logs say about the pipe.
#[derive(Debug)]
enum Notice {
    /// A line a library wrote, without its line break.
    Wrote(String),
    /// How many lines were dropped after the last one written.
    Dropped(u64),
    /// Reading the pipe failed, and the reading end is closed: a later write
    /// to the pipe fails.
    Failed(io::Error),
}

/// Writes `notice` to the logs as a warning.
fn write_warning(notice: Notice) {
    match notice {
        Notice::Wrote(text) => {
            tracing::warn!(text = text.as_str(), "a library wrote a line to stdout");
        }
        Notice::Dropped(lines) => tracing::warn!(
            lines,
            "lines that libraries wrote to stdout were dropped, as the logs could not keep up"
        ),
        Notice::Failed(error) => tracing::warn!(
            cause = error.to_string().as_str(),
            "what libraries write to stdout can no longer be read"
        ),
    }
}

fn start(
    pipe: impl Read + Send + 'static,
    limits: Limits,
    write: impl Fn(Notice) + Send + 'static,
) -> io::Result<StdoutLogging> {
    let kept = Arc::new(Kept::new(limits.lines));
    let writer = std::thread::Builder::new()
        .name("stdout-logs".to_owned())
        .spawn({
            let kept = Arc::clone(&kept);
            move || write_kept(&kept, write)
        })?;
    let reader = std::thread::Builder::new()
        .name("stdout-pipe".to_owned())
        .spawn({
            let kept = Arc::clone(&kept);
            move || read_pipe(pipe, limits.line_bytes, &kept)
        });
    match reader {
        Ok(reader) => Ok(StdoutLogging { reader, writer }),
        Err(error) => {
            // Ends the thread that writes the logs.
            kept.end(None);
            Err(error)
        }
    }
}

/// Reads `pipe` until it ends or fails, and keeps the lines read.
fn read_pipe(mut pipe: impl Read, line_bytes: usize, kept: &Kept) {
    let mut lines = LineSplitter::new(line_bytes);
    let mut buffer = vec![0; READ_BYTES];
    let failure = loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break None,
            Ok(read) => lines.split(&buffer[..read], |line| kept.push(line)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => break Some(error),
        }
    };
    lines.rest(|line| kept.push(line));
    kept.end(failure);
}

/// Writes the lines kept to the logs, with the bytes that are not UTF-8 as
/// U+FFFD, until the pipe ends.
fn write_kept(kept: &Kept, write: impl Fn(Notice)) {
    loop {
        match kept.next() {
            Next::Line(line) => {
                write(Notice::Wrote(text(&line.bytes)));
                if line.dropped_after > 0 {
                    write(Notice::Dropped(line.dropped_after));
                }
            }
            Next::End(failure) => {
                if let Some(error) = failure {
                    write(Notice::Failed(error));
                }
                return;
            }
        }
    }
}

/// The lines read and not yet written to the logs, shared by the two
/// threads.
struct Kept {
    queue: Mutex<Queue>,
    changed: Condvar,
}

impl Kept {
    fn new(lines: usize) -> Self {
        Self {
            queue: Mutex::new(Queue::new(lines)),
            changed: Condvar::new(),
        }
    }

    fn push(&self, line: Vec<u8>) {
        lock(&self.queue).push(line);
        self.changed.notify_one();
    }

    fn end(&self, failure: Option<io::Error>) {
        lock(&self.queue).end(failure);
        self.changed.notify_one();
    }

    /// Waits for the next thing to write.
    fn next(&self) -> Next {
        let mut queue = lock(&self.queue);
        loop {
            if let Some(next) = queue.pop() {
                return next;
            }
            queue = self
                .changed
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The lines kept, up to a number. A line read while the number is reached
/// is dropped, and counted with the last line kept, so that the logs say
/// where lines are missing.
struct Queue {
    lines: VecDeque<KeptLine>,
    most: usize,
    ended: bool,
    failure: Option<io::Error>,
}

#[derive(Debug, PartialEq, Eq)]
struct KeptLine {
    /// The line as read, without its line break.
    bytes: Vec<u8>,
    /// How many lines were dropped after this one.
    dropped_after: u64,
}

/// The next thing to write to the logs.
#[derive(Debug)]
enum Next {
    Line(KeptLine),
    /// The pipe ended, after a failure to read it if there was one.
    End(Option<io::Error>),
}

impl Queue {
    /// Keeps up to `most` lines, and at least one.
    fn new(most: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            most: most.max(1),
            ended: false,
            failure: None,
        }
    }

    fn push(&mut self, line: Vec<u8>) {
        if self.lines.len() < self.most {
            self.lines.push_back(KeptLine {
                bytes: line,
                dropped_after: 0,
            });
        } else if let Some(last) = self.lines.back_mut() {
            last.dropped_after += 1;
        }
    }

    fn end(&mut self, failure: Option<io::Error>) {
        self.ended = true;
        self.failure = failure;
    }

    /// The next thing to write: the first line kept, or, once every line is
    /// taken, the end of the pipe. `None` while there is neither.
    fn pop(&mut self) -> Option<Next> {
        if let Some(line) = self.lines.pop_front() {
            return Some(Next::Line(line));
        }
        self.ended.then(|| Next::End(self.failure.take()))
    }
}

/// Splits the bytes read into lines, without their line breaks (`\n` or
/// `\r\n`), and a line longer than `line_bytes` into parts, each cut where a
/// character begins.
struct LineSplitter {
    partial: Vec<u8>,
    line_bytes: usize,
}

impl LineSplitter {
    /// `line_bytes` is at least 4, the most bytes of one character.
    fn new(line_bytes: usize) -> Self {
        Self {
            partial: Vec::new(),
            line_bytes: line_bytes.max(4),
        }
    }

    /// Passes each line that `bytes` ends, and each part cut from a long
    /// line, to `line`, and keeps the rest. A carriage return at the end is
    /// not counted until the next byte, so that a line of `line_bytes` ended
    /// in `\r\n` is not cut.
    fn split(&mut self, bytes: &[u8], mut line: impl FnMut(Vec<u8>)) {
        for &byte in bytes {
            if byte == b'\n' {
                if self.partial.last() == Some(&b'\r') {
                    self.partial.pop();
                }
                line(std::mem::take(&mut self.partial));
                continue;
            }
            self.partial.push(byte);
            let pending_return = usize::from(byte == b'\r');
            if self.partial.len() > self.line_bytes + pending_return {
                self.cut(&mut line);
            }
        }
    }

    /// Passes the line not ended, if any, to `line`: in two parts if a
    /// carriage return at its end takes it past `line_bytes`.
    fn rest(mut self, mut line: impl FnMut(Vec<u8>)) {
        if self.partial.len() > self.line_bytes {
            self.cut(&mut line);
        }
        if !self.partial.is_empty() {
            line(self.partial);
        }
    }

    /// Passes the line up to `line_bytes`, cut where a character begins, to
    /// `line`, and keeps the rest.
    fn cut(&mut self, line: &mut impl FnMut(Vec<u8>)) {
        let rest = self
            .partial
            .split_off(char_start(&self.partial, self.line_bytes));
        line(std::mem::replace(&mut self.partial, rest));
    }
}

/// Where the character that holds byte `at` of `bytes` begins: `at`, or one
/// of the 3 bytes before it. `at` if none of them begins a character, as in
/// bytes that are not UTF-8.
fn char_start(bytes: &[u8], at: usize) -> usize {
    (at.saturating_sub(3)..=at)
        .rev()
        .find(|&i| bytes.get(i).is_some_and(|&byte| !is_continuation(byte)))
        .unwrap_or(at)
}

/// Whether `byte` continues a character in UTF-8, rather than begins one.
fn is_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::sync::mpsc;
    use std::time::Duration;

    use tracing::Level;

    use super::*;
    use crate::test_support::log_receiver::{LogRecord, capture};

    // Splitting the bytes read into lines.

    /// The lines that the reads are split into, and the parts of the rest,
    /// each as text.
    fn split(line_bytes: usize, reads: &[&[u8]]) -> (Vec<String>, Vec<String>) {
        let mut splitter = LineSplitter::new(line_bytes);
        let mut lines = Vec::new();
        for bytes in reads {
            splitter.split(bytes, |line| lines.push(text(&line)));
        }
        let mut rest = Vec::new();
        splitter.rest(|part| rest.push(text(&part)));
        (lines, rest)
    }

    fn texts(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|&text| text.to_owned()).collect()
    }

    #[test]
    fn lines_are_split_without_their_line_breaks() {
        assert_eq!(
            split(64, &[b"a\nb\r\n\nc"]),
            (texts(&["a", "b", ""]), texts(&["c"]))
        );
    }

    #[test]
    fn a_line_read_in_pieces_is_one_line() {
        assert_eq!(
            split(64, &[b"ab", b"c\r", b"\nd\n"]),
            (texts(&["abc", "d"]), texts(&[]))
        );
    }

    #[test]
    fn a_long_line_is_cut_into_parts_of_the_most_bytes() {
        assert_eq!(
            split(4, &[b"abcdefghij\n"]),
            (texts(&["abcd", "efgh", "ij"]), texts(&[]))
        );
    }

    #[test]
    fn a_line_of_the_most_bytes_is_not_cut() {
        assert_eq!(split(4, &[b"abcd\n"]), (texts(&["abcd"]), texts(&[])));
    }

    #[test]
    fn a_line_of_the_most_bytes_ended_in_cr_lf_is_one_line() {
        assert_eq!(split(4, &[b"abcd\r\n"]), (texts(&["abcd"]), texts(&[])));
        assert_eq!(
            split(4, &[b"abcd\r", b"\n"]),
            (texts(&["abcd"]), texts(&[]))
        );
    }

    #[test]
    fn a_carriage_return_not_followed_by_a_line_feed_goes_on_to_the_next_part() {
        assert_eq!(
            split(4, &[b"abcd\rx\n"]),
            (texts(&["abcd", "\rx"]), texts(&[]))
        );
    }

    #[test]
    fn a_carriage_return_at_the_end_of_the_rest_is_kept() {
        assert_eq!(split(4, &[b"abc\r"]), (texts(&[]), texts(&["abc\r"])));
        assert_eq!(split(4, &[b"\r"]), (texts(&[]), texts(&["\r"])));
    }

    #[test]
    fn a_rest_past_the_most_bytes_with_its_carriage_return_is_in_two_parts() {
        assert_eq!(split(4, &[b"abcd\r"]), (texts(&[]), texts(&["abcd", "\r"])));
    }

    #[test]
    fn a_long_line_is_cut_where_a_character_begins() {
        // "aあい": `a`, then two characters of three bytes each.
        assert_eq!(
            split(5, &["aあい\n".as_bytes()]),
            (texts(&["aあ", "い"]), texts(&[]))
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_are_kept_as_they_are() {
        let mut splitter = LineSplitter::new(64);
        let mut lines = Vec::new();

        splitter.split(b"a\xffb\n", |line| lines.push(line));

        assert_eq!(lines, [b"a\xffb".to_vec()]);
    }

    #[test]
    fn a_long_line_where_no_character_begins_is_cut_at_the_most_bytes() {
        // Bytes that each continue a character, and none that begins one.
        let mut splitter = LineSplitter::new(4);
        let mut lines = Vec::new();

        splitter.split(&[0x80; 6], |line| lines.push(line));
        splitter.rest(|part| lines.push(part));

        assert_eq!(lines, [vec![0x80; 4], vec![0x80; 2]]);
    }

    // Keeping the lines up to a number.

    fn line(text: &str, dropped_after: u64) -> KeptLine {
        KeptLine {
            bytes: text.as_bytes().to_vec(),
            dropped_after,
        }
    }

    fn popped_line(queue: &mut Queue) -> Option<KeptLine> {
        match queue.pop() {
            Some(Next::Line(line)) => Some(line),
            other => panic!("not a line: {other:?}"),
        }
    }

    #[test]
    fn lines_are_taken_in_the_order_kept() {
        let mut queue = Queue::new(4);
        queue.push(b"a".to_vec());
        queue.push(b"b".to_vec());

        assert_eq!(popped_line(&mut queue), Some(line("a", 0)));
        assert_eq!(popped_line(&mut queue), Some(line("b", 0)));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn lines_past_the_most_are_dropped_and_counted_with_the_last_line_kept() {
        let mut queue = Queue::new(2);
        for text in ["a", "b", "c", "d"] {
            queue.push(text.as_bytes().to_vec());
        }

        assert_eq!(popped_line(&mut queue), Some(line("a", 0)));
        queue.push(b"e".to_vec());
        queue.push(b"f".to_vec());
        assert_eq!(popped_line(&mut queue), Some(line("b", 2)));
        assert_eq!(popped_line(&mut queue), Some(line("e", 1)));
    }

    #[test]
    fn the_end_comes_after_the_lines_kept() {
        let mut queue = Queue::new(2);
        queue.push(b"a".to_vec());
        queue.end(Some(io::Error::other("broken")));

        assert_eq!(popped_line(&mut queue), Some(line("a", 0)));
        match queue.pop() {
            Some(Next::End(Some(error))) => assert_eq!(error.to_string(), "broken"),
            other => panic!("not the end with the failure: {other:?}"),
        }
    }

    // Writing to the logs.

    fn warning(message: &str, fields: &[(&str, &str)]) -> LogRecord {
        LogRecord {
            level: Level::WARN,
            message: message.to_owned(),
            fields: fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn each_notice_is_written_as_a_warning() {
        let ((), records) = capture(|| {
            write_warning(Notice::Wrote("Rayon: a\u{1b}".to_owned()));
            write_warning(Notice::Dropped(3));
            write_warning(Notice::Failed(io::Error::other("broken")));
        });

        assert_eq!(
            records,
            vec![
                warning(
                    "a library wrote a line to stdout",
                    &[("text", "Rayon: a\u{1b}")]
                ),
                warning(
                    "lines that libraries wrote to stdout were dropped, as the logs could not keep up",
                    &[("lines", "3")]
                ),
                warning(
                    "what libraries write to stdout can no longer be read",
                    &[("cause", "broken")]
                ),
            ]
        );
    }

    // The two threads.

    /// A notice, in a form that tests can compare.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Written {
        Wrote(String),
        Dropped(u64),
        Failed(io::ErrorKind),
    }

    impl From<Notice> for Written {
        fn from(notice: Notice) -> Self {
            match notice {
                Notice::Wrote(text) => Self::Wrote(text),
                Notice::Dropped(lines) => Self::Dropped(lines),
                Notice::Failed(error) => Self::Failed(error.kind()),
            }
        }
    }

    /// The notices written, and the function that writes them there.
    fn recorder() -> (Arc<Mutex<Vec<Written>>>, impl Fn(Notice) + Send + 'static) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let write = {
            let written = Arc::clone(&written);
            move |notice: Notice| written.lock().unwrap().push(Written::from(notice))
        };
        (written, write)
    }

    /// More lines than a test writes, so that none is dropped.
    const LIMITS: Limits = Limits {
        lines: 8,
        line_bytes: 16,
    };

    #[test]
    fn the_lines_written_to_the_pipe_are_written_to_the_logs_in_order() {
        let (reading, mut writing) = io::pipe().unwrap();
        let (written, write) = recorder();

        let logging = start(reading, LIMITS, write).unwrap();
        writing.write_all(b"a\nb\nc").unwrap();
        drop(writing);
        logging.finish();

        assert_eq!(
            *written.lock().unwrap(),
            [
                Written::Wrote("a".to_owned()),
                Written::Wrote("b".to_owned()),
                Written::Wrote("c".to_owned()),
            ]
        );
    }

    #[test]
    fn while_the_logs_wait_the_pipe_is_read_on_and_the_lines_past_the_limit_are_dropped() {
        // More than a pipe holds, so that the writes end only if the pipe is
        // read while the logs wait.
        const LINES: usize = 100_000;
        let (reading, mut writing) = io::pipe().unwrap();
        let (written, record) = recorder();
        let (release, released) = mpsc::channel::<()>();
        // Each write to the logs waits until `release` is dropped.
        let write = move |notice: Notice| {
            record(notice);
            let _ = released.recv();
        };

        let limits = Limits {
            lines: 2,
            line_bytes: 16,
        };

        let logging = start(reading, limits, write).unwrap();
        let (done, writes_ended) = mpsc::channel();
        std::thread::spawn(move || {
            writing.write_all(&b"xxxxxxxx\n".repeat(LINES)).unwrap();
            drop(writing);
            done.send(()).unwrap();
        });
        let ended = writes_ended.recv_timeout(Duration::from_secs(60));
        drop(release);

        // Checked before `finish`, which would wait on a reading that waits.
        assert!(ended.is_ok(), "the writes to the pipe did not end");
        logging.finish();
        let written = written.lock().unwrap();
        let lines = written
            .iter()
            .filter(|notice| matches!(notice, Written::Wrote(_)))
            .count();
        let dropped: u64 = written
            .iter()
            .map(|notice| match notice {
                Written::Dropped(lines) => *lines,
                _ => 0,
            })
            .sum();
        assert!(dropped > 0, "{written:?}");
        // Every line read is either written or counted as dropped.
        assert_eq!(lines + usize::try_from(dropped).unwrap(), LINES);
    }

    /// Returns the results given, one per read, and then the end.
    struct Reads(VecDeque<io::Result<Vec<u8>>>);

    impl Read for Reads {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(Err(error)) => Err(error),
            }
        }
    }

    #[test]
    fn an_interrupted_read_is_tried_again() {
        let reads = Reads(VecDeque::from([
            Ok(b"a\n".to_vec()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(b"b\n".to_vec()),
        ]));
        let (written, write) = recorder();

        start(reads, LIMITS, write).unwrap().finish();

        assert_eq!(
            *written.lock().unwrap(),
            [
                Written::Wrote("a".to_owned()),
                Written::Wrote("b".to_owned()),
            ]
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_are_written_as_the_replacement_character() {
        let reads = Reads(VecDeque::from([Ok(b"a\xffb\n".to_vec())]));
        let (written, write) = recorder();

        start(reads, LIMITS, write).unwrap().finish();

        assert_eq!(
            *written.lock().unwrap(),
            [Written::Wrote("a\u{fffd}b".to_owned())]
        );
    }

    #[test]
    fn a_failure_to_read_is_written_after_the_lines_read_before_it() {
        let reads = Reads(VecDeque::from([
            Ok(b"a\nb".to_vec()),
            Err(io::Error::other("broken")),
            Ok(b"never read\n".to_vec()),
        ]));
        let (written, write) = recorder();

        start(reads, LIMITS, write).unwrap().finish();

        assert_eq!(
            *written.lock().unwrap(),
            [
                Written::Wrote("a".to_owned()),
                Written::Wrote("b".to_owned()),
                Written::Failed(io::ErrorKind::Other),
            ]
        );
    }
}
