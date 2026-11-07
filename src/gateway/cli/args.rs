//! Reads the command line arguments with clap, and checks what the arguments
//! alone can tell.
//!
//! clap reads every value as text, and the values are checked after clap has
//! read them: a value that clap checked itself (a number's range, one of a
//! few words) would be reported before a `--help` later on the command line
//! is seen, and `--help` is to show the help whatever the other arguments are.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::collection::{self, CollectionChange};
use crate::format::Format;
use crate::query::{self, ChunkRangeRequest, SearchMode, SearchRequest, SearchUnit};
use crate::types::{CollectionName, CollectionType, Key, TypeAttributes};
use crate::{archive_ingest, config, file_ingest, raw_ingest};

use super::super::operations::SearchLimit;
use super::{
    Action, CliError, CliHelp, CliOperation, ContentSource, GlobalOptions, Invocation, LogFormat,
    McpTransport, Operation, Output,
};

#[cfg(test)]
mod tests;

/// The address the MCP server (HTTP) listens on when `--address` is not
/// given.
const DEFAULT_ADDRESS: &str = "127.0.0.1:8000";

/// The search limit when `--limit` is not given.
const DEFAULT_LIMIT: u64 = 20;

pub(super) fn read_args(args: Vec<OsString>, format: &dyn Format) -> Result<Invocation, CliError> {
    if asks_for_help(&args) {
        return help_for_commands(&args);
    }
    let matches = Cli::command()
        .try_get_matches_from(&args)
        .map_err(|error| usage_error(&error, &args))?;
    let cli = Cli::from_arg_matches(&matches).map_err(|error| usage_error(&error, &args))?;
    let options = global_options(&cli)?;
    let action = if cli.version {
        Action::Version
    } else {
        match cli.command {
            None => return Err(usage("no command was given")),
            Some(command) => action(command, format)?,
        }
    };
    Ok(Invocation { options, action })
}

/// The help of the commands that the arguments name, once only the commands
/// are checked: a word where a command is expected that is not one of the
/// commands there is a usage error, and nothing else is looked at.
fn help_for_commands(args: &[OsString]) -> Result<Invocation, CliError> {
    let commands =
        command_path(args).map_err(|word| usage(&format!("unrecognized subcommand '{word}'")))?;
    let mut path: Vec<OsString> = vec!["mdya".into()];
    path.extend(commands.into_iter().map(OsString::from));
    path.push("--help".into());
    match Cli::command().try_get_matches_from(&path) {
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            Ok(help(error.render().to_string()))
        }
        Err(error) => Err(usage_error(&error, args)),
        Ok(_) => Err(usage("the help could not be found")),
    }
}

/// The commands that the arguments name, such as `collection` and `add`, read
/// from the definition of the commands rather than by clap, so that what is
/// wrong with the other arguments does not hide them. Before `--`, the
/// options are passed over, with the value after one that takes a value; each
/// other word, until a command without commands of its own, is to be one of
/// the commands there. The error is the first word that is not.
fn command_path(args: &[OsString]) -> Result<Vec<String>, String> {
    let root = Cli::command();
    let mut current = &root;
    let mut path = Vec::new();
    let mut words = args.iter().skip(1).map(|arg| arg.to_string_lossy());
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if let Some(name) = word.strip_prefix("--") {
            if !name.contains('=') && takes_value(current, |arg| arg.get_long() == Some(name)) {
                words.next();
            }
            continue;
        }
        if let Some(shorts) = word.strip_prefix('-')
            && !shorts.is_empty()
        {
            let mut letters = shorts.chars();
            if let (Some(letter), None) = (letters.next(), letters.next())
                && takes_value(current, |arg| arg.get_short() == Some(letter))
            {
                words.next();
            }
            continue;
        }
        if current.get_subcommands().next().is_none() {
            break;
        }
        match current.find_subcommand(word.as_ref()) {
            Some(command) => {
                path.push(command.get_name().to_owned());
                current = command;
            }
            None => return Err(word.into_owned()),
        }
    }
    Ok(path)
}

