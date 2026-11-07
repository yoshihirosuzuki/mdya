use std::ffi::OsString;
use std::path::PathBuf;

use crate::collection::{CollectionChange, CollectionError};
use crate::config::ConfigError;
use crate::format::test_double::TestFormat;
use crate::ingest::IngestError;
use crate::query::{ChunkRangeRequest, QueryError, SearchMode, SearchUnit};
use crate::types::{CollectionName, CollectionType, Key, TypeAttributes};

use super::super::super::operations::SearchLimit;
use super::super::{
    Action, CliError, ContentSource, Invocation, LogFormat, McpTransport, Operation, Output,
};
use super::read_args;

fn read(args: &[&str]) -> Result<Invocation, CliError> {
    let mut all: Vec<OsString> = vec!["mdya".into()];
    all.extend(args.iter().map(OsString::from));
    let format = TestFormat::new()
        .with_media_type("md", "markdown")
        .with_media_type("text/markdown", "markdown");
    read_args(all, &format)
}

fn operation(args: &[&str]) -> Operation {
    match read(args) {
        Ok(Invocation {
            action: Action::Operation(operation),
            ..
        }) => operation.0,
        other => panic!("not an operation: {other:?}"),
    }
}

fn help(args: &[&str]) -> String {
    match read(args) {
        Ok(Invocation {
            action: Action::Help(help),
            ..
        }) => help.0,
        other => panic!("not the help: {other:?}"),
    }
}

/// The detail of the usage error, or a panic when it is another result.
fn usage(args: &[&str]) -> String {
    match read(args) {
        Err(CliError::Usage { detail }) => detail,
        other => panic!("not a usage error: {other:?}"),
    }
}

fn attributes(path: &str) -> TypeAttributes {
    let mut attributes = serde_json::Map::new();
    attributes.insert("path".to_owned(), serde_json::json!(path));
    TypeAttributes(attributes)
}

// The help that the user approved, in clap's layout.
const MDYA_HELP: &str = "\
Search your own documents on this computer, from the command line or over MCP

Usage: mdya [OPTIONS] <COMMAND>

Commands:
  collection  Add, list, change, and remove collections
  update      Bring collections up to date with their sources
  put         Ingest one document, from a file or stdin
  remove      Remove a document from a collection
  search      Search the documents
  get         Get the text of a document, or of a range of its chunks
  status      Show the state of the index
  config      Show and change the settings
  embedding   Rebuild the embeddings
  optimize    Maintain the index
  mcp         Start the MCP server

Options:
      --data-dir <DIR>         The settings and index directory (default: ~/.mdya)
      --model-cache-dir <DIR>  The embedding model cache directory (default: ~/.mdya-models)
      --log-level <LEVEL>      Write logs from this level: error, warn, info, debug, or trace, or per target such as mdya=debug [default: warn]
      --log-format <FORMAT>    How to write logs [default: compact] [possible values: compact, pretty, json]
      --no-color               Do not color the output
  -h, --help                   Print help
      --version                Print the version
";

const SEARCH_HELP: &str = "\
Search the documents

Usage: mdya search [OPTIONS] <QUERY>

Arguments:
  <QUERY>  What to search for

Options:
      --mode <MODE>              How to search [default: hybrid] [possible values: fts, vector, hybrid]
  -c, --collection <COLLECTION>  A collection to search; separate several with commas, or give it more than once (default: all)
  -n, --limit <LIMIT>            The most hits to return, from 1 to 9007199254740991 [default: 20]
      --unit <UNIT>              Return each matching document once, or each matching chunk [default: document] [possible values: document, chunk]
      --json                     Write the result as JSON
  -h, --help                     Print help
";

#[test]
fn the_help_of_mdya_is_the_approved_one() {
    assert_eq!(help(&["--help"]), MDYA_HELP);
    assert_eq!(help(&["-h"]), MDYA_HELP);
}

#[test]
fn the_help_of_search_is_the_approved_one() {
    assert_eq!(help(&["search", "--help"]), SEARCH_HELP);
}

