//! Shows the progress of one operation on stderr: the one-line progress
//! display, drawn again in place on a terminal whose width can be measured,
//! or lines added one by one, at the start and every ten seconds, elsewhere.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::types::{Progress, ProgressSink};

use super::super::operations::GatewayProgressSink;
use super::lock;
use super::screen::Screen;
use super::{MeasureWidth, ProgressDisplay, ProgressKind, progress_text};

/// How often the one-line progress display is drawn again.
const IN_PLACE_INTERVAL: Duration = Duration::from_millis(100);
/// How often a line is added.
const LINE_INTERVAL: Duration = Duration::from_secs(10);

/// How the progress is shown.
enum Mode {
    /// The one-line progress display, drawn again in place.
    InPlace(Terminal),
    /// Lines added one by one.
    LineByLine,
}

impl Mode {
    /// The one-line progress display where there is a terminal whose width
    /// can be measured now; lines elsewhere.
    fn of(terminal: Option<MeasureWidth>) -> Self {
        let Some(measure) = terminal else {
            return Self::LineByLine;
        };
        match measure() {
            Some(width) => Self::InPlace(Terminal {
                measure,
                width: AtomicUsize::new(width),
            }),
            None => Self::LineByLine,
        }
    }

    fn interval(&self) -> Duration {
        match self {
            Self::InPlace(_) => IN_PLACE_INTERVAL,
            Self::LineByLine => LINE_INTERVAL,
        }
    }
}

/// The terminal that the one-line progress display is drawn on.
struct Terminal {
    measure: MeasureWidth,
    /// The width measured last, in columns.
    width: AtomicUsize,
}

impl Terminal {
    /// The width now, or the one measured last if it cannot be measured now.
    fn width(&self) -> usize {
        match (self.measure)() {
            Some(width) => {
                self.width.store(width, Ordering::Relaxed);
                width
            }
            None => self.width.load(Ordering::Relaxed),
        }
    }
}

/// The progress of one operation on stderr. The real [`ProgressDisplay`].
pub(super) struct StderrProgress {
    shown: Arc<Shown>,
    ticker: Mutex<Option<Ticker>>,
    /// Set once the progress is finished, so that finishing again, on drop
    /// for one, does not clear the display of the progress started next.
    finished: AtomicBool,
}

impl StderrProgress {
    /// Shows the progress of an operation of `kind` on `screen`. `terminal`
    /// measures the width of the terminal that `screen` is, where the
    /// progress can be drawn again in place.
    pub(super) fn start(
        screen: Arc<Mutex<Screen>>,
        kind: ProgressKind,
        terminal: Option<MeasureWidth>,
    ) -> Self {
        let mode = Mode::of(terminal);
        let interval = mode.interval();
        let started = Instant::now();
        let elapsed = Box::new(move || started.elapsed());
        Self::start_with(screen, kind, mode, Some(interval), elapsed)
    }

    /// Shows the progress now, and again every `interval` until it is
    /// finished; without an interval, never again. `elapsed` tells the time
    /// since the start.
    fn start_with(
        screen: Arc<Mutex<Screen>>,
        kind: ProgressKind,
        mode: Mode,
        interval: Option<Duration>,
        elapsed: Elapsed,
    ) -> Self {
        let state = State {
            maintaining: kind == ProgressKind::Optimize,
            ..State::default()
        };
        let shown = Arc::new(Shown {
            screen,
            kind,
            mode,
            state: Mutex::new(state),
            mark: Mutex::default(),
            elapsed,
        });
        shown.show();
        // Without the thread, the progress stays as it was shown at the
        // start; the operation itself goes on.
        let ticker = interval.and_then(|interval| {
            let shown = Arc::clone(&shown);
            Ticker::start(interval, move || shown.show())
        });
        Self {
            shown,
            ticker: Mutex::new(ticker),
            finished: AtomicBool::new(false),
        }
    }
}

/// Tells the time since the progress started.
type Elapsed = Box<dyn Fn() -> Duration + Send + Sync>;

/// What the display shares with its ticker.
struct Shown {
    screen: Arc<Mutex<Screen>>,
    kind: ProgressKind,
    mode: Mode,
    state: Mutex<State>,
    mark: Mutex<Mark>,
    elapsed: Elapsed,
}