/// Whether the option of `command` that `is_it` picks takes a value.
fn takes_value(command: &clap::Command, is_it: impl Fn(&clap::Arg) -> bool) -> bool {
    command
        .get_arguments()
        .any(|arg| is_it(arg) && arg.get_action().takes_values())
}

fn help(text: String) -> Invocation {
    Invocation {
        options: GlobalOptions::default(),
        action: Action::Help(CliHelp(text)),
    }
}

/// Whether `--help` or `-h` is among the arguments before `--`.
fn asks_for_help(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(is_help_flag)
}

fn is_help_flag(arg: &OsString) -> bool {
    arg == "--help" || arg == "-h"
}

/// What clap found wrong, as a usage error. A command that needs one of its
/// commands is told so, without the help that clap would show.
fn usage_error(error: &clap::Error, args: &[OsString]) -> CliError {
    if error.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        let commands = command_path(args).unwrap_or_default().join(" ");
        return usage(&format!("mdya {commands} needs a command"));
    }
    let rendered = error.render().to_string();
    let first = rendered.lines().next().unwrap_or_default();
    usage(first.strip_prefix("error: ").unwrap_or(first))
}

fn usage(detail: &str) -> CliError {
    CliError::Usage {
        detail: detail.to_owned(),
    }
}

fn global_options(cli: &Cli) -> Result<GlobalOptions, CliError> {
    let log_format = match cli.log_format.as_deref() {
        None | Some("compact") => LogFormat::Compact,
        Some("pretty") => LogFormat::Pretty,
        Some("json") => LogFormat::Json,
        Some(other) => {
            return Err(usage(&format!(
                "the log format {other} is not one of compact, pretty, and json"
            )));
        }
    };
    Ok(GlobalOptions {
        log_level: cli.log_level.clone(),
        log_format,
        data_dir: cli.data_dir.clone(),
        model_cache_dir: cli.model_cache_dir.clone(),
    })
}

fn action(command: Command, format: &dyn Format) -> Result<Action, CliError> {
    let operation = match command {
        Command::Mcp(args) => return mcp(args).map(Action::Mcp),
        Command::Collection(CollectionCommand::Add(args)) => create_collection(args)?,
        Command::Collection(CollectionCommand::Remove(args)) => Operation::DeleteCollection {
            name: CollectionName(args.name),
            yes: args.yes,
            optimize: !args.no_optimize,
        },
        Command::Collection(CollectionCommand::List(args)) => Operation::ListCollections {
            output: output(args.json),
        },
        Command::Collection(CollectionCommand::Set(args)) => change_collection(args)?,
        Command::Update(args) => update(args),
        Command::Put(args) => put(args, format)?,
        Command::Remove(args) => Operation::DeleteDocument {
            collection: CollectionName(args.collection),
            key: Key(args.key),
            yes: args.yes,
            optimize: !args.no_optimize,
        },
        Command::Search(args) => search(args)?,
        Command::Get(args) => get(args)?,
        Command::Status(args) => Operation::Status {
            output: output(args.json),
        },
        Command::Config(ConfigCommand::List(args)) => Operation::ListSettings {
            output: output(args.json),
        },
        Command::Config(ConfigCommand::Set(args)) => {
            config::check_setting(&args.name, Some(&args.value)).map_err(CliError::Config)?;
            Operation::SetSetting {
                name: args.name,
                value: args.value,
            }
        }
        Command::Config(ConfigCommand::Reset(args)) => {
            config::check_setting(&args.name, None).map_err(CliError::Config)?;
            Operation::ResetSetting { name: args.name }
        }
        Command::Embedding(EmbeddingCommand::Rebuild(args)) => Operation::RebuildEmbeddings {
            force: args.force,
            optimize: !args.no_optimize,
        },
        Command::Optimize => Operation::Optimize,
    };
    Ok(Action::Operation(CliOperation(operation)))
}

fn output(json: bool) -> Output {
    if json { Output::Json } else { Output::Human }
}

fn create_collection(args: AddArgs) -> Result<Operation, CliError> {
    let name = CollectionName(args.name);
    collection::check_collection_name(&name).map_err(|source| CliError::Collection {
        name: name.clone(),
        source,
    })?;
    let collection_type =
        CollectionType(args.collection_type.unwrap_or_else(|| "directory".into()));
    let attributes = location_attributes(args.location);
    check_attribute_names(&collection_type, &attributes)?;
    Ok(Operation::CreateCollection {
        name,
        collection_type,
        description: args.description,
        attributes,
    })
}

