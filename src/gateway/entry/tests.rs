//! `run_with`, through the real submodules of `gateway`, with test doubles
//! of the other components.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::config::{Config, ConfigError};
use crate::embedding::EmbeddingSettings;
use crate::embedding::test_double::TestEmbedding;
use crate::format::test_double::TestFormat;
use crate::index::test_double::TestIndex;
use crate::ingest::Ingest;
use crate::types::test_double::test_dir_lookup;
use crate::types::{CollectionType, DirLookup};

use super::super::cli::LogFormat;
use super::super::operations::version;
use super::super::test_double::captured::CapturedOutput;
use super::super::test_double::collections::TestCollections;
use super::super::test_double::config::TestConfig;
use super::super::test_double::ingest::TestIngest;
use super::super::test_double::query::TestQuery;
use super::super::test_double::watchdog::{TestWatchdog, WatchdogState};
use super::super::{Factories, Io, LogSettings, run_with};
use super::{Environment, hf_token, progress_terminal, size_limit};

/// The home directory that the tests give, an absolute path on the operating
/// system that runs them.
#[cfg(windows)]
const HOME: &str = r"C:\Users\user";
#[cfg(not(windows))]
const HOME: &str = "/home/user";

/// The working directory that the tests give, an absolute path on the
/// operating system that runs them.
#[cfg(windows)]
const WORK: &str = r"C:\work";
#[cfg(not(windows))]
const WORK: &str = "/work";

/// What a run is given, besides the arguments.
struct Setup {
    config: TestConfig,
    collections: TestCollections,
    dirs: DirLookup,
    env: Vec<(&'static str, &'static str)>,
    stdin: Vec<u8>,
    stderr_is_terminal: bool,
    /// Whether creating `format`, the first thing done, panics.
    format_panics: bool,
}

impl Setup {
    fn new() -> Self {
        Self {
            config: TestConfig::new(),
            collections: TestCollections::new(),
            dirs: test_dir_lookup(Some(HOME), Ok(WORK)),
            env: Vec::new(),
            stdin: Vec::new(),
            stderr_is_terminal: false,
            format_panics: false,
        }
    }
}

/// What the factories received.
#[derive(Default)]
struct Records {
    /// Whether to color, and the log format, of each log receiver set.
    log_settings: Vec<(bool, LogFormat)>,
    environment: Vec<Vec<(OsString, OsString)>>,
    config_dirs: Vec<PathBuf>,
    embeddings: Vec<EmbeddingRecord>,
}

/// What an `embedding` was created with.
#[derive(Debug, PartialEq)]
struct EmbeddingRecord {
    model: String,
    hf_token: Option<String>,
    /// What its function for the model cache directory gave.
    model_cache_dir: Result<PathBuf, String>,
}

struct Outcome {
    code: ExitCode,
    stdout: String,
    stderr: String,
    records: Records,
    watchdog: WatchdogState,
}

