//! Integration tests of the mdya executable, which runs as a separate
//! process: the logs on stderr, as the log settings shape them, and what only
//! the executable does around the components, which running mdya in the
//! test's own process (`Workspace::run`) leaves out: the panic hook, and the
//! pipe on stdout that keeps what the libraries write there out of the results
//! and out of the MCP exchange.
//!
//! For a panic and a library's write to stdout, the tests run this test
//! executable again, in a process of its own, where an ignored test runs mdya
//! as the executable does, through `mdya::gateway::run_replacing`, with one
//! component replaced.

// `allow-unwrap-in-tests` and `allow-panic-in-tests` in clippy.toml cover
// test functions and `#[cfg(test)]` code, which the helpers below, and the
// components that the tests put in place of the real ones, are not.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::ffi::OsString;
use std::process::{Command, ExitCode, Output};
use std::sync::Arc;

use mdya::gateway::Factories;
use mdya::query::{ChunkRangeRequest, Query, QueryError, SearchRequest, SearchResult};
use mdya::types::{CollectionName, Key};
use serde_json::{Value, json};

use crate::common::Workspace;
use crate::mcp_server::{StdioServer, failed, succeeded};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The warning that mdya writes when `RUST_LOG` has a directive that is not
/// valid.
const IGNORED_DIRECTIVE: &str = "a directive in RUST_LOG is not valid, so it was ignored";

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_log_is_one_line_on_stderr_in_the_compact_format_by_default() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        with_rust_log(&workspace, "mdya=bogus").args(["config", "list"]),
    );

    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].contains(&format!(
            " WARN mdya::gateway::entry: {IGNORED_DIRECTIVE} directive=mdya=bogus detail="
        )),
        "{stderr}"
    );
    assert!(!text(&output.stdout).contains(IGNORED_DIRECTIVE));
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_log_is_a_json_object_on_one_line_in_the_json_format() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        with_rust_log(&workspace, "mdya=bogus").args(["--log-format", "json", "config", "list"]),
    );

    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    let log: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(log["level"], "WARN", "{log}");
    assert_eq!(log["target"], "mdya::gateway::entry", "{log}");
    assert_eq!(log["fields"]["message"], IGNORED_DIRECTIVE, "{log}");
    assert_eq!(log["fields"]["directive"], "mdya=bogus", "{log}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_log_spreads_over_lines_in_the_pretty_format() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        with_rust_log(&workspace, "mdya=bogus").args(["--log-format", "pretty", "config", "list"]),
    );

    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(lines.len() > 1, "{stderr}");
    assert!(
        lines[0].contains(&format!(
            " WARN mdya::gateway::entry: {IGNORED_DIRECTIVE}, directive: mdya=bogus, detail: "
        )),
        "{stderr}"
    );
    // The place in the source that wrote the log, on a line of its own.
    assert!(lines[1].trim_start().starts_with("at src/"), "{stderr}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_log_level_option_comes_before_rust_log() {
    let workspace = Workspace::new();

    let from_rust_log = output_of(
        &workspace,
        with_rust_log(&workspace, "mdya=bogus").args(["config", "list"]),
    );
    let from_the_option = output_of(
        &workspace,
        with_rust_log(&workspace, "mdya=bogus").args(["--log-level", "warn", "config", "list"]),
    );

    assert!(from_rust_log.status.success());
    assert!(
        text(&from_rust_log.stderr).contains(IGNORED_DIRECTIVE),
        "{}",
        text(&from_rust_log.stderr)
    );
    // With the option, `RUST_LOG` is not read, so nothing in it is found not
    // valid.
    assert!(from_the_option.status.success());
    assert_eq!(text(&from_the_option.stderr), "");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn rust_log_keeps_its_valid_directives_which_can_name_a_target() {
    let workspace = Workspace::new();

    for (rust_log, warned) in [
        // No valid directive is left, so the default directives, which write
        // the warnings of mdya, apply.
        ("mdya=bogus", true),
        ("mdya=bogus,error", false),
        ("mdya::gateway=warn,mdya=bogus", true),
        ("lance=warn,mdya=bogus", false),
    ] {
        let output = output_of(
            &workspace,
            with_rust_log(&workspace, rust_log).args(["config", "list"]),
        );

        let stderr = text(&output.stderr);
        assert!(output.status.success(), "RUST_LOG={rust_log}: {stderr}");
        if warned {
            assert!(
                stderr.contains(IGNORED_DIRECTIVE),
                "RUST_LOG={rust_log}: {stderr}"
            );
        } else {
            assert_eq!(stderr, "", "RUST_LOG={rust_log}");
        }
    }
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn by_default_the_logs_below_warn_are_left_out() {
    let workspace = Workspace::new();
    let added = workspace.run_executable(&["collection", "add", "notes", "--type", "raw"]);
    assert!(added.status.success(), "{}", text(&added.stderr));

    let by_default = output_of(
        &workspace,
        without_rust_log(&workspace).args(["collection", "list"]),
    );
    let from_debug = output_of(
        &workspace,
        without_rust_log(&workspace).args(["--log-level", "debug", "collection", "list"]),
    );

    assert!(by_default.status.success());
    assert_eq!(text(&by_default.stderr), "");
    // LanceDB, which keeps the collections, writes debug logs as it reads
    // them.
    let stderr = text(&from_debug.stderr);
    assert!(from_debug.status.success(), "{stderr}");
    assert!(
        stderr.lines().any(|line| line.contains(" DEBUG ")),
        "{stderr}"
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn messages_are_written_whatever_the_log_level() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        without_rust_log(&workspace).args([
            "--log-level",
            "error",
            "collection",
            "add",
            "notes",
            "--type",
            "raw",
        ]),
    );

    let stderr = text(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Created the collection \"notes\" (raw)."),
        "{stderr}"
    );
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn characters_in_a_log_are_replaced_and_escaped_in_json() {
    let workspace = Workspace::new();
    let directive = "mdya=x\u{1b}\u{202e}y";

    let written = |format: &str| {
        let output = output_of(
            &workspace,
            with_rust_log(&workspace, directive).args(["--log-format", format, "config", "list"]),
        );
        assert!(output.status.success());
        text(&output.stderr)
    };

    for format in ["compact", "pretty"] {
        let stderr = written(format);
        assert!(
            stderr.contains("mdya=x\\u{1b}\\u{202e}y"),
            "{format}: {stderr}"
        );
        assert!(
            !stderr.contains(['\u{1b}', '\u{202e}']),
            "{format}: {stderr:?}"
        );
    }
    let stderr = written("json");
    assert!(stderr.contains("mdya=x\\u001b\\u202ey"), "{stderr}");
    assert!(!stderr.contains(['\u{1b}', '\u{202e}']), "{stderr:?}");
    let log: Value = serde_json::from_str(stderr.lines().next().unwrap()).unwrap();
    assert_eq!(log["fields"]["directive"], directive, "{log}");
}

#[cfg(unix)]
#[test]
#[ignore = "runs the real embedding model; run it with `just integration`"]
fn a_log_is_written_above_the_progress_bar_which_is_drawn_again_below_it() {
    use crate::common::Locked;

    let workspace = Workspace::with_real_model();
    let source = workspace.dir("notes");
    std::fs::write(source.join("a.md"), "# a\n\nalpha\n").unwrap();
    let locked = source.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let added = workspace.run_executable(&["collection", "add", "notes", source.to_str().unwrap()]);
    assert!(added.status.success(), "{}", text(&added.stderr));
    let _locked = Locked::new(&locked, 0o000);

    let mut update = workspace.executable();
    update
        .env_remove("RUST_LOG")
        .args(["--no-color", "update", "notes"]);
    let (status, shown) = stderr_on_a_terminal(update);

    // The update draws the bar as it starts, and the walk through the
    // directory, which comes after, warns that it cannot read `locked`.
    let warning = shown
        .find(" WARN mdya::file_ingest::walk: a part of the directory cannot be read")
        .unwrap_or_else(|| panic!("{shown:?}"));
    let log_start = shown[..warning].rfind('\r').unwrap() + 1;
    let log_end = warning + shown[warning..].find('\n').unwrap() + 1;
    // Before the log, the bar is drawn over with as many spaces, and the
    // cursor is back at the start of the line.
    let cleared = shown[..log_start].strip_suffix('\r').unwrap();
    let (drawn, spaces) = cleared.rsplit_once('\r').unwrap();
    let bar = drawn.rsplit('\r').next().unwrap();
    assert!(bar.contains("Updating"), "{shown:?}");
    assert_eq!(spaces, " ".repeat(bar.chars().count()), "{shown:?}");
    // After the log, on the next line, the bar is drawn again.
    assert!(
        shown[log_end..].starts_with(&format!("\r{bar}")),
        "{shown:?}"
    );
    // The update goes on to its end, which is exit code 1 for the part of the
    // directory that it could not read.
    assert!(
        shown.contains("Updated the collection \"notes\": 1 added"),
        "{shown:?}"
    );
    assert_eq!(status.code(), Some(1), "{shown:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_panic_on_the_command_line_ends_mdya_with_101_and_asks_for_a_bug_report() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        &mut in_a_process_of_its_own(
            QUERY_PANICS,
            &workspace,
            &["search", "--mode", "fts", "words"],
        ),
    );

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(101), "{stderr}");
    assert_eq!(mdya_stdout(&output), "");
    assert!(
        stderr.contains("an unexpected failure occurred, which is a bug in mdya"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Report the bug at https://github.com/yoshihirosuzuki/mdya/issues"),
        "{stderr}"
    );
    // The clue to where the bug is: the version, the place of the panic, and
    // its message, with its control character and its character that changes
    // the direction of text replaced.
    assert!(
        stderr.contains(&format!(
            "{VERSION} panicked at tests/integration/executable.rs:"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(":\n{SHOWN_PANIC_MESSAGE}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains(['\u{1b}', '\u{202e}']), "{stderr:?}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn a_panic_in_a_call_to_the_mcp_server_fails_that_call_and_the_server_goes_on() {
    let workspace = Workspace::new();
    let mut server =
        StdioServer::start_as_it_is(in_a_process_of_its_own(QUERY_PANICS, &workspace, &["mcp"]));
    server.skip_past(MDYA_STARTS);
    server.initialize();
    let search = json!({ "query": "words", "mode": "fts" });

    let panicked = server.call("search", search.clone());
    let listed = server.call("list_collections", json!({}));
    let version = server.call("get_version", json!({}));
    let panicked_again = server.call("search", search);
    let ended = server.finish();

    for result in [&panicked, &panicked_again] {
        let error = failed(result);
        assert_eq!(error["code"], "internal", "{error}");
        assert_eq!(error["details"], json!({}), "{error}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("an unexpected failure occurred, which is a bug in mdya"),
            "{error}"
        );
    }
    assert_eq!(succeeded(&listed)["collections"], json!([]), "{listed}");
    succeeded(&version);
    assert_eq!(ended.status.code(), Some(0), "{}", ended.stderr);
    assert!(ended.stdout.is_empty(), "{:?}", ended.stdout);
    assert!(
        ended.stderr.contains(&format!(
            "{VERSION} panicked at tests/integration/executable.rs:"
        )),
        "{}",
        ended.stderr
    );
    assert!(
        ended.stderr.contains(SHOWN_PANIC_MESSAGE),
        "{}",
        ended.stderr
    );
    assert!(
        !ended.stderr.contains(['\u{1b}', '\u{202e}']),
        "{:?}",
        ended.stderr
    );
    assert!(!workspace.model_was_prepared());
}

/// The tests that panic in a process of their own show how mdya goes on
/// after a panic that unwinds, but not that the executable, as it is shipped,
/// unwinds: the test executable unwinds whatever the profile says, as Cargo
/// ignores the profile's `panic` setting for tests. The executable is shipped
/// from the `dist` profile, which takes its settings from `release`.
#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn the_shipped_executable_is_built_to_unwind_on_a_panic() {
    let manifest = include_str!("../../Cargo.toml");

    let release = section(manifest, "[profile.release]");
    let dist = section(manifest, "[profile.dist]");

    assert!(
        release.lines().any(|line| line == "panic = \"unwind\""),
        "{release}"
    );
    assert!(
        dist.lines().any(|line| line == "inherits = \"release\""),
        "{dist}"
    );
    assert!(
        !dist.lines().any(|line| line.starts_with("panic")),
        "{dist}"
    );
}

/// The lines of the table that starts with the line `header` in `manifest`,
/// up to the next table.
fn section<'a>(manifest: &'a str, header: &str) -> &'a str {
    let start = manifest
        .find(&format!("\n{header}\n"))
        .unwrap_or_else(|| panic!("no {header} in Cargo.toml"))
        + 1;
    let end = manifest[start + 1..]
        .find("\n[")
        .map_or(manifest.len(), |end| start + 1 + end);
    &manifest[start..end]
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn what_a_library_writes_to_stdout_stays_out_of_the_result_on_the_command_line() {
    let workspace = Workspace::new();

    let output = output_of(
        &workspace,
        &mut in_a_process_of_its_own(CONFIG_WRITES, &workspace, &["config", "list", "--json"]),
    );

    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let settings: Value = serde_json::from_str(&mdya_stdout(&output)).unwrap();
    assert_eq!(
        settings["embedding.model"], "cl-nagoya/ruri-v3-30m",
        "{settings}"
    );
    assert!(!text(&output.stdout).contains(LIBRARY_LINE));
    assert!(written_to_the_logs(&stderr), "{stderr}");
}

#[test]
#[ignore = "left out of the default tests to keep them few; run it with `just integration`"]
fn what_a_library_writes_to_stdout_stays_out_of_the_mcp_exchange() {
    let workspace = Workspace::new();
    let mut server =
        StdioServer::start_as_it_is(in_a_process_of_its_own(CONFIG_WRITES, &workspace, &["mcp"]));
    server.skip_past(MDYA_STARTS);

    // Every line that the server writes on stdout meanwhile must be a
    // JSON-RPC message.
    server.initialize();
    let listed = server.call("list_collections", json!({}));
    let ended = server.finish();

    assert_eq!(succeeded(&listed)["collections"], json!([]), "{listed}");
    assert_eq!(ended.status.code(), Some(0), "{}", ended.stderr);
    assert!(ended.stdout.is_empty(), "{:?}", ended.stdout);
    assert!(written_to_the_logs(&ended.stderr), "{}", ended.stderr);
    assert!(!workspace.model_was_prepared());
}

/// The message that the replaced query component panics with, which has a
/// control character and a character that changes the direction of text.
const PANIC_MESSAGE: &str = "the query panicked, with \u{1b} and \u{202e} in its message";

/// The message as mdya shows it on stderr.
const SHOWN_PANIC_MESSAGE: &str = "the query panicked, with \\u{1b} and \\u{202e} in its message";

/// What the replaced settings component writes to stdout, as a library might.
const LIBRARY_LINE: &str = "written to stdout by the replaced settings component";

/// Whether `stderr` has the warning that a library wrote `LIBRARY_LINE` to
/// stdout.
fn written_to_the_logs(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        line.contains("a library wrote a line to stdout") && line.contains(LIBRARY_LINE)
    })
}

/// The test of this module that runs mdya with a query component that
/// panics.
const QUERY_PANICS: &str = "runs_mdya_with_a_query_component_that_panics";

/// The test of this module that runs mdya with a settings component that
/// writes to stdout.
const CONFIG_WRITES: &str = "runs_mdya_with_a_settings_component_that_writes_to_stdout";

#[test]
#[ignore = "run in a process of its own by the tests above"]
fn runs_mdya_with_a_query_component_that_panics() {
    let Some(args) = arguments_of_this_process() else {
        return;
    };
    run_mdya_and_exit(args, |factories| {
        factories.query = Box::new(|_, _| Arc::new(PanickingQuery));
    });
}

#[test]
#[ignore = "run in a process of its own by the tests above"]
fn runs_mdya_with_a_settings_component_that_writes_to_stdout() {
    let Some(args) = arguments_of_this_process() else {
        return;
    };
    run_mdya_and_exit(args, |factories| {
        factories.config = Box::new(|dir| {
            write_the_library_line();
            mdya::config::create(dir)
        });
    });
}

/// A query component that panics whenever it is called.
struct PanickingQuery;

#[async_trait::async_trait]
impl Query for PanickingQuery {
    async fn search(&self, _request: SearchRequest) -> Result<SearchResult, QueryError> {
        panic!("{PANIC_MESSAGE}")
    }

    async fn get_document(
        &self,
        _collection: &CollectionName,
        _key: &Key,
        _range: ChunkRangeRequest,
    ) -> Result<String, QueryError> {
        panic!("{PANIC_MESSAGE}")
    }
}

#[expect(
    clippy::print_stdout,
    reason = "the replaced settings component writes to stdout, as a library might"
)]
fn write_the_library_line() {
    println!("{LIBRARY_LINE}");
}