// The help is shown whatever the other arguments are, once the commands are
// right.
#[test]
fn the_help_is_shown_whatever_the_other_arguments_are() {
    for args in [
        &["search", "--limit", "0", "--help"][..],
        &["search", "--limit", "abc", "--mode", "bad", "-h"],
        &["search", "--bogus", "--help"],
        &["search", "q", "extra", "--help"],
        &["--no-color", "search", "--limit", "--help"],
        &["--log-format", "bad", "search", "--help"],
    ] {
        assert_eq!(help(args), SEARCH_HELP, "{args:?}");
    }
}

#[test]
fn the_help_of_a_command_under_another_names_both() {
    let text = help(&["collection", "add", "--help"]);

    assert!(
        text.starts_with(
            "Create a collection\n\nUsage: mdya collection add [OPTIONS] <NAME> [LOCATION]\n"
        ),
        "{text}"
    );
}

#[test]
fn the_help_with_a_command_that_does_not_exist_is_a_usage_error() {
    assert_eq!(
        usage(&["nosuchcmd", "--help"]),
        "unrecognized subcommand 'nosuchcmd'"
    );
    assert_eq!(
        usage(&["collection", "nosuch", "--help"]),
        "unrecognized subcommand 'nosuch'"
    );
}

// clap shows the help as soon as it reads the flag, so the commands are
// checked apart from clap's reading.
#[test]
fn the_help_before_a_command_that_does_not_exist_is_a_usage_error_too() {
    assert_eq!(
        usage(&["--help", "nosuchcmd"]),
        "unrecognized subcommand 'nosuchcmd'"
    );
    assert_eq!(
        usage(&["-h", "collection", "nosuch"]),
        "unrecognized subcommand 'nosuch'"
    );
    assert_eq!(
        usage(&["collection", "--help", "nosuch"]),
        "unrecognized subcommand 'nosuch'"
    );
}

#[test]
fn the_help_before_a_command_that_exists_is_its_help() {
    assert_eq!(help(&["--help", "search"]), SEARCH_HELP);
    assert_eq!(help(&["-h", "search", "q", "--limit", "0"]), SEARCH_HELP);
}

// What is wrong with the other arguments does not hide a command that does
// not exist.
#[test]
fn a_command_that_does_not_exist_after_an_unknown_option_is_a_usage_error() {
    assert_eq!(
        usage(&["--bogus", "nosuchcmd", "--help"]),
        "unrecognized subcommand 'nosuchcmd'"
    );
    assert_eq!(
        usage(&["collection", "--bogus", "nosuch", "--help"]),
        "unrecognized subcommand 'nosuch'"
    );
}

// The value of an option that takes one is not read as a command.
#[test]
fn the_values_of_options_are_passed_over_when_the_commands_are_checked() {
    assert_eq!(
        help(&["--data-dir", "search", "search", "--help"]),
        SEARCH_HELP
    );
    assert_eq!(
        help(&["--data-dir=x", "-h", "search", "-n", "--help"]),
        SEARCH_HELP
    );
    assert_eq!(
        help(&[
            "--log-level",
            "debug",
            "search",
            "q",
            "--mode",
            "x",
            "--help"
        ]),
        SEARCH_HELP
    );
}

// The words after a command without commands of its own are its arguments.
#[test]
fn the_words_after_a_command_without_commands_are_not_checked() {
    assert_eq!(help(&["search", "--help", "nosuch"]), SEARCH_HELP);
}

#[test]
fn a_help_flag_after_two_dashes_is_not_the_help() {
    assert!(matches!(
        operation(&["search", "--", "--help"]),
        Operation::Search { query, .. } if query == "--help"
    ));
}

#[test]
fn no_command_is_a_usage_error() {
    assert_eq!(usage(&[]), "no command was given");
    assert_eq!(usage(&["--no-color"]), "no command was given");
}

#[test]
fn a_command_without_its_own_command_is_a_usage_error_without_the_help() {
    assert_eq!(usage(&["collection"]), "mdya collection needs a command");
    assert_eq!(
        usage(&["--data-dir", "d", "config"]),
        "mdya config needs a command"
    );
}