fn run(args: &[&str], setup: Setup) -> Outcome {
    let records = Arc::new(Mutex::new(Records::default()));
    let watchdog = TestWatchdog::new();
    let config = Arc::new(setup.config);
    let collections = Arc::new(setup.collections);
    let format_panics = setup.format_panics;
    let factories = Factories {
        set_log_receiver: {
            let records = Arc::clone(&records);
            Box::new(move |settings: LogSettings| {
                let recorded = (settings.color, settings.format);
                records.lock().unwrap().log_settings.push(recorded);
            })
        },
        set_process_environment: {
            let records = Arc::clone(&records);
            Box::new(move |variables| records.lock().unwrap().environment.push(variables))
        },
        config: {
            let records = Arc::clone(&records);
            Box::new(move |data_dir| {
                records.lock().unwrap().config_dirs.push(data_dir);
                Arc::clone(&config) as Arc<dyn Config>
            })
        },
        start_watchdog: watchdog.start_fn(),
        embedding: {
            let records = Arc::clone(&records);
            Box::new(move |settings: EmbeddingSettings| {
                records.lock().unwrap().embeddings.push(EmbeddingRecord {
                    model: settings.model.clone(),
                    hf_token: settings.hf_token.clone(),
                    model_cache_dir: (settings.model_cache_dir)()
                        .map_err(|error| error.to_string()),
                });
                Arc::new(TestEmbedding::new("cl-nagoya/ruri-v3-30m", 256))
            })
        },
        index: Box::new(|_, _| Arc::new(TestIndex::new())),
        collections: Box::new(move |_, _| Arc::clone(&collections) as _),
        query: Box::new(|_, _| Arc::new(TestQuery::new())),
        format: Box::new(move || {
            assert!(!format_panics, "a bug in creating format");
            Arc::new(TestFormat::new())
        }),
        ingests: Box::new(|_, _| {
            BTreeMap::from([(
                CollectionType("raw".to_owned()),
                Arc::new(TestIngest::new()) as Arc<dyn Ingest>,
            )])
        }),
        dirs: setup.dirs,
    };
    let stdout = CapturedOutput::new();
    let stderr = CapturedOutput::new();
    let io = Io {
        stdin: Box::new(Cursor::new(setup.stdin)),
        stdout: Box::new(stdout.clone()),
        stderr: Box::new(stderr.clone()),
        stdin_is_terminal: false,
        stdout_is_terminal: false,
        stderr_is_terminal: setup.stderr_is_terminal,
    };
    let args = std::iter::once("mdya")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let env = setup
        .env
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();

    let code = run_with(args, env, io, factories);

    let records = std::mem::take(&mut *records.lock().unwrap());
    Outcome {
        code,
        stdout: stdout.text(),
        stderr: stderr.text(),
        records,
        watchdog: watchdog.state(),
    }
}

fn nothing_assembled(outcome: &Outcome) -> bool {
    outcome.records.environment.is_empty()
        && outcome.records.log_settings.is_empty()
        && outcome.records.config_dirs.is_empty()
}

// Before anything is assembled

#[test]
fn the_version_is_written_without_assembling_anything() {
    let outcome = run(&["--version"], Setup::new());

    assert_eq!(outcome.code, ExitCode::SUCCESS);
    assert_eq!(outcome.stdout, format!("{}\n", version().version));
    assert!(nothing_assembled(&outcome));
}

#[test]
fn the_help_is_written_without_assembling_anything() {
    let outcome = run(&["--help"], Setup::new());

    assert_eq!(outcome.code, ExitCode::SUCCESS);
    assert!(
        outcome.stdout.starts_with("Search your own documents"),
        "{}",
        outcome.stdout
    );
    assert!(nothing_assembled(&outcome));
}

#[test]
fn an_argument_that_is_not_valid_is_a_usage_error_before_anything_is_assembled() {
    let outcome = run(&["nosuch"], Setup::new());

    assert_eq!(outcome.code, ExitCode::from(2));
    assert_eq!(outcome.stdout, "");
    assert!(
        outcome
            .stderr
            .starts_with("Error: the arguments are not valid"),
        "{}",
        outcome.stderr
    );
    assert!(nothing_assembled(&outcome));
}

#[test]
fn a_confirmation_without_a_terminal_is_a_usage_error_before_anything_is_assembled() {
    let outcome = run(&["collection", "remove", "notes"], Setup::new());

    assert_eq!(outcome.code, ExitCode::from(2));
    assert!(
        outcome.stderr.contains("a confirmation is needed"),
        "{}",
        outcome.stderr
    );
    assert!(nothing_assembled(&outcome));
}

#[test]
fn a_log_level_that_is_not_valid_is_a_usage_error_before_anything_is_assembled() {
    let outcome = run(&["--log-level", "loud", "status"], Setup::new());

    assert_eq!(outcome.code, ExitCode::from(2));
    assert!(
        outcome
            .stderr
            .contains("the log level \"loud\" is not valid"),
        "{}",
        outcome.stderr
    );
    assert!(nothing_assembled(&outcome));
}

// The settings and index directory