/// Set on the process of its own that a test above starts, to the arguments
/// that mdya runs with there, one on each line. Without it, as in an ordinary
/// run of the tests, the tests that run mdya there do nothing.
const ARGUMENTS_OF_THIS_PROCESS: &str = "MDYA_TEST_ARGUMENTS_OF_THIS_PROCESS";

/// What the test in a process of its own writes on stdout just before mdya
/// runs there, so that the test that started it can tell what mdya wrote from
/// what the test harness wrote before.
const MDYA_STARTS: &str = "mdya starts here";

/// This test executable, to run the test `test` of this module in a process
/// of its own, where mdya runs with `args` on `workspace`'s directories. The
/// process ends with mdya's exit code.
fn in_a_process_of_its_own(test: &str, workspace: &Workspace, args: &[&str]) -> Command {
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, rest)| rest);
    let arguments: Vec<String> = workspace
        .arguments(args)
        .into_iter()
        .map(|argument| argument.into_string().unwrap())
        .collect();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            format!("{module}::{test}").as_str(),
            "--exact",
            "--ignored",
            // Without it, the test harness takes what the test writes to
            // stdout with `println!`, before it reaches the process's stdout.
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ARGUMENTS_OF_THIS_PROCESS, arguments.join("\n"))
        .env_remove("RUST_LOG");
    command
}