impl Shown {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Draws the one-line progress display again, or adds a line, with the
    /// last progress received.
    fn show(&self) {
        let state = *self.state();
        // Progress that cannot be written to stderr has nowhere else to go.
        let _ = match &self.mode {
            Mode::InPlace(terminal) => {
                let mark = (self.kind == ProgressKind::Update).then(|| {
                    let mut mark = lock(&self.mark);
                    mark.turn(state.chunks)
                });
                let text = progress_text::in_place(self.kind, state, mark, terminal.width());
                lock(&self.screen).draw_progress(text)
            }
            Mode::LineByLine => {
                let line = progress_text::line(self.kind, state, (self.elapsed)());
                lock(&self.screen).write(format!("{line}\n").as_bytes())
            }
        };
    }
}

/// The marks at the head of the one-line progress display of an update, in
/// turn.
const MARKS: [char; 4] = ['|', '/', '-', '\\'];

/// The mark at the head of the one-line progress display of an update. It
/// shows that the update is working, in place of the number of the chunks
/// embedded, which has no total to be read against.
#[derive(Default)]
struct Mark {
    /// The chunks embedded when the mark last turned.
    chunks: u64,
    step: usize,
}

impl Mark {
    /// The mark to draw, one step on if more chunks were embedded since the
    /// last drawing. A mark taken from the number of chunks itself would
    /// seem to stand still when as many chunks as there are marks were
    /// embedded between two drawings.
    fn turn(&mut self, chunks: u64) -> char {
        if chunks > self.chunks {
            self.chunks = chunks;
            self.step = (self.step + 1) % MARKS.len();
        }
        MARKS[self.step]
    }
}

/// The last progress received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct State {
    pub(super) progress: Option<Progress>,
    /// The chunks whose embedding vectors are done, summed.
    pub(super) chunks: u64,
    pub(super) maintaining: bool,
}

impl ProgressSink for StderrProgress {
    fn report(&self, progress: Progress) {
        self.shown.state().progress = Some(progress);
    }

    fn add_embedded_chunks(&self, count: u64) {
        let mut state = self.shown.state();
        state.chunks = state.chunks.saturating_add(count);
    }
}

impl GatewayProgressSink for StderrProgress {
    fn maintenance_started(&self) {
        self.shown.state().maintaining = true;
    }
}

impl ProgressDisplay for StderrProgress {
    fn finish(&self) {
        if self.finished.swap(true, Ordering::Relaxed) {
            return;
        }
        let ticker = lock(&self.ticker).take();
        if let Some(ticker) = ticker {
            ticker.stop();
        }
        if let Mode::InPlace(_) = self.shown.mode {
            let _ = lock(&self.shown.screen).clear_progress();
        }
    }
}

impl Drop for StderrProgress {
    /// Finishes the progress if the caller has not, so that a display left
    /// behind, on an error for one, is not drawn again below the messages
    /// that follow. Finishing again does nothing.
    fn drop(&mut self) {
        self.finish();
    }
}

/// Calls a function at an interval, on a thread of its own.
struct Ticker {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Ticker {
    /// `None` if the thread cannot be started.
    fn start(interval: Duration, tick: impl Fn() + Send + 'static) -> Option<Self> {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("progress".to_owned())
            .spawn(move || {
                while stopped.recv_timeout(interval) == Err(RecvTimeoutError::Timeout) {
                    tick();
                }
            })
            .ok()?;
        Some(Self { stop, thread })
    }