/// The type-specific attributes that a location makes: `path`, or none.
fn location_attributes(location: Option<String>) -> TypeAttributes {
    let mut attributes = serde_json::Map::new();
    if let Some(location) = location {
        attributes.insert("path".to_owned(), serde_json::Value::String(location));
    }
    TypeAttributes(attributes)
}

/// Checks the names of the type-specific attributes against the collection
/// type. A type that mdya does not know is a usage error.
fn check_attribute_names(
    collection_type: &CollectionType,
    attributes: &TypeAttributes,
) -> Result<(), CliError> {
    let checked = match collection_type.0.as_str() {
        "directory" => file_ingest::check_attribute_names(attributes),
        "archive" => archive_ingest::check_attribute_names(attributes),
        "raw" => raw_ingest::check_attribute_names(attributes),
        other => {
            return Err(usage(&format!(
                "the collection type {other} is not one of directory, archive, and raw"
            )));
        }
    };
    checked.map_err(|source| CliError::Ingest {
        collection_type: collection_type.clone(),
        source,
    })
}

fn change_collection(args: SetArgs) -> Result<Operation, CliError> {
    let description = match (args.description, args.no_description) {
        (Some(description), _) => Some(Some(description)),
        (None, true) => Some(None),
        (None, false) => None,
    };
    let attributes = args
        .location
        .map(|location| location_attributes(Some(location)));
    if description.is_none() && attributes.is_none() {
        return Err(CliError::NothingToChange);
    }
    Ok(Operation::ChangeCollection {
        name: CollectionName(args.name),
        change: CollectionChange {
            description,
            attributes,
        },
    })
}

fn update(args: UpdateArgs) -> Operation {
    match args.collection {
        Some(collection) => Operation::Update {
            collection: CollectionName(collection),
            overwrite: args.force,
            optimize: !args.no_optimize,
        },
        None => Operation::UpdateAll {
            overwrite: args.force,
            optimize: !args.no_optimize,
        },
    }
}

fn put(args: PutArgs, format: &dyn Format) -> Result<Operation, CliError> {
    if format
        .file_type_from_mime_or_alias(&args.file_type)
        .is_none()
    {
        return Err(CliError::UnsupportedFileType {
            value: args.file_type,
        });
    }
    let content = if args.location == "-" {
        ContentSource::Stdin
    } else {
        ContentSource::File(PathBuf::from(args.location))
    };
    Ok(Operation::Put {
        collection: CollectionName(args.collection),
        key: Key(args.key),
        media_type: args.file_type,
        content,
        overwrite: args.force,
        optimize: !args.no_optimize,
    })
}

fn search(args: SearchArgs) -> Result<Operation, CliError> {
    let mode = match args.mode.as_deref() {
        None | Some("hybrid") => SearchMode::Hybrid,
        Some("fts") => SearchMode::FullText,
        Some("vector") => SearchMode::Vector,
        Some(other) => {
            return Err(usage(&format!(
                "the search mode {other} is not one of fts, vector, and hybrid"
            )));
        }
    };
    let unit = match args.unit.as_deref() {
        None | Some("document") => SearchUnit::Document,
        Some("chunk") => SearchUnit::Chunk,
        Some(other) => {
            return Err(usage(&format!(
                "the unit {other} is not one of document and chunk"
            )));
        }
    };
    let limit = match args.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(limit) => whole_number(limit)
            .filter(|limit| *limit <= SearchLimit::MAX)
            .ok_or_else(|| {
                usage(&format!(
                    "the limit {limit} is not a whole number from 1 to {}",
                    SearchLimit::MAX
                ))
            })?,
    };
    let collections: Vec<CollectionName> =
        args.collections.into_iter().map(CollectionName).collect();
    let request = SearchRequest {
        query: args.query,
        mode,
        collections,
        limit: usize::try_from(limit).unwrap_or(usize::MAX),
        unit,
    };
    query::check_search_request(&request).map_err(CliError::Query)?;
    Ok(Operation::Search {
        query: request.query,
        mode,
        collections: request.collections,
        // A limit of 0 is reported above.
        limit: SearchLimit::new(limit),
        unit,
        output: output(args.json),
    })
}