/// The arguments that a test above set on this process, or none in an
/// ordinary run of the tests.
#[expect(
    clippy::disallowed_methods,
    reason = "a test above sets the variable on the process of its own that it starts"
)]
fn arguments_of_this_process() -> Option<Vec<OsString>> {
    let arguments = std::env::var_os(ARGUMENTS_OF_THIS_PROCESS)?;
    Some(
        arguments
            .to_str()
            .unwrap()
            .lines()
            .map(OsString::from)
            .collect(),
    )
}

/// Runs mdya with `args`, as the executable does, with the real components as
/// `replace` leaves them, and ends this process with mdya's exit code, so that
/// the test that started the process sees it.
fn run_mdya_and_exit(args: Vec<OsString>, replace: impl FnOnce(&mut Factories)) -> ! {
    mark_where_mdya_starts();
    let code = mdya::gateway::run_replacing(args, replace);
    // `ExitCode` cannot be read, only compared.
    let code = (0..=u8::MAX)
        .find(|value| ExitCode::from(*value) == code)
        .unwrap();
    std::process::exit(i32::from(code))
}

#[expect(
    clippy::print_stdout,
    reason = "the mark tells the test that started this process where mdya's stdout starts"
)]
fn mark_where_mdya_starts() {
    println!("{MDYA_STARTS}");
}

