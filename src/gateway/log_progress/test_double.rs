//! A test double for [`Console`], for the tests of `cli`.

// Parts of the double are for tests that the submodules using it have not
// written yet.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use crate::types::{Progress, ProgressSink};

use super::super::operations::GatewayProgressSink;
use super::{Console, ProgressDisplay, ProgressKind};

/// Keeps what is written to stderr and what every progress display it starts
/// receives, on one timeline, so that a test can check their order (for
/// example, that the progress ends before the summary is written).
#[derive(Default)]
pub(crate) struct TestConsole {
    timeline: Arc<Mutex<Vec<ConsoleEvent>>>,
    displays: Mutex<Vec<Arc<TestProgressDisplay>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConsoleEvent {
    Stderr(String),
    ProgressStarted(ProgressKind),
    /// What the display started `index`-th (from 0) received.
    Display {
        index: usize,
        event: DisplayEvent,
    },
}

impl TestConsole {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn events(&self) -> Vec<ConsoleEvent> {
        self.timeline.lock().unwrap().clone()
    }

    /// Everything written to stderr, joined.
    pub(crate) fn stderr(&self) -> String {
        self.timeline
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                ConsoleEvent::Stderr(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The progress displays started so far, in order.
    pub(crate) fn displays(&self) -> Vec<Arc<TestProgressDisplay>> {
        self.displays.lock().unwrap().clone()
    }
}

impl Console for TestConsole {
    fn write_stderr(&self, text: &str) {
        self.timeline
            .lock()
            .unwrap()
            .push(ConsoleEvent::Stderr(text.to_owned()));
    }

    fn start_progress(&self, kind: ProgressKind) -> Arc<dyn ProgressDisplay> {
        self.timeline
            .lock()
            .unwrap()
            .push(ConsoleEvent::ProgressStarted(kind));
        let mut displays = self.displays.lock().unwrap();
        let display = Arc::new(TestProgressDisplay {
            index: displays.len(),
            events: Mutex::default(),
            timeline: Arc::clone(&self.timeline),
        });
        displays.push(Arc::clone(&display));
        display
    }
}

/// A progress display that records what it receives, by itself and on the
/// timeline of the console that started it.
pub(crate) struct TestProgressDisplay {
    index: usize,
    events: Mutex<Vec<DisplayEvent>>,
    timeline: Arc<Mutex<Vec<ConsoleEvent>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayEvent {
    Report(Progress),
    EmbeddedChunks(u64),
    MaintenanceStarted,
    Finished,
}

impl TestProgressDisplay {
    pub(crate) fn events(&self) -> Vec<DisplayEvent> {
        self.events.lock().unwrap().clone()
    }

    fn record(&self, event: DisplayEvent) {
        self.events.lock().unwrap().push(event);
        self.timeline.lock().unwrap().push(ConsoleEvent::Display {
            index: self.index,
            event,
        });
    }
}

impl ProgressSink for TestProgressDisplay {
    fn report(&self, progress: Progress) {
        self.record(DisplayEvent::Report(progress));
    }

    fn add_embedded_chunks(&self, count: u64) {
        self.record(DisplayEvent::EmbeddedChunks(count));
    }
}

impl GatewayProgressSink for TestProgressDisplay {
    fn maintenance_started(&self) {
        self.record(DisplayEvent::MaintenanceStarted);
    }
}

impl ProgressDisplay for TestProgressDisplay {
    fn finish(&self) {
        self.record(DisplayEvent::Finished);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_and_progress_are_kept_on_one_timeline() {
        let console = TestConsole::new();

        console.write_stderr("first\n");
        let display = console.start_progress(ProgressKind::Update);
        display.finish();
        console.write_stderr("second\n");

        assert_eq!(
            console.events(),
            vec![
                ConsoleEvent::Stderr("first\n".to_owned()),
                ConsoleEvent::ProgressStarted(ProgressKind::Update),
                ConsoleEvent::Display {
                    index: 0,
                    event: DisplayEvent::Finished
                },
                ConsoleEvent::Stderr("second\n".to_owned()),
            ]
        );
        assert_eq!(console.stderr(), "first\nsecond\n");
    }

    #[test]
    fn a_started_display_records_what_it_receives() {
        let console = TestConsole::new();

        let display = console.start_progress(ProgressKind::Optimize);
        display.maintenance_started();
        display.finish();

        let displays = console.displays();
        assert_eq!(displays.len(), 1);
        assert_eq!(
            displays[0].events(),
            vec![DisplayEvent::MaintenanceStarted, DisplayEvent::Finished]
        );
    }

    #[test]
    fn a_display_can_be_handed_over_as_the_sinks_it_extends() {
        let console = TestConsole::new();
        let display = console.start_progress(ProgressKind::Update);

        // `cli` puts the display in an operation's input as a
        // `GatewayProgressSink`, and the operation passes that on to the
        // components as a `ProgressSink`.
        let for_operation: Arc<dyn GatewayProgressSink> = display;
        let for_components: &dyn ProgressSink = for_operation.as_ref();
        for_components.add_embedded_chunks(2);

        assert_eq!(
            console.displays()[0].events(),
            vec![DisplayEvent::EmbeddedChunks(2)]
        );
    }
}