fn get(args: GetArgs) -> Result<Operation, CliError> {
    let start = chunk_index(args.chunk_index.as_deref(), "--chunk-index")?;
    let end = chunk_index(args.end_chunk_index.as_deref(), "--end-chunk-index")?;
    let range = ChunkRangeRequest { start, end };
    query::check_chunk_range(&range).map_err(CliError::Query)?;
    Ok(Operation::Get {
        collection: CollectionName(args.collection),
        key: Key(args.key),
        range,
        size_limit_off: args.no_size_limit,
    })
}

/// A chunk index: a whole number of 0 or more. One larger than a `usize`
/// holds is read as `usize::MAX`, which is past the last chunk all the same.
fn chunk_index(value: Option<&str>, option: &str) -> Result<Option<usize>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !is_whole_number(value) {
        return Err(usage(&format!(
            "the value {value} of {option} is not a whole number of 0 or more"
        )));
    }
    Ok(Some(
        value
            .parse::<u64>()
            .ok()
            .and_then(|index| usize::try_from(index).ok())
            .unwrap_or(usize::MAX),
    ))
}

/// `text` as a whole number, when it is only ASCII digits and fits in a
/// `u64`.
fn whole_number(text: &str) -> Option<u64> {
    if !is_whole_number(text) {
        return None;
    }
    text.parse().ok()
}

fn is_whole_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn mcp(args: McpArgs) -> Result<McpTransport, CliError> {
    if !args.http {
        if args.address.is_some() {
            return Err(usage("--address needs --http"));
        }
        return Ok(McpTransport::Stdio);
    }
    let address = args.address.as_deref().unwrap_or(DEFAULT_ADDRESS);
    let (host, port) = host_and_port(address).ok_or_else(|| {
        usage(&format!(
            "the address {address} is not in the form <host>:<port>, with an IPv6 address in brackets"
        ))
    })?;
    Ok(McpTransport::Http { host, port })
}

/// Splits `<host>:<port>`. The host is an IPv4 address, an IPv6 address in
/// brackets (returned without them), or a host name without `:`, `[`, or
/// `]`. The port is ASCII digits only, from 0 to 65535.
fn host_and_port(address: &str) -> Option<(String, u16)> {
    let (host, port) = address.rsplit_once(':')?;
    if !is_whole_number(port) {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if let Some(bracketed) = host.strip_prefix('[') {
        let ipv6 = bracketed.strip_suffix(']')?;
        ipv6.parse::<std::net::Ipv6Addr>().ok()?;
        return Some((ipv6.to_owned(), port));
    }
    if host.is_empty() || host.contains([':', '[', ']']) {
        return None;
    }
    Some((host.to_owned(), port))
}

// The command is optional for clap, so that `--version` alone can be read;
// the help's usage line says that a command is needed, as it is for anything
// else. The help flag is declared here, so that `--version` comes after it.
#[derive(Parser, Debug)]
#[command(
    name = "mdya",
    about = "Search your own documents on this computer, from the command line or over MCP",
    override_usage = "mdya [OPTIONS] <COMMAND>",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    #[arg(
        long,
        value_name = "DIR",
        help = "The settings and index directory (default: ~/.mdya)"
    )]
    data_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "DIR",
        help = "The embedding model cache directory (default: ~/.mdya-models)"
    )]
    model_cache_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "LEVEL",
        help = "Write logs from this level: error, warn, info, debug, or trace, or per target such as mdya=debug [default: warn]"
    )]
    log_level: Option<String>,
    #[arg(
        long,
        value_name = "FORMAT",
        help = "How to write logs [default: compact] [possible values: compact, pretty, json]"
    )]
    log_format: Option<String>,
    // Read by `entry` from the arguments themselves, so that an error in
    // reading them is not colored either.
    #[arg(long, help = "Do not color the output")]
    no_color: bool,
    // Every command takes it, as clap's own help flag, which is turned off
    // above, would be.
    #[arg(short = 'h', long = "help", global = true, action = clap::ArgAction::Help, help = "Print help")]
    help: Option<bool>,
    #[arg(long, help = "Print the version")]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(subcommand, about = "Add, list, change, and remove collections")]
    Collection(CollectionCommand),
    #[command(about = "Bring collections up to date with their sources")]
    Update(UpdateArgs),
    #[command(about = "Ingest one document, from a file or stdin")]
    Put(PutArgs),
    #[command(about = "Remove a document from a collection")]
    Remove(RemoveArgs),
    #[command(about = "Search the documents")]
    Search(SearchArgs),
    #[command(about = "Get the text of a document, or of a range of its chunks")]
    Get(GetArgs),
    #[command(about = "Show the state of the index")]
    Status(JsonArgs),
    #[command(subcommand, about = "Show and change the settings")]
    Config(ConfigCommand),
    #[command(subcommand, about = "Rebuild the embeddings")]
    Embedding(EmbeddingCommand),
    #[command(about = "Maintain the index")]
    Optimize,
    #[command(about = "Start the MCP server")]
    Mcp(McpArgs),
}

