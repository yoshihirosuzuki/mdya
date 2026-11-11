//! What the integration tests share: running mdya with the real components,
//! in the test's own process or as the executable, on directories of the
//! test's own.
//!
//! In the test's own process, only the functions that set the log receiver
//! and the process's environment variables are replaced. Both act on the whole
//! process, which the tests share: the log receiver can be set only once per
//! process, and setting environment variables while other test threads run is
//! unsound.
//!
//! The tests run the real default embedding model, so that `cargo test` needs
//! the network the first time: it fetches the model, about 150 MB, and keeps
//! it under `target/`. The tests in `src/embedding/on_device/real_model_tests.rs`
//! are ignored by default because they fetch up to 1.2 GB for each model.
//!
//! Each file under `tests/` is a crate of its own that uses a part of this
//! module, so what one of them leaves unused is not dead code.
#![allow(dead_code)]
// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which these helpers of the tests are not.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex, Once};

use mdya::format::Format;
use mdya::gateway::{Factories, Io, run_with};
use mdya::index::Index;
use mdya::ingest::Ingest;
use mdya::types::{CollectionType, DirLookup};
use tempfile::TempDir;

/// What one run of mdya ended with.
#[derive(Debug)]
pub struct Outcome {
    pub code: ExitCode,
    pub stdout: String,
    pub stderr: String,
}

/// The directories of one test, and running mdya on them.
pub struct Workspace {
    root: TempDir,
    data_dir: PathBuf,
}

impl Workspace {
    pub fn new() -> Self {
        prepare_full_text_search();
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        Self { root, data_dir }
    }

    /// A directory inside this test's own directory, created if needed.
    pub fn dir(&self, name: &str) -> PathBuf {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs mdya with the given arguments, on this test's directories.
    pub fn run(&self, args: &[&str]) -> Outcome {
        self.run_with_stdin(args, b"")
    }

    /// Runs mdya with the given arguments, and `stdin` as its standard input.
    pub fn run_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Outcome {
        let stdout = Captured::default();
        let stderr = Captured::default();
        let io = Io {
            stdin: Box::new(Cursor::new(stdin.to_vec())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            stdin_is_terminal: false,
            stdout_is_terminal: false,
            stderr_is_terminal: false,
        };
        let code = run_with(self.arguments(args), Vec::new(), io, real_factories());
        Outcome {
            code,
            stdout: stdout.text(),
            stderr: stderr.text(),
        }
    }

    /// The mdya executable, given this test's directories.
    pub fn executable(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mdya"));
        command.args(self.directory_arguments());
        command
    }

    fn arguments(&self, args: &[&str]) -> Vec<OsString> {
        let mut all = vec![OsString::from("mdya")];
        all.extend(self.directory_arguments());
        all.extend(args.iter().map(OsString::from));
        all
    }

    fn directory_arguments(&self) -> [OsString; 4] {
        [
            OsString::from("--data-dir"),
            self.data_dir.clone().into_os_string(),
            OsString::from("--model-cache-dir"),
            model_cache_dir().into_os_string(),
        ]
    }
}

/// The directory that `.cargo/config.toml` keeps the tests' data in.
fn test_data_dir() -> &'static Path {
    Path::new(env!("LANCE_LANGUAGE_MODEL_HOME"))
        .parent()
        .unwrap()
}

/// Where the tests keep the embedding models, shared so that each model is
/// fetched once.
fn model_cache_dir() -> PathBuf {
    test_data_dir().join("models")
}

/// The real components, with the two process-wide functions replaced.
fn real_factories() -> Factories {
    Factories {
        set_log_receiver: Box::new(|_| {}),
        set_process_environment: Box::new(|_| {}),
        config: Box::new(mdya::config::create),
        start_watchdog: Box::new(|settings| Box::new(mdya::watchdog::start(settings))),
        embedding: Box::new(mdya::embedding::create),
        index: Box::new(mdya::index::create),
        collections: Box::new(mdya::collection::create),
        query: Box::new(mdya::query::create),
        format: Box::new(mdya::format::create),
        ingests: Box::new(real_ingests),
        dirs: DirLookup {
            home_dir: Arc::new(std::env::home_dir),
            current_dir: Arc::new(std::env::current_dir),
        },
    }
}

fn real_ingests(
    format: Arc<dyn Format>,
    index: Arc<dyn Index>,
) -> BTreeMap<CollectionType, Arc<dyn Ingest>> {
    BTreeMap::from([
        (
            CollectionType("directory".to_owned()),
            mdya::file_ingest::create(Arc::clone(&format), Arc::clone(&index)),
        ),
        (
            CollectionType("archive".to_owned()),
            mdya::archive_ingest::create(Arc::clone(&format), Arc::clone(&index)),
        ),
        (
            CollectionType("raw".to_owned()),
            mdya::raw_ingest::create(format, index),
        ),
    ])
}

/// Places the full-text search dictionary configuration where cargo points
/// `LANCE_LANGUAGE_MODEL_HOME` (`.cargo/config.toml`), once per process.
fn prepare_full_text_search() {
    static PREPARED: Once = Once::new();
    PREPARED.call_once(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(mdya::index::place_fts_dictionary_config(test_data_dir()))
            .unwrap();
    });
}

/// A writer whose bytes can be read after the writer was given away.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
