//! A test double for the zip and tar readers, for the driver's unit tests. It
//! returns the entries a test gives it, does what a reader does with the size
//! of the list, and records what it was asked.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};

use super::kind::ArchiveKind;
use super::readers::Readers;
use super::transfer::{Contents, Entry, ListError, ListSize, Listing, Position, ReadResult};
use crate::types::BoxError;

type Hook = Box<dyn Fn() + Send + Sync>;
type ReadHook = Box<dyn Fn(Position, &mut File) -> Option<ReadResult> + Send + Sync>;

/// An entry of the archive the double pretends to read. Its content is its
/// name, unless a test says otherwise with [`TestReaders::on_read`].
#[derive(Clone)]
pub(super) struct TestItem {
    pub(super) name: Vec<u8>,
    pub(super) regular: bool,
}

pub(super) fn document(name: &str) -> TestItem {
    raw_document(name.as_bytes())
}

pub(super) fn raw_document(name: &[u8]) -> TestItem {
    TestItem {
        name: name.to_vec(),
        regular: true,
    }
}

pub(super) fn not_regular(name: &str) -> TestItem {
    TestItem {
        name: name.as_bytes().to_vec(),
        regular: false,
    }
}

#[derive(Default)]
struct ReaderLog {
    kinds_listed: Vec<ArchiveKind>,
    kinds_read: Vec<ArchiveKind>,
    max_contents: Vec<u64>,
    positions: Vec<Position>,
}

/// What the double was set up with and what it was asked, shared with the
/// listings and the contents it hands out.
struct Shared {
    items: Mutex<Vec<TestItem>>,
    on_list_step: Hook,
    on_list_end: Hook,
    on_read: ReadHook,
    list_failure: Option<String>,
    start_failure: Option<String>,
    log: Mutex<ReaderLog>,
}

pub(super) struct TestReaders {
    shared: Arc<Shared>,
    /// Whether each coming duplication of the file fails. Past its end, none do.
    clone_failures: Mutex<VecDeque<bool>>,
}

impl TestReaders {
    pub(super) fn new(items: Vec<TestItem>) -> Self {
        Self {
            shared: Arc::new(Shared {
                items: Mutex::new(items),
                on_list_step: Box::new(|| {}),
                on_list_end: Box::new(|| {}),
                on_read: Box::new(|_, _| None),
                list_failure: None,
                start_failure: None,
                log: Mutex::new(ReaderLog::default()),
            }),
            clone_failures: Mutex::new(VecDeque::new()),
        }
    }

    fn shared_mut(&mut self) -> &mut Shared {
        Arc::get_mut(&mut self.shared).unwrap()
    }

    /// Runs `hook` each time the first pass is asked for an entry.
    pub(super) fn on_list_step(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.shared_mut().on_list_step = Box::new(hook);
        self
    }

    /// Runs `hook` when the first pass reaches the end of the list.
    pub(super) fn on_list_end(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.shared_mut().on_list_end = Box::new(hook);
        self
    }

    /// Answers the second pass from `hook` whenever it returns a result.
    pub(super) fn on_read(
        mut self,
        hook: impl Fn(Position, &mut File) -> Option<ReadResult> + Send + Sync + 'static,
    ) -> Self {
        self.shared_mut().on_read = Box::new(hook);
        self
    }

    /// Makes the first pass end with the source being unreadable.
    pub(super) fn failing_list(mut self, cause: &str) -> Self {
        self.shared_mut().list_failure = Some(cause.to_owned());
        self
    }

    /// Makes starting the second pass fail.
    pub(super) fn failing_start(mut self, cause: &str) -> Self {
        self.shared_mut().start_failure = Some(cause.to_owned());
        self
    }

    /// Says whether each coming duplication of the file fails.
    pub(super) fn with_clone_failures(self, failures: &[bool]) -> Self {
        *self.clone_failures.lock().unwrap() = failures.iter().copied().collect();
        self
    }

    pub(super) fn set_items(&self, items: Vec<TestItem>) {
        *self.shared.items.lock().unwrap() = items;
    }

    pub(super) fn kinds_listed(&self) -> Vec<ArchiveKind> {
        self.shared.log.lock().unwrap().kinds_listed.clone()
    }

    pub(super) fn kinds_read(&self) -> Vec<ArchiveKind> {
        self.shared.log.lock().unwrap().kinds_read.clone()
    }

    pub(super) fn max_contents(&self) -> Vec<u64> {
        self.shared.log.lock().unwrap().max_contents.clone()
    }

    pub(super) fn positions_read(&self) -> Vec<Position> {
        self.shared.log.lock().unwrap().positions.clone()
    }
}

impl Readers for TestReaders {
    fn clone_file(&self, file: &File) -> io::Result<File> {
        if self.clone_failures.lock().unwrap().pop_front() == Some(true) {
            return Err(io::Error::other("duplicating the file failed"));
        }
        file.try_clone()
    }

    fn listing(&self, kind: ArchiveKind, _file: File) -> Box<dyn Listing> {
        self.shared.log.lock().unwrap().kinds_listed.push(kind);
        let items = self.shared.items.lock().unwrap().clone();
        Box::new(TestListing {
            shared: Arc::clone(&self.shared),
            items: items.into_iter().enumerate().collect(),
        })
    }

    fn contents(
        &self,
        kind: ArchiveKind,
        file: File,
        max_content: u64,
    ) -> Result<Box<dyn Contents>, BoxError> {
        let mut log = self.shared.log.lock().unwrap();
        log.kinds_read.push(kind);
        log.max_contents.push(max_content);
        drop(log);
        if let Some(cause) = &self.shared.start_failure {
            return Err(cause.as_str().into());
        }
        Ok(Box::new(TestContents {
            shared: Arc::clone(&self.shared),
            file,
        }))
    }
}

struct TestListing {
    shared: Arc<Shared>,
    items: VecDeque<(usize, TestItem)>,
}

impl Listing for TestListing {
    fn next(&mut self, size: &mut ListSize) -> Result<Option<Entry>, ListError> {
        (self.shared.on_list_step)();
        let Some((index, item)) = self.items.pop_front() else {
            (self.shared.on_list_end)();
            return match &self.shared.list_failure {
                Some(cause) => Err(ListError::SourceUnreadable(cause.as_str().into())),
                None => Ok(None),
            };
        };
        size.add(item.name.len() as u64)
            .map_err(|_| ListError::ListSizeExceeded)?;
        Ok(Some(Entry {
            name: item.name,
            regular: item.regular,
            position: Position(index as u64),
        }))
    }
}

struct TestContents {
    shared: Arc<Shared>,
    file: File,
}

impl Contents for TestContents {
    fn read(&mut self, position: Position) -> ReadResult {
        self.shared.log.lock().unwrap().positions.push(position);
        if let Some(result) = (self.shared.on_read)(position, &mut self.file) {
            return result;
        }
        let item = self.shared.items.lock().unwrap()[position.0 as usize].clone();
        ReadResult::Entry {
            content: Ok(item.name.clone()),
            name: item.name,
            regular: item.regular,
        }
    }
}
