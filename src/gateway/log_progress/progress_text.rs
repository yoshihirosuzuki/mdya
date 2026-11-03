//! The text of the progress: the one-line progress display drawn again in
//! place on a terminal, and the lines added one by one elsewhere.

use std::time::Duration;

use super::ProgressKind;
use super::progress::State;
use super::screen::width as columns;

/// A bar with fewer cells is left out: it would show too little.
const MIN_BAR_CELLS: usize = 10;

/// The one-line progress display, fitted to a terminal `width` columns wide:
/// the mark, if any, the name of the operation, a bar of the share done that
/// fills the rest of the line, and the numbers.
pub(super) fn in_place(
    kind: ProgressKind,
    state: State,
    mark: Option<char>,
    width: usize,
) -> String {
    let parts = Parts::of(kind, state);
    let head = match (mark, &parts.numbers) {
        (Some(mark), Some(_)) => format!("{mark} {}", parts.name),
        _ => parts.name.to_owned(),
    };
    // The last column is left free, so that the display never writes it,
    // and how a terminal moves the cursor after it never matters.
    let room = width.saturating_sub(1);
    let (Some(numbers), Some((done, total))) = (&parts.numbers, parts.share) else {
        return cut(parts.plain(&head), room);
    };
    let plain = parts.plain(&head);
    // With the bar, the line has one more space than the plain text, and the
    // two brackets.
    let cells = room.saturating_sub(columns(&plain) + 3);
    if cells < MIN_BAR_CELLS {
        return cut(plain, room);
    }
    let bar = share_bar(done, total, cells);
    format!("{head} {bar} {numbers}")
}

/// A line of progress, added below the earlier ones, to be read later and
/// without the lines around it: the name of the operation, the numbers, for
/// an update the chunks embedded with what they are, and the time since the
/// start. Without a bar, which has no width to fill.
pub(super) fn line(kind: ProgressKind, state: State, elapsed: Duration) -> String {
    let parts = Parts::of(kind, state);
    let mut line = parts.plain(parts.name);
    if kind == ProgressKind::Update && parts.numbers.is_some() {
        line.push_str(&format!(", {} chunks embedded", state.chunks));
    }
    format!("{line}, {}", time(elapsed))
}

/// `20s`, `1m 20s`, or `1h 2m 3s`, in whole seconds.
fn time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
}

/// What the progress shows.
struct Parts {
    name: &'static str,
    numbers: Option<String>,
    /// What was done out of the total, when the total is known.
    share: Option<(u64, u64)>,
}

impl Parts {
    fn of(kind: ProgressKind, state: State) -> Self {
        if state.maintaining || kind == ProgressKind::Optimize {
            return Self {
                name: "Optimizing",
                numbers: None,
                share: None,
            };
        }
        let done = state.progress.map_or(0, |progress| progress.done);
        let total = state.progress.and_then(|progress| progress.total);
        let numbers = match total {
            Some(total) => format!("{done}/{total}"),
            None => done.to_string(),
        };
        let name = match kind {
            ProgressKind::Update => "Updating",
            ProgressKind::RebuildEmbeddings => "Rebuilding",
            ProgressKind::Optimize => "Optimizing",
        };
        Self {
            name,
            numbers: Some(numbers),
            share: total.map(|total| (done, total)),
        }
    }

    /// `head`, then the numbers, without a bar.
    fn plain(&self, head: &str) -> String {
        match &self.numbers {
            Some(numbers) => format!("{head} {numbers}"),
            None => head.to_owned(),
        }
    }
}

/// `[#####-----]`, with `cells` cells, those for the share done filled.
fn share_bar(done: u64, total: u64, cells: usize) -> String {
    let filled = match total {
        0 => cells,
        total => {
            let filled = u128::from(done.min(total)) * cells as u128 / u128::from(total);
            usize::try_from(filled).unwrap_or(cells)
        }
    };
    format!("[{}{}]", "#".repeat(filled), "-".repeat(cells - filled))
}

