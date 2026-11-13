//! What the integration tests share: running mdya with the real components,
//! either in the test's own process or as the executable, with directories
//! that belong to each test.
//!
//! In the test's own process, only the functions that set the log receiver
//! and the process's environment variables are replaced. Both act on the whole
//! process, which the tests share: the log receiver can be set only once per
//! process, and setting environment variables while other test threads run is
//! unsound.
//!
//! Most tests are ignored by default and run by hand with `just integration`:
//! each test that runs the real default embedding model, and the tests that do
//! not but are left out to keep the default tests few. The first run of a test
//! that runs the real model needs the network: it fetches the model, about
//! 150 MB, and keeps it under `target/`.
//!
//! A test that does not run the real model, except the one described below,
//! must not prepare it. Such a test gets a model cache directory of its own
//! that is never created, and each run of mdya through a `Workspace` function
//! whose name begins with `run` checks that the directory still does not
//! exist. Preparing the default model, which runs on this PC, creates the
//! directory before fetching anything, so a preparation that cannot fetch the
//! model leaves it behind too. A preparation that fails before that leaves
//! nothing: one that cannot decide the directory, which cannot happen here as
//! the tests give an absolute path, and one that fails while it sets up the
//! connection to fetch with (`Hub::connect` in `src/embedding/on_device.rs`).
//!
//! One test in `shared_parts.rs` checks that check: it prepares the model on
//! purpose, through `Workspace::executable_that_cannot_fetch`, which does not
//! check, and then checks that the directory is there.

// `allow-unwrap-in-tests` in clippy.toml covers test functions and
// `#[cfg(test)]` code, which these helpers of the tests are not.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};
use std::sync::{Arc, Mutex, Once};

use mdya::collection::CollectionInfo;
use mdya::embedding::EmbeddingSettings;
use mdya::format::Format;
use mdya::gateway::{Factories, Io, run_with};
use mdya::index::Index;
use mdya::ingest::Ingest;
use mdya::types::{BoxError, CollectionName, CollectionType, DirLookup, TypeAttributes};
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
    model_cache: ModelCache,
}

/// Where a workspace keeps the embedding models.
enum ModelCache {
    /// The directory that the tests running the real model share, so that
    /// each model is fetched once.
    Shared,
    /// A directory of the test's own, which must never be created, except in
    /// the one test that checks that check (see this module's documentation).
    Unused(PathBuf),
}

impl Workspace {
    /// A workspace for a test that must not prepare the embedding model, and
    /// for the one test that checks that check (see this module's
    /// documentation).
    pub fn new() -> Self {
        Self::with_model_cache(|root| ModelCache::Unused(root.join("model-cache")))
    }

    /// A workspace for a test that runs the real default embedding model.
    pub fn with_real_model() -> Self {
        Self::with_model_cache(|_| ModelCache::Shared)
    }

    fn with_model_cache(model_cache: impl FnOnce(&Path) -> ModelCache) -> Self {
        prepare_full_text_search();
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let model_cache = model_cache(root.path());
        Self {
            root,
            data_dir,
            model_cache,
        }
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
        self.run_with_io(args, stdin, false)
    }

    /// Runs mdya with the given arguments, as if its standard input were a
    /// terminal where the user types `answer` to a confirmation.
    pub fn run_answering(&self, args: &[&str], answer: &str) -> Outcome {
        self.run_with_io(args, answer.as_bytes(), true)
    }