#[test]
fn version_is_its_own_action() {
    assert!(matches!(
        read(&["--version"]),
        Ok(Invocation {
            action: Action::Version,
            ..
        })
    ));
}

#[test]
fn the_global_options_are_read_as_given() {
    let invocation = read(&[
        "--log-level",
        "mdya=debug",
        "--log-format",
        "json",
        "--no-color",
        "--data-dir",
        "~/data",
        "--model-cache-dir",
        "models",
        "status",
    ])
    .unwrap();

    let options = invocation.options;
    assert_eq!(options.log_level.as_deref(), Some("mdya=debug"));
    assert_eq!(options.log_format, LogFormat::Json);
    assert!(options.no_color);
    assert_eq!(options.data_dir, Some(PathBuf::from("~/data")));
    assert_eq!(options.model_cache_dir, Some(PathBuf::from("models")));
}

#[test]
fn the_global_options_have_their_defaults() {
    let options = read(&["status"]).unwrap().options;

    assert_eq!(options.log_level, None);
    assert_eq!(options.log_format, LogFormat::Compact);
    assert!(!options.no_color);
    assert_eq!(options.data_dir, None);
    assert_eq!(options.model_cache_dir, None);
}

#[test]
fn the_log_formats_are_read_and_another_is_a_usage_error() {
    let format = |name: &str| {
        read(&["--log-format", name, "status"])
            .unwrap()
            .options
            .log_format
    };

    assert_eq!(format("compact"), LogFormat::Compact);
    assert_eq!(format("pretty"), LogFormat::Pretty);
    assert_eq!(
        usage(&["--log-format", "xml", "status"]),
        "the log format xml is not one of compact, pretty, and json"
    );
}

#[test]
fn collection_add_reads_its_arguments_and_defaults_to_a_directory() {
    assert_eq!(
        format!(
            "{:?}",
            operation(&[
                "collection",
                "add",
                "notes",
                "~/notes",
                "--description",
                "my notes"
            ])
        ),
        format!(
            "{:?}",
            Operation::CreateCollection {
                name: CollectionName("notes".to_owned()),
                collection_type: CollectionType("directory".to_owned()),
                description: Some("my notes".to_owned()),
                attributes: attributes("~/notes"),
            }
        )
    );
}

#[test]
fn collection_add_of_a_raw_collection_has_no_attributes() {
    assert!(matches!(
        operation(&["collection", "add", "memo", "--type", "raw"]),
        Operation::CreateCollection { collection_type, attributes, .. }
            if collection_type.0 == "raw" && attributes.0.is_empty()
    ));
}

#[test]
fn collection_add_of_an_archive_takes_its_location_and_needs_one() {
    assert!(matches!(
        operation(&["collection", "add", "old", "--type", "archive", "notes.zip"]),
        Operation::CreateCollection { collection_type, attributes: given, .. }
            if collection_type.0 == "archive" && given == attributes("notes.zip")
    ));
    assert!(matches!(
        read(&["collection", "add", "old", "--type", "archive"]),
        Err(CliError::Ingest { collection_type, source: IngestError::AttributesMissingOrExtra })
            if collection_type.0 == "archive"
    ));
}

#[test]
fn collection_add_checks_the_name() {
    assert!(matches!(
        read(&["collection", "add", "bad name", "~/notes"]),
        Err(CliError::Collection { name, source: CollectionError::NameInvalid }) if name.0 == "bad name"
    ));
}

#[test]
fn collection_add_checks_the_attributes_against_the_type() {
    assert!(matches!(
        read(&["collection", "add", "notes"]),
        Err(CliError::Ingest { collection_type, source: IngestError::AttributesMissingOrExtra })
            if collection_type.0 == "directory"
    ));
    assert!(matches!(
        read(&["collection", "add", "memo", "--type", "raw", "~/memo"]),
        Err(CliError::Ingest { collection_type, source: IngestError::AttributesMissingOrExtra })
            if collection_type.0 == "raw"
    ));
}