#[derive(Subcommand, Debug)]
enum CollectionCommand {
    #[command(about = "Create a collection")]
    Add(AddArgs),
    #[command(about = "Remove a collection and its documents from the index")]
    Remove(RemoveCollectionArgs),
    #[command(about = "List the collections")]
    List(JsonArgs),
    #[command(about = "Change a collection's description or location")]
    Set(SetArgs),
}

#[derive(Args, Debug)]
struct AddArgs {
    #[arg(value_name = "NAME", help = "The name of the collection")]
    name: String,
    #[arg(
        long = "type",
        value_name = "TYPE",
        help = "The collection type [default: directory] [possible values: directory, archive, raw]"
    )]
    collection_type: Option<String>,
    #[arg(long, value_name = "TEXT", help = "What the collection is about")]
    description: Option<String>,
    #[arg(
        value_name = "LOCATION",
        help = "The directory, or the archive file, that the documents are in (none for raw)"
    )]
    location: Option<String>,
}

#[derive(Args, Debug)]
struct RemoveCollectionArgs {
    #[arg(value_name = "NAME", help = "The name of the collection")]
    name: String,
    #[arg(long, help = "Remove it without asking for confirmation")]
    yes: bool,
    #[arg(long, help = "Do not maintain the index at the end")]
    no_optimize: bool,
}

#[derive(Args, Debug)]
struct SetArgs {
    #[arg(value_name = "NAME", help = "The name of the collection")]
    name: String,
    #[arg(
        long,
        value_name = "TEXT",
        conflicts_with = "no_description",
        help = "A new description"
    )]
    description: Option<String>,
    #[arg(long, help = "Remove the description")]
    no_description: bool,
    #[arg(
        value_name = "LOCATION",
        help = "A new directory, or archive file, that the documents are in"
    )]
    location: Option<String>,
}

#[derive(Args, Debug)]
struct UpdateArgs {
    #[arg(
        value_name = "COLLECTION",
        required_unless_present = "all",
        conflicts_with = "all",
        help = "The collection to update"
    )]
    collection: Option<String>,
    #[arg(long, help = "Update all the collections")]
    all: bool,
    #[arg(
        long,
        help = "Ingest the documents again even if they have not changed"
    )]
    force: bool,
    #[arg(long, help = "Do not maintain the index at the end")]
    no_optimize: bool,
}

#[derive(Args, Debug)]
struct PutArgs {
    #[arg(
        value_name = "COLLECTION",
        help = "The collection to put the document in"
    )]
    collection: String,
    #[arg(value_name = "KEY", help = "The key of the document")]
    key: String,
    #[arg(
        long,
        value_name = "TYPE",
        help = "The file type: a MIME type, such as text/markdown or application/pdf, or md, markdown, or pdf"
    )]
    file_type: String,
    #[arg(long, help = "Ingest the document again even if it has not changed")]
    force: bool,
    #[arg(long, help = "Do not maintain the index at the end")]
    no_optimize: bool,
    #[arg(
        value_name = "LOCATION",
        help = "The file to read the document from, or - to read it from stdin"
    )]
    location: String,
}