    /// Stops the calls, and waits for the one under way to end.
    fn stop(self) {
        drop(self.stop);
        // A panic in `tick` has ended the calls already.
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::gateway::test_double::captured::CapturedOutput;

    fn screen() -> (Arc<Mutex<Screen>>, CapturedOutput) {
        let output = CapturedOutput::new();
        let screen = Arc::new(Mutex::new(Screen::new(Box::new(output.clone()))));
        (screen, output)
    }

    /// Measures the widths given, one at a time, and then nothing.
    fn widths(widths: &[Option<usize>]) -> MeasureWidth {
        let widths = Mutex::new(widths.iter().copied().collect::<VecDeque<_>>());
        Arc::new(move || widths.lock().unwrap().pop_front().flatten())
    }

    fn in_place(width: usize) -> Mode {
        Mode::of(Some(Arc::new(move || Some(width))))
    }

    /// A clock that stays at the start.
    fn no_time() -> Elapsed {
        Box::new(|| Duration::ZERO)
    }

    #[test]
    fn a_line_has_the_time_since_the_start() {
        let (screen, output) = screen();

        let _display = StderrProgress::start_with(
            screen,
            ProgressKind::Optimize,
            Mode::LineByLine,
            None,
            Box::new(|| Duration::from_secs(80)),
        );

        assert_eq!(output.text(), "Optimizing, 1m 20s\n");
    }

    /// A display without a ticker, shown again only by `show`.
    fn display(kind: ProgressKind, mode: Mode) -> (StderrProgress, CapturedOutput) {
        let (screen, output) = screen();
        (
            StderrProgress::start_with(screen, kind, mode, None, no_time()),
            output,
        )
    }

    impl StderrProgress {
        fn show(&self) {
            self.shown.show();
        }
    }

    fn line(state: State) -> String {
        format!(
            "{}\n",
            progress_text::line(ProgressKind::Update, state, Duration::ZERO)
        )
    }

    const HALF: Progress = Progress {
        done: 5,
        total: Some(10),
    };

    #[test]
    fn without_a_terminal_lines_are_added() {
        assert!(matches!(Mode::of(None), Mode::LineByLine));
    }

    #[test]
    fn on_a_terminal_whose_width_cannot_be_measured_lines_are_added() {
        assert!(matches!(Mode::of(Some(widths(&[None]))), Mode::LineByLine));
    }

    #[test]
    fn on_a_terminal_whose_width_is_measured_the_display_is_drawn_in_place() {
        let Mode::InPlace(terminal) = Mode::of(Some(widths(&[Some(60)]))) else {
            panic!("not in place");
        };

        assert_eq!(terminal.width.load(Ordering::Relaxed), 60);
    }

    #[test]
    fn a_width_that_cannot_be_measured_later_is_the_last_one_measured() {
        let Mode::InPlace(terminal) = Mode::of(Some(widths(&[Some(60), Some(100), None]))) else {
            panic!("not in place");
        };

        assert_eq!(terminal.width(), 100);
        assert_eq!(terminal.width(), 100);
    }

    #[test]
    fn the_display_is_drawn_in_place_at_the_start() {
        let (_display, output) = display(ProgressKind::Update, in_place(60));

        assert_eq!(
            output.text(),
            format!(
                "\r{}",
                progress_text::in_place(ProgressKind::Update, State::default(), Some('|'), 60)
            )
        );
    }

    /// The display of an update at 60 columns with `mark` at its head.
    fn marked(mark: char) -> String {
        progress_text::in_place(ProgressKind::Update, State::default(), Some(mark), 60)
    }

    #[test]
    fn the_mark_turns_only_when_chunks_were_embedded_since_the_last_drawing() {
        let (display, output) = display(ProgressKind::Update, in_place(60));
        let mut drawn = Vec::new();
        let mut draw = |chunks| {
            display.add_embedded_chunks(chunks);
            display.show();
            drawn.push(output.text().rsplit('\r').next().unwrap().to_owned());
        };

        draw(0);
        draw(1);
        draw(0);
        // As many as the marks: the mark turns one step, not a whole turn.
        draw(4);
        draw(4);
        draw(1);

        assert_eq!(
            drawn,
            [
                marked('|'),
                marked('/'),
                marked('/'),
                marked('-'),
                marked('\\'),
                marked('|')
            ]
        );
    }

    #[test]
    fn the_rebuild_has_no_mark() {
        let (_display, output) = display(ProgressKind::RebuildEmbeddings, in_place(60));

        let rebuild =
            progress_text::in_place(ProgressKind::RebuildEmbeddings, State::default(), None, 60);
        assert_eq!(output.text(), format!("\r{rebuild}"));
    }

    #[test]
    fn the_display_follows_the_width_of_the_terminal() {
        let mode = Mode::of(Some(widths(&[Some(60), Some(60), Some(100)])));
        let (display, output) = display(ProgressKind::Update, mode);

        display.show();

        let wide = progress_text::in_place(ProgressKind::Update, State::default(), Some('|'), 100);
        assert!(
            output.text().ends_with(&format!("\r{wide}")),
            "{:?}",
            output.text()
        );
    }

    #[test]
    fn a_line_is_added_at_the_start() {
        let (_display, output) = display(ProgressKind::Update, Mode::LineByLine);

        assert_eq!(output.text(), line(State::default()));
    }

    #[test]
    fn the_last_progress_reported_is_shown() {
        let (display, output) = display(ProgressKind::Update, Mode::LineByLine);

        display.report(Progress {
            done: 1,
            total: Some(10),
        });
        display.report(HALF);
        display.show();

        let expected = State {
            progress: Some(HALF),
            ..State::default()
        };
        assert!(output.text().ends_with(&format!("\n{}", line(expected))));
    }

    #[test]
    fn the_chunks_are_summed() {
        let (display, output) = display(ProgressKind::Update, Mode::LineByLine);

        display.add_embedded_chunks(3);
        display.add_embedded_chunks(4);
        display.show();

        let expected = State {
            chunks: 7,
            ..State::default()
        };
        assert!(output.text().ends_with(&format!("\n{}", line(expected))));
    }

    #[test]
    fn maintenance_is_shown_once_started() {
        let (display, output) = display(ProgressKind::Update, Mode::LineByLine);
        display.report(HALF);

        display.maintenance_started();
        display.show();

        let expected = State {
            progress: Some(HALF),
            maintaining: true,
            ..State::default()
        };
        assert!(output.text().ends_with(&format!("\n{}", line(expected))));
    }

    #[test]
    fn optimizing_shows_maintenance_from_the_start() {
        let (_display, output) = display(ProgressKind::Optimize, Mode::LineByLine);

        let expected = State {
            maintaining: true,
            ..State::default()
        };
        assert_eq!(output.text(), line(expected));
    }

    #[test]
    fn finishing_clears_the_display_drawn_in_place() {
        let (screen, output) = screen();
        let display = StderrProgress::start_with(
            Arc::clone(&screen),
            ProgressKind::Update,
            in_place(60),
            None,
            no_time(),
        );
        let shown = progress_text::in_place(ProgressKind::Update, State::default(), Some('|'), 60);

        display.finish();
        lock(&screen).write(b"Updated.\n").unwrap();

        assert_eq!(
            output.text(),
            format!(
                "\r{shown}\r{}\rUpdated.\n",
                " ".repeat(shown.chars().count())
            )
        );
    }

    #[test]
    fn dropping_without_finishing_clears_the_display_drawn_in_place() {
        let (screen, output) = screen();
        let display = StderrProgress::start_with(
            Arc::clone(&screen),
            ProgressKind::Update,
            in_place(60),
            None,
            no_time(),
        );
        let shown = marked('|');

        drop(display);
        lock(&screen).write(b"Failed.\n").unwrap();

        assert_eq!(
            output.text(),
            format!(
                "\r{shown}\r{}\rFailed.\n",
                " ".repeat(shown.chars().count())
            )
        );
    }

    #[test]
    fn finishing_again_leaves_the_next_display_alone() {
        let (screen, output) = screen();
        let first = StderrProgress::start_with(
            Arc::clone(&screen),
            ProgressKind::Update,
            in_place(60),
            None,
            no_time(),
        );
        first.finish();
        let _next = StderrProgress::start_with(
            Arc::clone(&screen),
            ProgressKind::RebuildEmbeddings,
            in_place(60),
            None,
            no_time(),
        );
        let shown = output.text();

        drop(first);

        assert_eq!(output.text(), shown);
    }

    #[test]
    fn finishing_lines_adds_nothing() {
        let (display, output) = display(ProgressKind::Update, Mode::LineByLine);
        let before = output.text();

        display.finish();

        assert_eq!(output.text(), before);
    }

    #[test]
    fn the_progress_is_shown_again_at_the_interval_until_finished() {
        let (screen, output) = screen();

        let display = StderrProgress::start_with(
            screen,
            ProgressKind::Update,
            Mode::LineByLine,
            Some(Duration::from_millis(1)),
            no_time(),
        );
        let line = line(State::default());
        let deadline = Instant::now() + Duration::from_secs(10);
        while output.text().matches(&line).count() < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        display.finish();
        let shown = output.text();

        assert!(shown.matches(&line).count() >= 3, "{shown:?}");
        // `finish` took the ticker and waited for its thread to end.
        assert!(lock(&display.ticker).is_none());
    }

    #[test]
    fn a_ticker_calls_until_stopped() {
        let (ticks, ticked) = mpsc::channel();
        let ticker = Ticker::start(Duration::from_millis(1), move || {
            let _ = ticks.send(());
        })
        .unwrap();

        ticked.recv_timeout(Duration::from_secs(10)).unwrap();
        ticked.recv_timeout(Duration::from_secs(10)).unwrap();
        ticker.stop();
        while ticked.try_recv().is_ok() {}

        // The thread has ended, and dropped the function with the sender.
        assert_eq!(ticked.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }
}