#[test]
fn collection_add_of_an_unknown_type_is_a_usage_error() {
    assert_eq!(
        usage(&["collection", "add", "notes", "--type", "web", "x"]),
        "the collection type web is not one of directory, archive, and raw"
    );
}

#[test]
fn collection_remove_reads_yes_and_no_optimize() {
    assert!(matches!(
        operation(&["collection", "remove", "notes"]),
        Operation::DeleteCollection { name, yes: false, optimize: true } if name.0 == "notes"
    ));
    assert!(matches!(
        operation(&["collection", "remove", "notes", "--yes", "--no-optimize"]),
        Operation::DeleteCollection {
            yes: true,
            optimize: false,
            ..
        }
    ));
}

#[test]
fn collection_list_status_and_config_list_take_json() {
    assert!(matches!(
        operation(&["collection", "list"]),
        Operation::ListCollections {
            output: Output::Human
        }
    ));
    assert!(matches!(
        operation(&["collection", "list", "--json"]),
        Operation::ListCollections {
            output: Output::Json
        }
    ));
    assert!(matches!(
        operation(&["status", "--json"]),
        Operation::Status {
            output: Output::Json
        }
    ));
    assert!(matches!(
        operation(&["config", "list"]),
        Operation::ListSettings {
            output: Output::Human
        }
    ));
}

#[test]
fn collection_set_reads_what_to_change() {
    let change = |args: &[&str]| match operation(args) {
        Operation::ChangeCollection { change, .. } => change,
        other => panic!("{other:?}"),
    };

    assert_eq!(
        change(&["collection", "set", "notes", "--description", "d"]),
        CollectionChange {
            description: Some(Some("d".to_owned())),
            attributes: None,
        }
    );
    assert_eq!(
        change(&["collection", "set", "notes", "--no-description"]),
        CollectionChange {
            description: Some(None),
            attributes: None,
        }
    );
    assert_eq!(
        change(&["collection", "set", "notes", "~/elsewhere"]),
        CollectionChange {
            description: None,
            attributes: Some(attributes("~/elsewhere")),
        }
    );
}

#[test]
fn collection_set_without_anything_to_change_is_an_error() {
    assert!(matches!(
        read(&["collection", "set", "notes"]),
        Err(CliError::NothingToChange)
    ));
}

#[test]
fn collection_set_takes_a_description_or_no_description_but_not_both() {
    assert!(
        usage(&[
            "collection",
            "set",
            "notes",
            "--description",
            "d",
            "--no-description"
        ])
        .contains("cannot be used with")
    );
}

#[test]
fn update_reads_one_collection_or_all() {
    assert!(matches!(
        operation(&["update", "notes", "--force"]),
        Operation::Update { collection, overwrite: true, optimize: true } if collection.0 == "notes"
    ));
    assert!(matches!(
        operation(&["update", "--all", "--no-optimize"]),
        Operation::UpdateAll {
            overwrite: false,
            optimize: false
        }
    ));
}

#[test]
fn update_needs_one_collection_or_all_but_not_both() {
    assert!(usage(&["update"]).contains("required"));
    assert!(usage(&["update", "notes", "--all"]).contains("cannot be used with"));
}

#[test]
fn put_reads_a_file_or_stdin() {
    assert!(matches!(
        operation(&["put", "notes", "a.md", "--file-type", "md", "a.md"]),
        Operation::Put { content: ContentSource::File(path), overwrite: false, optimize: true, .. }
            if path == std::path::Path::new("a.md")
    ));
    assert!(matches!(
        operation(&["put", "notes", "", "--file-type", "text/markdown", "-", "--force"]),
        Operation::Put { key, content: ContentSource::Stdin, overwrite: true, media_type, .. }
            if key == Key(String::new()) && media_type == "text/markdown"
    ));
}

#[test]
fn put_of_a_file_type_that_does_not_exist_is_an_error() {
    assert!(matches!(
        read(&["put", "notes", "a", "--file-type", "image/png", "a.png"]),
        Err(CliError::UnsupportedFileType { value }) if value == "image/png"
    ));
}