#[test]
fn the_default_settings_and_index_directory_is_under_the_home_directory() {
    let outcome = run(&["config", "list"], Setup::new());

    let data_dir = Path::new(HOME).join(".mdya");
    assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    assert_eq!(outcome.records.config_dirs, [data_dir.as_path()]);
    assert_eq!(
        outcome.records.environment,
        [crate::index::process_environment(&data_dir)]
    );
}

#[test]
fn a_relative_settings_and_index_directory_is_resolved_from_the_working_directory() {
    let outcome = run(&["--data-dir", "data", "config", "list"], Setup::new());

    assert_eq!(outcome.records.config_dirs, [Path::new(WORK).join("data")]);
}

#[test]
fn a_settings_and_index_directory_that_cannot_be_determined_ends_before_anything_is_assembled() {
    let mut setup = Setup::new();
    setup.dirs = test_dir_lookup(None, Ok(WORK));

    let outcome = run(&["config", "list"], setup);

    assert_eq!(outcome.code, ExitCode::from(1));
    assert!(
        outcome
            .stderr
            .starts_with("Error: the settings and index directory cannot be determined"),
        "{}",
        outcome.stderr
    );
    assert!(nothing_assembled(&outcome));
}

// Assembling

#[test]
fn settings_that_cannot_be_read_end_before_the_watchdog_starts() {
    let mut setup = Setup::new();
    setup.config = TestConfig::new().on_current(|| {
        Err(ConfigError::CannotRead {
            source: "the file is broken".into(),
        })
    });

    let outcome = run(&["status"], setup);

    assert_eq!(outcome.code, ExitCode::from(1));
    assert!(
        outcome
            .stderr
            .starts_with("Error: the settings cannot be read"),
        "{}",
        outcome.stderr
    );
    assert!(outcome.watchdog.started.is_empty());
}

#[test]
fn the_watchdog_runs_with_the_memory_limit_until_the_operation_ends() {
    let outcome = run(&["config", "list"], Setup::new());

    let started = &outcome.watchdog.started;
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].memory_limit_mb, 8192);
    assert_eq!(started[0].exit_code, 137);
    assert!(
        started[0]
            .message
            .starts_with("Error: mdya's memory usage went over the limit of 8192 MB"),
        "{}",
        started[0].message
    );
    assert!(started[0].message.ends_with('\n'));
    assert_eq!(outcome.watchdog.running, 0);
}

#[test]
fn embedding_gets_the_model_the_token_and_the_default_model_cache_directory() {
    let mut setup = Setup::new();
    setup.env = vec![("HF_TOKEN", "hf_secret")];

    let outcome = run(&["config", "list"], setup);

    assert_eq!(
        outcome.records.embeddings,
        [EmbeddingRecord {
            model: "cl-nagoya/ruri-v3-30m".to_owned(),
            hf_token: Some("hf_secret".to_owned()),
            model_cache_dir: Ok(Path::new(HOME).join(".mdya-models")),
        }]
    );
}

#[test]
fn a_relative_model_cache_directory_is_resolved_from_the_working_directory() {
    let outcome = run(
        &["--model-cache-dir", "models", "config", "list"],
        Setup::new(),
    );

    assert_eq!(
        outcome.records.embeddings[0].model_cache_dir,
        Ok(Path::new(WORK).join("models"))
    );
}

#[test]
fn a_model_cache_directory_that_cannot_be_determined_is_an_error_only_when_it_is_asked_for() {
    let mut setup = Setup::new();
    setup.dirs = test_dir_lookup(Some(HOME), Err(std::io::ErrorKind::NotFound));

    let outcome = run(&["--model-cache-dir", "models", "config", "list"], setup);

    assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    assert_eq!(
        outcome.records.embeddings[0].model_cache_dir,
        Err("the embedding model cache directory cannot be determined".to_owned())
    );
}

#[test]
fn an_empty_hf_token_is_no_token() {
    let mut setup = Setup::new();
    setup.env = vec![("HF_TOKEN", "")];

    let outcome = run(&["config", "list"], setup);

    assert_eq!(outcome.records.embeddings[0].hf_token, None);
}