#[derive(Args, Debug)]
struct RemoveArgs {
    #[arg(value_name = "COLLECTION", help = "The collection the document is in")]
    collection: String,
    #[arg(value_name = "KEY", help = "The key of the document")]
    key: String,
    #[arg(long, help = "Remove it without asking for confirmation")]
    yes: bool,
    #[arg(long, help = "Do not maintain the index at the end")]
    no_optimize: bool,
}

#[derive(Args, Debug)]
struct SearchArgs {
    #[arg(value_name = "QUERY", help = "What to search for")]
    query: String,
    #[arg(
        long,
        value_name = "MODE",
        help = "How to search [default: hybrid] [possible values: fts, vector, hybrid]"
    )]
    mode: Option<String>,
    #[arg(
        short = 'c',
        long = "collection",
        value_name = "COLLECTION",
        value_delimiter = ',',
        num_args = 1,
        help = "A collection to search; separate several with commas, or give it more than once (default: all)"
    )]
    collections: Vec<String>,
    // A value such as -1 is read as the value, so that it is reported as a
    // limit that is not valid.
    #[arg(
        short = 'n',
        long,
        value_name = "LIMIT",
        allow_hyphen_values = true,
        help = "The most hits to return, from 1 to 9007199254740991 [default: 20]"
    )]
    limit: Option<String>,
    #[arg(
        long,
        value_name = "UNIT",
        help = "Return each matching document once, or each matching chunk [default: document] [possible values: document, chunk]"
    )]
    unit: Option<String>,
    #[arg(long, help = "Write the result as JSON")]
    json: bool,
}

#[derive(Args, Debug)]
struct GetArgs {
    #[arg(value_name = "COLLECTION", help = "The collection the document is in")]
    collection: String,
    #[arg(value_name = "KEY", help = "The key of the document")]
    key: String,
    #[arg(
        long,
        value_name = "INDEX",
        allow_hyphen_values = true,
        help = "The first chunk to get, such as a search hit's chunk index (default: the whole text)"
    )]
    chunk_index: Option<String>,
    #[arg(
        long,
        value_name = "INDEX",
        allow_hyphen_values = true,
        help = "The last chunk to get, not before --chunk-index, which it needs (default: only the chunk at --chunk-index)"
    )]
    end_chunk_index: Option<String>,
    #[arg(long, help = "Do not apply the output size limit")]
    no_size_limit: bool,
}

#[derive(Args, Debug)]
struct JsonArgs {
    #[arg(long, help = "Write the result as JSON")]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    #[command(about = "Show the settings")]
    List(JsonArgs),
    #[command(about = "Change a setting")]
    Set(SetSettingArgs),
    #[command(about = "Put a setting back to its default")]
    Reset(ResetSettingArgs),
}

#[derive(Args, Debug)]
struct SetSettingArgs {
    #[arg(
        value_name = "NAME",
        help = "The name of the setting, as mdya config list shows it"
    )]
    name: String,
    #[arg(value_name = "VALUE", help = "The new value")]
    value: String,
}

#[derive(Args, Debug)]
struct ResetSettingArgs {
    #[arg(
        value_name = "NAME",
        help = "The name of the setting, as mdya config list shows it"
    )]
    name: String,
}

#[derive(Subcommand, Debug)]
enum EmbeddingCommand {
    #[command(about = "Rebuild the embeddings with the configured embedding model")]
    Rebuild(RebuildArgs),
}

#[derive(Args, Debug)]
struct RebuildArgs {
    #[arg(
        long,
        help = "Rebuild them even if they were made with the configured embedding model"
    )]
    force: bool,
    #[arg(long, help = "Do not maintain the index at the end")]
    no_optimize: bool,
}

#[derive(Args, Debug)]
struct McpArgs {
    #[arg(long, help = "Serve over HTTP instead of stdio")]
    http: bool,
    #[arg(
        long,
        value_name = "ADDRESS",
        help = "The address to listen on over HTTP, as <host>:<port>, with an IPv6 address in brackets [default: 127.0.0.1:8000]"
    )]
    address: Option<String>,
}