#[test]
fn remove_reads_the_document() {
    assert!(matches!(
        operation(&["remove", "notes", "a.md", "--yes"]),
        Operation::DeleteDocument { collection, key, yes: true, optimize: true }
            if collection.0 == "notes" && key.0 == "a.md"
    ));
}

#[test]
fn search_takes_its_defaults() {
    assert_eq!(
        format!("{:?}", operation(&["search", "release"])),
        format!(
            "{:?}",
            Operation::Search {
                query: "release".to_owned(),
                mode: SearchMode::Hybrid,
                collections: Vec::new(),
                limit: SearchLimit::new(20),
                unit: SearchUnit::Document,
                output: Output::Human,
            }
        )
    );
}

#[test]
fn search_reads_its_options() {
    assert_eq!(
        format!(
            "{:?}",
            operation(&[
                "search", "q", "--mode", "fts", "-c", "a,b", "-c", "c", "-n", "5", "--unit",
                "chunk", "--json"
            ])
        ),
        format!(
            "{:?}",
            Operation::Search {
                query: "q".to_owned(),
                mode: SearchMode::FullText,
                collections: ["a", "b", "c"]
                    .into_iter()
                    .map(|name| CollectionName(name.to_owned()))
                    .collect(),
                limit: SearchLimit::new(5),
                unit: SearchUnit::Chunk,
                output: Output::Json,
            }
        )
    );
    assert!(matches!(
        operation(&["search", "q", "--mode", "vector"]),
        Operation::Search {
            mode: SearchMode::Vector,
            ..
        }
    ));
}

#[test]
fn a_word_after_a_collection_is_not_read_as_another_collection() {
    assert_eq!(
        usage(&["search", "q", "-c", "notes", "work"]),
        "unexpected argument 'work' found"
    );
}

#[test]
fn search_checks_the_query_and_the_limit() {
    assert!(matches!(
        read(&["search", "  "]),
        Err(CliError::Query(QueryError::EmptyQuery))
    ));
    assert!(matches!(
        read(&["search", "q", "-n", "0"]),
        Err(CliError::Query(QueryError::ZeroLimit))
    ));
    assert!(matches!(
        operation(&["search", "q", "-n", "9007199254740991"]),
        Operation::Search { limit, .. } if limit == SearchLimit::new(SearchLimit::MAX)
    ));
}

#[test]
fn a_limit_that_is_not_a_whole_number_up_to_2_to_the_53_minus_1_is_a_usage_error() {
    for limit in [
        "9007199254740992",
        "18446744073709551616",
        "abc",
        "+5",
        "-1",
        "1.5",
        "",
    ] {
        assert_eq!(
            usage(&["search", "q", "-n", limit]),
            format!("the limit {limit} is not a whole number from 1 to 9007199254740991"),
            "{limit}"
        );
    }
}

#[test]
fn search_modes_and_units_other_than_its_own_are_usage_errors() {
    assert_eq!(
        usage(&["search", "q", "--mode", "semantic"]),
        "the search mode semantic is not one of fts, vector, and hybrid"
    );
    assert_eq!(
        usage(&["search", "q", "--unit", "page"]),
        "the unit page is not one of document and chunk"
    );
}

#[test]
fn get_reads_the_range() {
    let range = |args: &[&str]| match operation(args) {
        Operation::Get { range, .. } => range,
        other => panic!("{other:?}"),
    };

    assert_eq!(
        range(&["get", "notes", "a.md"]),
        ChunkRangeRequest {
            start: None,
            end: None
        }
    );
    assert_eq!(
        range(&[
            "get",
            "notes",
            "a.md",
            "--chunk-index",
            "2",
            "--end-chunk-index",
            "4"
        ]),
        ChunkRangeRequest {
            start: Some(2),
            end: Some(4)
        }
    );
    assert_eq!(
        range(&[
            "get",
            "notes",
            "a.md",
            "--chunk-index",
            "99999999999999999999999"
        ]),
        ChunkRangeRequest {
            start: Some(usize::MAX),
            end: None
        }
    );
    assert!(matches!(
        operation(&["get", "notes", "a.md", "--no-size-limit"]),
        Operation::Get {
            size_limit_off: true,
            ..
        }
    ));
}