#[test]
fn the_operation_runs_on_the_assembled_components() {
    let outcome = run(&["config", "list", "--json"], Setup::new());

    assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    let settings: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap();
    assert_eq!(settings["embedding.model"], json!("cl-nagoya/ruri-v3-30m"));
}

// The logs and the colors

#[test]
fn the_log_receiver_gets_the_log_format() {
    let outcome = run(&["--log-format", "json", "config", "list"], Setup::new());

    assert_eq!(outcome.records.log_settings, [(false, LogFormat::Json)]);
}

#[test]
fn stderr_is_colored_only_on_a_terminal_without_no_color() {
    let colored = |args: &[&str], terminal: bool, env: Vec<(&'static str, &'static str)>| {
        let mut setup = Setup::new();
        setup.stderr_is_terminal = terminal;
        setup.env = env;
        run(args, setup).records.log_settings[0].0
    };

    assert!(colored(&["config", "list"], true, vec![]));
    assert!(!colored(&["config", "list"], false, vec![]));
    assert!(!colored(&["--no-color", "config", "list"], true, vec![]));
    assert!(!colored(&["config", "list"], true, vec![("NO_COLOR", "")]));
}

#[test]
fn an_error_in_reading_the_arguments_is_not_colored_with_no_color() {
    let mut setup = Setup::new();
    setup.stderr_is_terminal = true;

    let outcome = run(&["--no-color", "nosuch"], setup);

    assert!(
        outcome.stderr.starts_with("Error: "),
        "{:?}",
        outcome.stderr
    );
}

#[test]
fn the_version_and_the_help_create_no_component() {
    for args in [&["--version"][..], &["--help"][..]] {
        let mut setup = Setup::new();
        // `format` would panic if it were created.
        setup.format_panics = true;

        let outcome = run(args, setup);

        assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    }
}

#[test]
fn format_that_reading_the_arguments_did_not_use_is_created_by_the_assembly() {
    let mut setup = Setup::new();
    setup.format_panics = true;

    // Reading these arguments does not use `format`; the assembly creates it.
    let outcome = run(&["config", "list"], setup);

    assert_eq!(outcome.code, ExitCode::from(101));
    assert!(
        outcome
            .stderr
            .starts_with("It happened while preparing to run mdya config list.\n"),
        "{}",
        outcome.stderr
    );
}

// Unexpected failures

#[test]
fn a_panic_while_reading_the_arguments_is_reported_as_an_unexpected_failure() {
    let mut setup = Setup::new();
    setup.format_panics = true;

    // Checking the file type creates `format`, which panics.
    let outcome = run(
        &["put", "notes", "a.md", "--file-type", "md", "a.md"],
        setup,
    );

    assert_eq!(outcome.code, ExitCode::from(101));
    assert!(
        outcome.stderr.starts_with(
            "It happened while reading the arguments.\n\
             Error: an unexpected failure occurred, which is a bug in mdya"
        ),
        "{}",
        outcome.stderr
    );
}