    fn run_with_io(&self, args: &[&str], stdin: &[u8], stdin_is_terminal: bool) -> Outcome {
        let stdout = Captured::default();
        let stderr = Captured::default();
        let io = Io {
            stdin: Box::new(Cursor::new(stdin.to_vec())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            stdin_is_terminal,
            stdout_is_terminal: false,
            stderr_is_terminal: false,
        };
        let code = run_with(self.arguments(args), Vec::new(), io, real_factories());
        self.check_model_not_prepared(args);
        Outcome {
            code,
            stdout: stdout.text(),
            stderr: stderr.text(),
        }
    }

    /// Runs the mdya executable with the given arguments, on this test's
    /// directories, and waits for it to end.
    pub fn run_executable(&self, args: &[&str]) -> Output {
        let output = self.executable().args(args).output().unwrap();
        self.check_model_not_prepared(args);
        output
    }

    /// Runs the mdya executable as `run_executable` does, with `dir` as its
    /// working directory.
    pub fn run_executable_in(&self, dir: &Path, args: &[&str]) -> Output {
        let output = self
            .executable()
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        self.check_model_not_prepared(args);
        output
    }

    /// Makes a collection of `collection_type`, a type that this mdya does not
    /// know, with `attributes` (a JSON object) as its type-specific
    /// attributes. It goes through the component that keeps the collections,
    /// as a newer mdya that knows the type would: the command line makes only
    /// collections of the types it knows. The embedding settings it gives are
    /// never used, as making a collection embeds nothing.
    pub fn add_collection_of_unknown_type(
        &self,
        name: &str,
        collection_type: &str,
        attributes: serde_json::Value,
    ) {
        let model_cache_dir = self.model_cache_dir();
        let embedding = mdya::embedding::create(EmbeddingSettings {
            model: "cl-nagoya/ruri-v3-30m".to_owned(),
            model_cache_dir: Arc::new(move || Ok::<_, BoxError>(model_cache_dir.clone())),
            hf_token: None,
            ollama_endpoint: "http://127.0.0.1:11434".to_owned(),
        });
        let index = mdya::index::create(self.data_dir.clone(), embedding);
        let collections = mdya::collection::create(self.data_dir.clone(), index);
        let collection = CollectionInfo {
            name: CollectionName(name.to_owned()),
            collection_type: CollectionType(collection_type.to_owned()),
            attributes: TypeAttributes(attributes.as_object().unwrap().clone()),
            description: None,
        };
        block_on(collections.create_collection(collection)).unwrap();
    }

    /// The mdya executable, given this test's directories. Running it this
    /// way, unlike the functions whose names begin with `run`, does not check
    /// that the embedding model was not prepared.
    pub fn executable(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mdya"));
        command.args(self.directory_arguments());
        command
    }

    /// The mdya executable, given this test's directories, with an HTTPS
    /// proxy that refuses connections: fetching the embedding model fails at
    /// once, without the network.
    pub fn executable_that_cannot_fetch(&self) -> Command {
        let mut command = self.executable();
        for name in ["NO_PROXY", "no_proxy", "ALL_PROXY", "all_proxy"] {
            command.env_remove(name);
        }
        command
            .env("HTTPS_PROXY", REFUSING_PROXY)
            .env("https_proxy", REFUSING_PROXY);
        command
    }

    /// Whether mdya has created the model cache directory, as preparing the
    /// embedding model does (see this module's documentation), in a workspace
    /// from `Workspace::new`. Always false in a workspace from
    /// `Workspace::with_real_model`.
    pub fn model_was_prepared(&self) -> bool {
        matches!(&self.model_cache, ModelCache::Unused(dir) if dir.try_exists().unwrap())
    }

    /// Panics if running mdya with `args` prepared the embedding model, in a
    /// workspace from `Workspace::new`.
    fn check_model_not_prepared(&self, args: &[&str]) {
        assert!(
            !self.model_was_prepared(),
            "`mdya {}` prepared the embedding model: it created {}",
            args.join(" "),
            self.model_cache_dir().display()
        );
    }

    fn model_cache_dir(&self) -> PathBuf {
        match &self.model_cache {
            ModelCache::Shared => test_data_dir().join("models"),
            ModelCache::Unused(dir) => dir.clone(),
        }
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
            self.model_cache_dir().into_os_string(),
        ]
    }
}

/// A proxy at a port that nothing listens on.
const REFUSING_PROXY: &str = "http://127.0.0.1:1";

/// The directory that `.cargo/config.toml` keeps the tests' data in.
fn test_data_dir() -> &'static Path {
    Path::new(env!("LANCE_LANGUAGE_MODEL_HOME"))
        .parent()
        .unwrap()
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
        block_on(mdya::index::place_fts_dictionary_config(test_data_dir())).unwrap();
    });
}

/// Runs `future` to its end on a runtime of its own.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
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
