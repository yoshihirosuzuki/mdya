//! MCP server exposing the search tools (`search_fts` / `search_vector`
//! / `search_hybrid`, which share one [`SearchEngine`] and one
//! [`request::SearchRequest`] shape with the mode carried by the tool
//! name), plus `get_document` and the `list_collections` / `get_status`
//! introspection tools.
//!
//! The embedder (`cl-nagoya/ruri-v3-30m`, ~140 MB) is loaded lazily on
//! the first `search_vector` / `search_hybrid` call via a shared
//! [`OnceCell`], so `mdya mcp` startup and `search_fts` stay
//! download-free. FTS needs no embedder at all.

mod error;
mod pid_lock;
mod request;

pub use error::{McpErrorCode, McpToolError};
pub use request::{GetDocumentRequest, SearchRequest};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rmcp::{
    Json, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use schemars::JsonSchema;
use serde::Serialize;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::config;
use crate::embedding::{EmbedError, Embedder, ModelCache, RURI_V3_30M_DIM, RuriV3_30m};
use crate::get::{get_chunk, get_document};
use crate::introspect::{self, CollectionListReport, StatusReport};
use crate::search::{SearchEngine, SearchError, SearchResponse};

/// Structured output for the `get_document` tool. Echoes the
/// `(collection, path)` locator alongside the faithful original text so
/// an MCP client can correlate the response without tracking request
/// state (mirrors how `SearchResponse` echoes the query / mode).
#[derive(Debug, Serialize, JsonSchema)]
pub struct GetDocumentResponse {
    pub collection: String,
    pub path: String,
    pub content: String,
}

/// Human-readable guidance returned in the MCP `initialize` handshake so
/// clients can pick the right tool without out-of-band docs.
const SERVER_INSTRUCTIONS: &str = "mdya is a local Markdown retrieval server. \
Use `search_fts` for exact keyword or phrase lookups (BM25), `search_vector` for \
semantic meaning-based search, and `search_hybrid` to combine both (BM25 + vector, \
fused with RRF). Every search tool takes `query`, an optional `k` (top-N, default 20), \
an optional `collections` filter (empty = all collections), and an optional `level` \
(`\"doc\"` default, or `\"chunk\"`). With `level: \"doc\"` each hit is one document \
(one per `(collection, path)`) carrying the max chunk score and a `matched_chunks` \
count; with `level: \"chunk\"` you get the raw chunk-level passages including \
`chunk_sequence`. Use `get_document` with a hit's `collection` and `path` to fetch the \
document's full original text, or add `chunk` (a `chunk_sequence` from a \
`level: \"chunk\"` hit) to fetch one chunk's body — the middle ground between the \
short snippet and the full document. Use `list_collections` to see the available collections \
(name, path, description, document count) and `get_status` for index health (server \
version, embedding model, vector dimension, and row counts).";

#[derive(Clone)]
pub struct Server {
    engine: Arc<SearchEngine>,
    /// The mdya base dir. `get_document` reads the `sources` table under
    /// `<config_dir>/index`. Held as a path rather than built handles so
    /// the test constructor ([`Server::with_seeded_embedder`]) need not
    /// touch the filesystem until a tool actually needs it.
    config_dir: PathBuf,
    /// The embedding-model cache dir, resolved by
    /// `cli::Cli::run` via `config::resolve_model_cache_dir` and passed
    /// in here. The lazy embedder reads its weights from this path on
    /// first vector/hybrid call; the test constructor never touches it
    /// because the embedder slot is pre-seeded.
    model_cache_dir: PathBuf,
    /// Lazily loaded embedder, shared across calls and clones so the
    /// ~140 MB weights load at most once per process.
    embedder: Arc<OnceCell<Arc<dyn Embedder>>>,
    // rmcp's #[tool_handler] macro accesses this field through generated
    // code rustc cannot see, so dead_code analysis flags it without the allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<Server>,
}

#[tool_router]
impl Server {
    /// Open the [`SearchEngine`] against `config_dir` (which validates
    /// the schema-metadata pin) and prepare an empty embedder slot.
    /// The embedder is not loaded here — see the module doc.
    ///
    /// `model_cache_dir` is held verbatim and only consulted
    /// when [`Server::get_or_load_embedder`] runs (first
    /// `search_vector` / `search_hybrid`).
    pub async fn new(config_dir: &Path, model_cache_dir: &Path) -> Result<Self> {
        let cfg = config::load(&config_dir.join("config.yml"))?;
        let dim = i32::try_from(RURI_V3_30M_DIM).expect("RURI_V3_30M_DIM (256) fits in i32");
        let engine = SearchEngine::open(config_dir, &cfg.embedding.model, dim).await?;
        Ok(Self {
            engine: Arc::new(engine),
            config_dir: config_dir.to_path_buf(),
            model_cache_dir: model_cache_dir.to_path_buf(),
            embedder: Arc::new(OnceCell::new()),
            tool_router: Self::tool_router(),
        })
    }

    /// Test-support constructor: build a server around an already-open
    /// engine and a pre-seeded embedder so library smoke tests exercise
    /// the tools without downloading the real model. `config_dir` must
    /// point at the ingested corpus so `get_document` can open its
    /// `sources` table; the embedder `OnceCell` is already filled so the
    /// model cache dir is never touched. `model_cache_dir` is taken for
    /// symmetry with [`Server::new`] (the struct invariant always holds a
    /// path), but tests can pass any value because the lazy-load path
    /// will not run.
    #[doc(hidden)]
    pub fn with_seeded_embedder(
        config_dir: &Path,
        model_cache_dir: &Path,
        engine: Arc<SearchEngine>,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        Self {
            engine,
            config_dir: config_dir.to_path_buf(),
            model_cache_dir: model_cache_dir.to_path_buf(),
            embedder: Arc::new(OnceCell::new_with(Some(embedder))),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "BM25 (lindera/ipadic) full-text search over Markdown collections. \
                       Returns doc-level hits by default (one per `(collection, path)` \
                       with the max chunk score and a `matched_chunks` count); pass \
                       `level: \"chunk\"` for raw chunk-level passages."
    )]
    pub async fn search_fts(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, Json<McpToolError>> {
        self.run_fts(req)
            .await
            .map(Json)
            .map_err(|e| Json(McpToolError::from(e)))
    }

    #[tool(
        description = "Vector (cosine) semantic search over Markdown collections. \
                       Returns doc-level hits by default (one per `(collection, path)` \
                       with the max chunk score and a `matched_chunks` count); pass \
                       `level: \"chunk\"` for raw chunk-level passages."
    )]
    pub async fn search_vector(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, Json<McpToolError>> {
        self.run_vector(req)
            .await
            .map(Json)
            .map_err(|e| Json(McpToolError::from(e)))
    }

    #[tool(
        description = "Hybrid (BM25 + vector, RRF) search over Markdown collections. \
                       Returns doc-level hits by default (one per `(collection, path)` \
                       with the max chunk score and a `matched_chunks` count); pass \
                       `level: \"chunk\"` for raw chunk-level passages."
    )]
    pub async fn search_hybrid(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<Json<SearchResponse>, Json<McpToolError>> {
        self.run_hybrid(req)
            .await
            .map(Json)
            .map_err(|e| Json(McpToolError::from(e)))
    }

    #[tool(
        description = "Fetch one Markdown document's full original text by its collection and \
                       path (e.g. from a search hit) — returns the faithful source, not a \
                       snippet. Pass `chunk` (a `chunk_sequence` from a `level: \"chunk\"` \
                       search hit) to fetch just one chunk's body instead: the middle ground \
                       between the snippet and the full document."
    )]
    pub async fn get_document(
        &self,
        Parameters(req): Parameters<GetDocumentRequest>,
    ) -> Result<Json<GetDocumentResponse>, Json<McpToolError>> {
        let GetDocumentRequest {
            collection,
            path,
            chunk,
        } = req;
        let content = match chunk {
            Some(seq) => get_chunk(&self.config_dir, &collection, &path, seq).await,
            None => get_document(&self.config_dir, &collection, &path).await,
        }
        .map_err(|e| Json(McpToolError::from(e)))?;
        Ok(Json(GetDocumentResponse {
            collection,
            path,
            content,
        }))
    }

    #[tool(
        description = "List the registered collections, each with its path, description, and \
                       document count. Use the returned names to scope a search's `collections`."
    )]
    pub async fn list_collections(&self) -> Result<Json<CollectionListReport>, Json<McpToolError>> {
        introspect::collection_list(Some(&self.config_dir))
            .await
            .map(Json)
            .map_err(|e| Json(McpToolError::from(e)))
    }

    #[tool(
        description = "Report index status: server version, embedding model, vector dimension, \
                       and the collection / chunk / document counts."
    )]
    pub async fn get_status(&self) -> Result<Json<StatusReport>, Json<McpToolError>> {
        introspect::status(Some(&self.config_dir))
            .await
            .map(Json)
            .map_err(|e| Json(McpToolError::from(e)))
    }

    /// FTS validates internally (no embedder load), mirroring
    /// `cli::search::run_fts`.
    async fn run_fts(&self, req: SearchRequest) -> Result<SearchResponse, SearchError> {
        self.engine.fts(&req.into_engine_request()).await
    }

    /// Reject bad input before loading the embedder so a typo never
    /// triggers the ~140 MB download, mirroring `cli::search::run_vector`.
    async fn run_vector(&self, req: SearchRequest) -> Result<SearchResponse, SearchError> {
        let req = req.into_engine_request();
        self.engine.validate_request(&req)?;
        let embedder = self.get_or_load_embedder().await?;
        self.engine.vector(&req, embedder.as_ref()).await
    }

    /// Same validate-before-load ordering as [`Server::run_vector`].
    async fn run_hybrid(&self, req: SearchRequest) -> Result<SearchResponse, SearchError> {
        let req = req.into_engine_request();
        self.engine.validate_request(&req)?;
        let embedder = self.get_or_load_embedder().await?;
        self.engine.hybrid(&req, embedder.as_ref()).await
    }

    /// Load the embedder on first use and cache it for the process
    /// lifetime. Concurrent first calls race into one init via
    /// [`OnceCell::get_or_try_init`]; the loser awaits the winner.
    async fn get_or_load_embedder(&self) -> Result<Arc<dyn Embedder>, SearchError> {
        let embedder = self
            .embedder
            .get_or_try_init(|| async {
                // ModelCacheError → EmbedError::Cache (named, not `from`, so
                // the conversion target is visible here) → SearchError via `?`.
                let cache = ModelCache::new(&self.model_cache_dir).map_err(EmbedError::Cache)?;
                let model = RuriV3_30m::new(&cache).await?;
                Ok::<Arc<dyn Embedder>, SearchError>(Arc::new(model) as Arc<dyn Embedder>)
            })
            .await?;
        Ok(embedder.clone())
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(SERVER_INSTRUCTIONS)
    }
}