#[test]
fn a_panic_while_assembling_says_which_command_was_being_prepared() {
    let mut setup = Setup::new();
    setup.config = TestConfig::new().on_current(|| panic!("a bug in reading the settings"));

    let outcome = run(&["status"], setup);

    assert_eq!(outcome.code, ExitCode::from(101));
    assert!(
        outcome
            .stderr
            .starts_with("It happened while preparing to run mdya status.\n"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn a_panic_while_running_says_which_command_was_running() {
    let mut setup = Setup::new();
    setup.collections = TestCollections::new().on_list(|| panic!("a bug in listing"));

    let outcome = run(&["collection", "list"], setup);

    assert_eq!(outcome.code, ExitCode::from(101));
    assert!(
        outcome
            .stderr
            .starts_with("It happened while running mdya collection list.\n"),
        "{}",
        outcome.stderr
    );
}

// The MCP server over stdio

#[test]
fn the_mcp_server_answers_over_the_given_stdin_and_stdout_and_ends_when_stdin_ends() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ];
    let mut setup = Setup::new();
    setup.stdin = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect::<String>()
        .into_bytes();

    let outcome = run(&["mcp"], setup);

    assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    let answer: serde_json::Value =
        serde_json::from_str(outcome.stdout.lines().next().unwrap()).unwrap();
    assert_eq!(answer["id"], json!(0));
    assert_eq!(answer["result"]["serverInfo"]["name"], json!("mdya"));
}

#[test]
fn an_mcp_exchange_that_cannot_start_ends_with_the_error() {
    let outcome = run(&["mcp"], Setup::new());

    assert_eq!(outcome.code, ExitCode::from(1));
    assert!(
        outcome
            .stderr
            .starts_with("Error: the connection with the MCP client cannot be started"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn an_address_that_cannot_be_listened_on_ends_the_mcp_server_with_the_error() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();

    let outcome = run(&["mcp", "--http", "--address", &address], Setup::new());

    assert_eq!(outcome.code, ExitCode::from(1));
    assert!(
        outcome
            .stderr
            .starts_with(&format!("Error: cannot listen on {address}")),
        "{}",
        outcome.stderr
    );
}

// The logs and the colors, more

#[test]
fn no_color_after_the_end_of_the_options_is_not_the_option() {
    let mut setup = Setup::new();
    setup.stderr_is_terminal = true;

    let outcome = run(&["search", "--", "--no-color"], setup);

    assert!(outcome.records.log_settings[0].0);
}

#[test]
fn a_directive_in_rust_log_that_is_not_valid_is_ignored_with_a_warning() {
    let mut setup = Setup::new();
    setup.env = vec![("RUST_LOG", "loud,info")];

    let (outcome, logs) =
        crate::test_support::log_receiver::capture(|| run(&["config", "list"], setup));

    assert_eq!(outcome.code, ExitCode::SUCCESS, "{}", outcome.stderr);
    let warning = logs
        .iter()
        .find(|log| log.message.contains("RUST_LOG"))
        .unwrap();
    assert_eq!(warning.level, tracing::Level::WARN);
    assert!(
        warning
            .fields
            .iter()
            .any(|(name, value)| name == "directive" && value == "loud"),
        "{warning:?}"
    );
}

#[test]
fn the_progress_is_drawn_in_place_only_on_a_terminal_that_can_draw_it() {
    let env = |term: Option<&str>| {
        Environment(
            term.map(|term| (OsString::from("TERM"), OsString::from(term)))
                .into_iter()
                .collect(),
        )
    };

    assert!(progress_terminal(&env(Some("xterm")), true).is_some());
    assert!(progress_terminal(&env(Some("xterm")), false).is_none());
    assert!(progress_terminal(&env(Some("dumb")), true).is_none());
    // Outside Windows, a terminal without `TERM` cannot be drawn on.
    assert_eq!(progress_terminal(&env(None), true).is_some(), cfg!(windows));
}

#[cfg(unix)]
#[test]
fn an_hf_token_that_is_not_unicode_is_no_token() {
    use std::os::unix::ffi::OsStringExt;

    let env = Environment(vec![(
        OsString::from("HF_TOKEN"),
        OsString::from_vec(vec![0x68, 0xff]),
    )]);

    assert_eq!(hf_token(&env), None);
}

// Small parts

#[test]
fn an_output_size_limit_of_0_applies_none() {
    assert_eq!(size_limit(0), None);
    assert_eq!(size_limit(1024), Some(1024));
}

#[test]
fn an_environment_variable_is_found_by_its_name() {
    let env = Environment(vec![
        (OsString::from("TERM"), OsString::from("xterm")),
        (OsString::from("term"), OsString::from("other")),
    ]);

    let found = env.get("TERM").and_then(|value| value.to_str());

    if cfg!(windows) {
        // Windows does not tell the case of the names apart; the last wins.
        assert_eq!(found, Some("other"));
    } else {
        assert_eq!(found, Some("xterm"));
    }
}