/// `text`, cut to `room` columns.
fn cut(text: String, room: usize) -> String {
    match columns(&text) <= room {
        true => text,
        false => text.chars().take(room).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Progress;

    fn reported(done: u64, total: Option<u64>, chunks: u64) -> State {
        State {
            progress: Some(Progress { done, total }),
            chunks,
            maintaining: false,
        }
    }

    const HALF_WAY: (u64, Option<u64>, u64) = (120, Some(450), 3200);

    fn update(width: usize) -> String {
        let (done, total, chunks) = HALF_WAY;
        in_place(
            ProgressKind::Update,
            reported(done, total, chunks),
            Some('/'),
            width,
        )
    }

    #[test]
    fn the_bar_fills_the_line_but_its_last_column() {
        let text = update(60);

        // 18 columns for the mark, the name, and the numbers, 3 for the
        // spaces and the brackets around the bar, and the bar's 38 cells, a
        // fraction 120/450 of them filled.
        assert_eq!(
            text,
            format!("/ Updating [{}{}] 120/450", "#".repeat(10), "-".repeat(28))
        );
        assert_eq!(text.chars().count(), 59);
    }

    #[test]
    fn a_wider_terminal_gets_a_longer_bar() {
        assert_eq!(update(100).chars().count(), 99);
    }

    #[test]
    fn without_room_for_ten_cells_the_bar_is_left_out() {
        assert_eq!(update(32).chars().count(), 31);
        assert_eq!(update(31), "/ Updating 120/450");
    }

    #[test]
    fn a_line_longer_than_the_terminal_is_cut() {
        assert_eq!(update(10), "/ Updatin");
    }

    #[test]
    fn without_the_total_the_bar_is_left_out() {
        let text = in_place(
            ProgressKind::Update,
            reported(120, None, 3200),
            Some('/'),
            80,
        );

        assert_eq!(text, "/ Updating 120");
    }

    #[test]
    fn without_a_mark_the_line_starts_with_the_name() {
        let (done, total, chunks) = HALF_WAY;

        let text = in_place(
            ProgressKind::Update,
            reported(done, total, chunks),
            None,
            60,
        );

        assert!(text.starts_with("Updating ["), "{text}");
    }

    #[test]
    fn the_rebuild_has_the_chunks_done_and_their_total() {
        let text = in_place(
            ProgressKind::RebuildEmbeddings,
            reported(1200, Some(5000), 0),
            None,
            60,
        );

        // 20 columns for the name and the numbers, 3 around the bar, and 36
        // cells.
        assert_eq!(
            text,
            format!("Rebuilding [{}{}] 1200/5000", "#".repeat(8), "-".repeat(28))
        );
    }

    #[test]
    fn the_bar_is_empty_at_the_start_and_full_at_the_end() {
        let bar = |done, total| {
            in_place(
                ProgressKind::RebuildEmbeddings,
                reported(done, Some(total), 0),
                None,
                60,
            )
        };

        assert!(bar(0, 450).contains(&format!("[{}]", "-".repeat(40))));
        assert!(bar(450, 450).contains(&format!("[{}]", "#".repeat(38))));
        assert!(bar(0, 0).contains(&format!("[{}]", "#".repeat(42))));
        assert!(bar(500, 450).contains(&format!("[{}]", "#".repeat(38))));
    }

    #[test]
    fn a_large_total_does_not_overflow() {
        let text = in_place(
            ProgressKind::RebuildEmbeddings,
            reported(u64::MAX - 1, Some(u64::MAX), 0),
            None,
            80,
        );

        assert!(
            text.starts_with(&format!("Rebuilding [{}-]", "#".repeat(23))),
            "{text}"
        );
        assert!(text.ends_with("] 18446744073709551614/18446744073709551615"));
    }

    const SEC: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn a_line_of_an_update_has_the_documents_the_chunks_embedded_and_the_time() {
        let (done, total, chunks) = HALF_WAY;

        let text = line(ProgressKind::Update, reported(done, total, chunks), SEC(80));

        assert_eq!(text, "Updating 120/450, 3200 chunks embedded, 1m 20s");
    }

    #[test]
    fn without_the_total_a_line_has_the_documents_done() {
        let text = line(ProgressKind::Update, reported(120, None, 3200), SEC(80));

        assert_eq!(text, "Updating 120, 3200 chunks embedded, 1m 20s");
    }

    #[test]
    fn before_any_progress_a_line_has_zeros() {
        assert_eq!(
            line(ProgressKind::Update, State::default(), SEC(0)),
            "Updating 0, 0 chunks embedded, 0s"
        );
    }

    #[test]
    fn a_line_of_a_rebuild_has_the_chunks_and_the_time() {
        let text = line(
            ProgressKind::RebuildEmbeddings,
            reported(1200, Some(5000), 0),
            SEC(80),
        );

        assert_eq!(text, "Rebuilding 1200/5000, 1m 20s");
    }

    #[test]
    fn maintenance_shows_its_name_and_on_a_line_the_time() {
        let state = State {
            maintaining: true,
            ..reported(120, Some(450), 3200)
        };

        assert_eq!(
            in_place(ProgressKind::Update, state, None, 80),
            "Optimizing"
        );
        assert_eq!(
            line(ProgressKind::RebuildEmbeddings, state, SEC(190)),
            "Optimizing, 3m 10s"
        );
    }

    #[test]
    fn optimizing_shows_maintenance_from_the_start() {
        assert_eq!(
            line(ProgressKind::Optimize, State::default(), SEC(0)),
            "Optimizing, 0s"
        );
    }

    #[test]
    fn the_time_drops_the_larger_units_that_are_zero() {
        let time = |elapsed| line(ProgressKind::Optimize, State::default(), elapsed);

        assert_eq!(time(Duration::from_millis(59_900)), "Optimizing, 59s");
        assert_eq!(time(SEC(60)), "Optimizing, 1m 0s");
        assert_eq!(time(SEC(3600)), "Optimizing, 1h 0m 0s");
        assert_eq!(time(SEC(3723)), "Optimizing, 1h 2m 3s");
    }
}