#[test]
fn get_checks_the_range() {
    assert!(matches!(
        read(&["get", "notes", "a.md", "--end-chunk-index", "4"]),
        Err(CliError::Query(QueryError::InvalidChunkRange))
    ));
    assert!(matches!(
        read(&[
            "get",
            "notes",
            "a.md",
            "--chunk-index",
            "4",
            "--end-chunk-index",
            "3"
        ]),
        Err(CliError::Query(QueryError::InvalidChunkRange))
    ));
    assert_eq!(
        usage(&["get", "notes", "a.md", "--chunk-index", "-1"]),
        "the value -1 of --chunk-index is not a whole number of 0 or more"
    );
}

#[test]
fn config_set_and_reset_check_the_setting() {
    assert!(matches!(
        operation(&["config", "set", "resources.memory_limit_mb", "4096"]),
        Operation::SetSetting { name, value } if name == "resources.memory_limit_mb" && value == "4096"
    ));
    assert!(matches!(
        operation(&["config", "reset", "embedding.model"]),
        Operation::ResetSetting { name } if name == "embedding.model"
    ));
    assert!(matches!(
        read(&["config", "set", "no.such", "1"]),
        Err(CliError::Config(ConfigError::UnknownName { name })) if name == "no.such"
    ));
    assert!(matches!(
        read(&["config", "reset", "no.such"]),
        Err(CliError::Config(ConfigError::UnknownName { .. }))
    ));
}

#[test]
fn embedding_rebuild_and_optimize_are_read() {
    assert!(matches!(
        operation(&["embedding", "rebuild", "--force", "--no-optimize"]),
        Operation::RebuildEmbeddings {
            force: true,
            optimize: false
        }
    ));
    assert!(matches!(operation(&["optimize"]), Operation::Optimize));
}

fn mcp(args: &[&str]) -> McpTransport {
    match read(args) {
        Ok(Invocation {
            action: Action::Mcp(transport),
            ..
        }) => transport,
        other => panic!("not the MCP server: {other:?}"),
    }
}

fn http(host: &str, port: u16) -> McpTransport {
    McpTransport::Http {
        host: host.to_owned(),
        port,
    }
}

#[test]
fn mcp_is_over_stdio_unless_http_is_given() {
    assert_eq!(mcp(&["mcp"]), McpTransport::Stdio);
    assert_eq!(mcp(&["mcp", "--http"]), http("127.0.0.1", 8000));
}

#[test]
fn the_address_is_an_ip_address_or_a_host_name_and_a_port() {
    assert_eq!(
        mcp(&["mcp", "--http", "--address", "127.0.0.1:8000"]),
        http("127.0.0.1", 8000)
    );
    assert_eq!(
        mcp(&["mcp", "--http", "--address", "[::1]:8000"]),
        http("::1", 8000)
    );
    assert_eq!(
        mcp(&["mcp", "--http", "--address", "localhost:0"]),
        http("localhost", 0)
    );
}

#[test]
fn an_address_not_in_the_form_is_a_usage_error() {
    for address in [
        "localhost",
        ":8000",
        "::1:8000",
        "[localhost]:8000",
        "localhost:+80",
        "localhost:70000",
        "localhost: 80",
        "[::1:8000",
    ] {
        assert_eq!(
            usage(&["mcp", "--http", "--address", address]),
            format!(
                "the address {address} is not in the form <host>:<port>, with an IPv6 address in brackets"
            ),
            "{address}"
        );
    }
}

#[test]
fn an_address_without_http_is_a_usage_error() {
    assert_eq!(
        usage(&["mcp", "--address", "127.0.0.1:8000"]),
        "--address needs --http"
    );
}

#[test]
fn a_value_that_is_not_utf_8_is_a_usage_error() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let all = vec![
            OsString::from("mdya"),
            OsString::from("search"),
            OsString::from_vec(vec![0x66, 0xff]),
        ];
        assert!(matches!(
            read_args(all, &TestFormat::new()),
            Err(CliError::Usage { .. })
        ));
    }
}
