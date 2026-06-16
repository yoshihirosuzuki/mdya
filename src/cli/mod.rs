//! CLI surface. Subcommand dispatch lives here;
//! each subcommand's logic is delegated to a sibling module so this file
//! stays a thin coordinator.

mod collection;
mod dim;
mod get;
mod init;
mod log_writer;
mod search;
mod status;
mod stress;
mod tracing_init;
mod update_all;
// `pub` (unlike the sibling subcommand modules) so integration tests in
// `tests/` — a separate crate — can drive `vector::switch_model` with a
// stand-in `Embedder`, the same test seam `ingest::update_all_collections`
// provides.
pub mod vector;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config;
use crate::runtime;

pub use tracing_init::{LogFormat, TracingOptions, init as init_tracing};

#[derive(Debug, Parser)]
#[command(
    name = "mdya",
    version,
    about = "Local Markdown retrieval primitive (BM25 + on-device vector + MCP)."
)]
pub struct Cli {
    /// Override the mdya base directory. Default: `$HOME/.mdya/`.
    //
    // There is no `MDYA_CONFIG_DIR` env binding; explicit
    // `args` ("--config-dir <path>") are the only override path.
    #[arg(long = "config-dir", global = true)]
    pub config_dir: Option<PathBuf>,

    /// Override the embedding-model cache directory.
    /// Default: `$HOME/.mdya-models/`.
    //
    // No env binding (no `MDYA_MODEL_CACHE_DIR`).
    #[arg(long = "model-cache-dir", global = true)]
    pub model_cache_dir: Option<PathBuf>,

    /// Tracing level (trace/debug/info/warn/error).
    /// Fallback chain: this flag > `RUST_LOG` > `warn`. (There is no
    /// `MDYA_LOG` env binding; `RUST_LOG` keeps the same use case via
    /// `tracing-subscriber`'s standard sniff.)
    #[arg(long = "log-level", global = true)]
    pub log_level: Option<String>,