/// What mdya wrote on stdout in a test's process of its own: what follows
/// the mark. The process ends right after mdya, before the test harness
/// writes anything more.
fn mdya_stdout(output: &Output) -> String {
    let stdout = text(&output.stdout);
    let (_, after) = stdout
        .split_once(&format!("{MDYA_STARTS}\n"))
        .unwrap_or_else(|| panic!("mdya did not start: {stdout:?}"));
    after.to_owned()
}

/// The mdya executable on `workspace`'s directories, with `RUST_LOG` set to
/// `rust_log`.
fn with_rust_log(workspace: &Workspace, rust_log: &str) -> Command {
    let mut command = workspace.executable();
    command.env("RUST_LOG", rust_log);
    command
}

/// The mdya executable on `workspace`'s directories, without the `RUST_LOG`
/// of the shell that runs the tests.
fn without_rust_log(workspace: &Workspace) -> Command {
    let mut command = workspace.executable();
    command.env_remove("RUST_LOG");
    command
}

/// Runs `command`, made from `workspace`, and checks that it did not prepare
/// the embedding model, as `Workspace::run_executable` does.
fn output_of(workspace: &Workspace, command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        !workspace.model_was_prepared(),
        "{command:?} prepared the embedding model"
    );
    output
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Runs `command` with its stderr on a terminal of 80 columns, with `TERM`
/// set and with its stdin and stdout on the null device, and returns how it
/// ended and what it wrote on the terminal, as the terminal passes it on.
#[cfg(unix)]
fn stderr_on_a_terminal(mut command: Command) -> (std::process::ExitStatus, String) {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Stdio;

    let mut controller = -1;
    let mut terminal = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the pointers are to locals that live through the call. No name
    // is asked for, and the terminal keeps its default settings.
    let opened = unsafe {
        libc::openpty(
            &mut controller,
            &mut terminal,
            std::ptr::null_mut(),
            std::ptr::null_mut::<libc::termios>(),
            // libc declares this pointer `*mut` on some systems, such as
            // macOS, and `*const` on others, such as Linux, where clippy
            // flags `&mut` as unneeded; a raw pointer fits either.
            &raw mut size,
        )
    };
    assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: `openpty` opened the two, and nothing else owns them.
    let (controller, terminal) = unsafe {
        (
            OwnedFd::from_raw_fd(controller),
            OwnedFd::from_raw_fd(terminal),
        )
    };
    command
        .env("TERM", "xterm")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(terminal));
    let mut child = command.spawn().unwrap();
    // The command keeps the terminal open, and the reading below ends only
    // once nothing keeps it open.
    drop(command);

    let mut controller = std::fs::File::from(controller);
    let mut shown = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match controller.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => shown.extend_from_slice(&buffer[..read]),
            // Once the terminal is closed, reading may end with this error
            // instead of with the end of what was written.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("{error}"),
        }
    }
    let status = child.wait().unwrap();
    (status, String::from_utf8(shown).unwrap())
}