pub async fn serve_stdio(config_dir: &Path, model_cache_dir: &Path) -> Result<()> {
    let service = Server::new(config_dir, model_cache_dir)
        .await?
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Serve the same tools over rmcp's Streamable HTTP transport,
/// mounted at `/mcp` on `addr`. Runs in the foreground until it receives a stop
/// signal (Ctrl-C, or SIGTERM from `kill` on Unix); one daemon per config dir
/// is enforced by an exclusive lock on `mcp.pid`.
///
/// Single-instance is taken *first* (fail fast on a second daemon), the model
/// is built once and cloned per session, and the pid is written only after the
/// listener binds so the file never names a daemon that is not accepting yet.
pub async fn serve_http(config_dir: &Path, model_cache_dir: &Path, addr: &str) -> Result<()> {
    let pid_path = config_dir.join("mcp.pid");
    let mut pid_file = pid_lock::PidFile::open(pid_path.clone())?;
    let mut pid_guard = pid_file.lock_exclusive()?;

    let server = Server::new(config_dir, model_cache_dir).await?;
    let shutdown = CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(shutdown.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    pid_lock::write_current_pid(&mut pid_guard, &pid_path)?;
    tracing::info!("MCP HTTP daemon listening on http://{local}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            await_stop_signal().await;
            shutdown.cancel();
        })
        .await?;
    Ok(())
}

/// Resolve when the OS asks the daemon to stop: SIGINT (Ctrl-C) on every
/// platform, plus SIGTERM on Unix — the default signal from `kill` and the
/// stop path for a backgrounded daemon, so it must also trigger the graceful
/// shutdown rather than terminate the process abruptly. Windows has no
/// SIGTERM (its service-stop path is out of scope).
async fn await_stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        // If SIGTERM can't be registered, degrade to Ctrl-C only rather than
        // panicking a long-lived daemon during shutdown setup.
        let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