    /// Tracing output format. Default: `compact`.
    //
    // Combining `global = true` with `default_value_t` here means every
    // subcommand sees the same default; clap v4 does not let subcommands
    // override a `default_value_t` on a global flag. That is the desired
    // behavior for mdya (uniform log format), so we accept the constraint.
    #[arg(
        long = "log-format",
        value_enum,
        default_value_t = LogFormat::Compact,
        global = true
    )]
    pub log_format: LogFormat,

    /// Disable ANSI color in stderr. Falls back to `NO_COLOR` env, then to
    /// TTY auto-detection on stderr.
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize `~/.mdya/` (config.yml + index/ + models/) idempotently.
    Init,

    /// Manage collections (declared side; ingest is handled by `update-all`).
    #[command(subcommand)]
    Collection(CollectionCommand),

    /// Walk every registered collection, ingest new/changed `.md` files
    /// into LanceDB (chunk → embed → upsert), and clean up orphaned chunks.
    UpdateAll,

    /// Search markdown collections (3 modes: fts / vector / hybrid).
    #[command(subcommand)]
    Search(SearchCommand),

    /// Manage the vector/embedding subsystem (model switching).
    #[command(subcommand)]
    Vector(VectorCommand),

    /// Print a document's full original text (the faithful source) —
    /// or one chunk's body with `--chunk <N>` — to stdout.
    /// Reads from the DB, not the filesystem.
    Get {
        /// Collection name (must be declared in config.yml).
        collection: String,
        /// Document path, relative to the collection root.
        path: String,
        /// 0-indexed `chunk_sequence` to print one chunk's
        /// body instead of the full document. Pair with a `chunk_sequence`
        /// from `mdya search ... --chunks` to read the middle ground
        /// between a snippet and the whole file.
        #[arg(long = "chunk", value_name = "N")]
        chunk: Option<u32>,
        /// Bypass the `get.cli_max_bytes` output-size cap and print the full
        /// document regardless of size — for redirects / pipes where a large
        /// output is intended. Has no effect with `--chunk` (chunk reads are
        /// never size-checked).
        #[arg(short = 'f', long = "no-size-limit")]
        no_size_limit: bool,
    },

    /// Print the package version. Stable surface for MCP / scripted callers.
    Version,

    /// Report index status: version, embedding model + vector dim (from the
    /// chunks table's schema pin), collection count, and chunk / document
    /// row counts. Requires an initialized index.
    Status {
        /// Output format.
        #[arg(long = "format", value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },

    /// Start the MCP server. Defaults to stdio (for editors like Claude Code);
    /// `--http` runs a foreground Streamable HTTP daemon instead.
    Mcp {
        /// Serve over Streamable HTTP instead of stdio. Foreground process;
        /// background it with the shell (`&` / nohup / systemd). One daemon per
        /// config dir is enforced via an exclusive lock on `mcp.pid`.
        #[arg(long)]
        http: bool,

        /// HTTP bind address (`host:port`), only used with `--http`. Default is
        /// loopback; remote exposure is out of scope (rmcp also rejects
        /// non-loopback `Host` headers by default).
        #[arg(long, default_value = "127.0.0.1:8000")]
        addr: String,
    },

    /// Test-only: allocate RSS to trip the runtime memory guard.
    /// Hidden from `--help`; only the integration tests should reach it.
    #[command(hide = true, subcommand)]
    Stress(stress::StressCommand),
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    /// Add a directory as a collection.
    Add {
        /// Path to the collection root. Tilde (`~/...`) is expanded.
        path: PathBuf,

        /// Override the collection name. Default: basename of `<path>`.
        #[arg(long)]
        name: Option<String>,

        /// Human-readable description stored with the collection and shown
        /// by `mdya collection list`.
        #[arg(long)]
        description: Option<String>,
    },

    /// List the registered collections (name, path, description, document
    /// count). Reads `config.yml` and the `sources` table.
    List {
        /// Output format.
        #[arg(long = "format", value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
}

#[derive(Debug, Subcommand)]
pub enum SearchCommand {
    /// BM25 full-text search.
    Fts(SearchArgs),
    /// Cosine vector search.
    Vector(SearchArgs),
    /// RRF hybrid (BM25 + vector) search.
    Hybrid(SearchArgs),
}

#[derive(Debug, Subcommand)]
pub enum VectorCommand {
    /// Switch the embedding model. Destructive: drops the chunks index
    /// and re-embeds every collection from disk. Requires confirmation
    /// (interactive `[y/N]`, or `--yes` to skip).
    Use {
        /// Embedding model id (e.g. `cl-nagoya/ruri-v3-30m` or
        /// `ollama:<model>`).
        model: String,

        /// Skip the interactive confirmation prompt. Required on a
        /// non-interactive stdin (scripts / CI).
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Output format for search results, selected via
/// `--format <human|json|md|xml>`. `human` / `md` are
/// human-facing and round `score` to 3 decimals; `json` / `xml` are
/// machine-faithful (raw `f32` score), and `xml` is a lossless 1:1 mirror
/// of the JSON envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
    Md,
    Xml,
}

/// Shared positional + flags for every search mode.
/// `-c/--collections` accepts CSV (`-c notes,work`)
/// and repeats (`-c notes -c work`) — both forms OR together. Space-
/// separated values (`-c notes work`) are intentionally rejected to
/// avoid positional ambiguity with `query`.
#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// Search query string. Empty or whitespace-only is an error.
    pub query: String,

    /// Collection filter. CSV (`-c notes,work`) or repeat
    /// (`-c notes -c work`). 0 values = all collections.
    #[arg(
        short = 'c',
        long = "collections",
        visible_alias = "collection",
        value_delimiter = ',',
        action = clap::ArgAction::Append
    )]
    pub collections: Vec<String>,

    /// Top-N. Default 20.
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: u32,

    /// Return raw chunk-level hits instead of the default doc-level
    /// summary. Off by default: each `(collection, path)` is collapsed
    /// to one hit carrying the max chunk score and a `matched_chunks`
    /// count. Pass `--chunks` to keep the chunk-level pass-through,
    /// e.g. to locate a specific passage.
    #[arg(long = "chunks")]
    pub chunks: bool,

    /// Output format. `human` (default) is the colored human-readable
    /// view; `json` is the machine envelope; `md` / `xml` add scripting
    /// / agent-friendly renderings.
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Human)]
    pub format: OutputFormat,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        let config_dir = self.config_dir.clone();
        let model_cache_dir = self.model_cache_dir.clone();
        install_memory_guard(config_dir.as_deref());
        match self.command {
            Command::Init => {
                init::run(config_dir.as_deref()).await?;
            }
            Command::Collection(CollectionCommand::Add {
                path,
                name,
                description,
            }) => {
                collection::run(
                    config_dir.as_deref(),
                    &path,
                    name.as_deref(),
                    description.as_deref(),
                )?;
            }
            Command::Collection(CollectionCommand::List { format }) => {
                collection::list(config_dir.as_deref(), format).await?;
            }
            Command::UpdateAll => {
                update_all::run(config_dir.as_deref(), model_cache_dir.as_deref()).await?;
            }
            Command::Search(cmd) => {
                search::run(
                    config_dir.as_deref(),
                    model_cache_dir.as_deref(),
                    cmd,
                    self.no_color,
                )
                .await?;
            }
            Command::Vector(VectorCommand::Use { model, yes }) => {
                vector::run(
                    config_dir.as_deref(),
                    model_cache_dir.as_deref(),
                    &model,
                    yes,
                )
                .await?;
            }
            Command::Get {
                collection,
                path,
                chunk,
                no_size_limit,
            } => {
                get::run(
                    config_dir.as_deref(),
                    &collection,
                    &path,
                    chunk,
                    no_size_limit,
                )
                .await?;
            }
            Command::Version => {
                println!("mdya v{}", env!("CARGO_PKG_VERSION"));
            }
            Command::Status { format } => {
                status::run(config_dir.as_deref(), format).await?;
            }
            Command::Mcp { http, addr } => {
                let dir = config::resolve_config_dir(config_dir.as_deref())?;
                let cache_dir = config::resolve_model_cache_dir(model_cache_dir.as_deref())?;
                if http {
                    crate::mcp::serve_http(&dir, &cache_dir, &addr).await?;
                } else {
                    crate::mcp::serve_stdio(&dir, &cache_dir).await?;
                }
            }
            Command::Stress(cmd) => {
                stress::run(&cmd);
            }
        }
        Ok(())
    }
}

/// Resolve whether ANSI colour escapes are emitted. Precedence:
/// `--no-color` flag has the final say, then `NO_COLOR` env
/// (no-color.org Unix standard), then stderr TTY auto-detection.
pub fn color_enabled(no_color_flag: bool) -> bool {
    if no_color_flag || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}

/// Install the runtime memory guard before any subcommand work
/// touches RAM-heavy paths. Reads `runtime.memory_limit_mb` from `config.yml`
/// if it exists; otherwise applies [`config::DEFAULT_MEMORY_LIMIT_MB`]. A
/// resolved limit of `0` is the documented disable sentinel and produces a
/// no-op inside `runtime::install`.
///
/// Failure to read or parse the config is intentionally swallowed: the
/// subcommand below (e.g. `init`, `collection add`) will surface a richer
/// error against the same file. This avoids racing two error paths and
/// keeps the guard installation off the critical-path of `mdya init`,
/// which legitimately runs before `config.yml` exists.
fn install_memory_guard(config_dir_flag: Option<&Path>) {
    let limit_mb = read_memory_limit_mb(config_dir_flag);
    runtime::install(limit_mb);
}

fn read_memory_limit_mb(config_dir_flag: Option<&Path>) -> u64 {
    let Ok(base) = config::resolve_config_dir(config_dir_flag) else {
        return config::DEFAULT_MEMORY_LIMIT_MB;
    };
    let config_path = base.join("config.yml");
    match config::load(&config_path) {
        Ok(cfg) => cfg.runtime.memory_limit_mb,
        Err(_) => config::DEFAULT_MEMORY_LIMIT_MB,
    }
}
