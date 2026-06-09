//! `SearchEngine` — read-only query path over the `chunks` table. The
//! three modes (FTS / Vector / Hybrid) share validation, collection
//! filter, batch parsing, and tie-break sort, varying only in the score
//! column they read: `_score`, `_distance` (cosine→similarity), and
//! `_relevance_score`.
//!
//! Doc-level is the default hit granularity: each mode over-fetches
//! chunks then folds them by `(collection, path)`. The fold lives here
//! rather than in LanceDB because LanceDB exposes no query-level dedup
//! builtin.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, UInt32Type};
use arrow_array::{Array, RecordBatch, StringArray, UInt32Array};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lance_index::scalar::inverted::SCORE_COL;
use lance_index::vector::DIST_COL;
use lancedb::DistanceType;
use lancedb::Table;
use lancedb::expr::{col, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select, VectorQuery};
use tracing::{debug, info};

/// LanceDB hybrid reranker output column. `lancedb-0.29.0/src/rerankers.rs:19`
/// declares this as a private `const`, so mdya re-declares the literal
/// string to read it back from the hybrid `execute_hybrid` output. The
/// value is part of LanceDB's public RecordBatch contract even though the
/// constant is not re-exported.
const RELEVANCE_SCORE_COL: &str = "_relevance_score";

/// Doc-level over-fetch multiplier. The
/// LanceDB query asks for `limit * DOC_LEVEL_OVERFETCH_K` chunk rows so
/// that `(collection, path)` dedup can still return roughly `limit`
/// unique documents in the common case. With `limit = 20` this requests
/// 200 chunks per mode — well within the LanceDB index scan budget for
/// MVP-sized corpora. If dedup yields fewer than `limit` documents the
/// response simply returns fewer rows; we do not iteratively re-fetch
/// because LanceDB lacks `.offset()` and re-issuing a query would re-rank
/// and double-count rows from the first call.
const DOC_LEVEL_OVERFETCH_K: usize = 10;

use crate::config;
use crate::embedding::Embedder;
use crate::store::metadata_check::{self, Outcome as MetadataOutcome};
use crate::store::{CHUNKS_TABLE_NAME, COL_BODY, COL_CHUNK_SEQUENCE, COL_COLLECTION, COL_PATH};

use super::error::SearchError;
use super::request::SearchRequest;
use super::response::{SearchHit, SearchLevel, SearchMode, SearchResponse};
use super::snippet::{DEFAULT_SNIPPET_CHARS, extract_snippet, extract_snippet_head};

pub struct SearchEngine {
    table: Table,
    /// Names declared in `config.yml::collections`. Used to reject
    /// `-c <typo>` early instead of silently returning zero hits.
    known_collections: Vec<String>,
}

impl SearchEngine {
    /// Open the engine: read config to learn which collections are
    /// declared, open the `chunks` LanceDB table, and verify its
    /// `Schema::metadata` matches the declared `embedding.model` +
    /// `vector_dim`. Missing pins abort with
    /// `SearchError::SchemaMetadataMissing`; a value mismatch logs a
    /// `tracing::warn!` and lets the search continue so the user can
    /// still inspect what is in the DB. The check runs once per
    /// `SearchEngine` instance; schema metadata is immutable within
    /// a dataset version.
    pub async fn open(
        config_dir: &Path,
        declared_embedding_model: &str,
        declared_vector_dim: i32,
    ) -> Result<Self, SearchError> {
        let cfg = config::load(&config_dir.join("config.yml"))?;
        let known_collections = cfg.collections.keys().cloned().collect();
        let table = open_chunks_table(config_dir).await?;
        warn_or_abort_on_metadata_mismatch(
            metadata_check::check(&table, declared_embedding_model, declared_vector_dim)
                .await
                .map_err(SearchError::Query)?,
            declared_embedding_model,
            declared_vector_dim,
        )?;
        Ok(Self {
            table,
            known_collections,
        })
    }

    /// Cosine-similarity vector search. Embeds the query off the async
    /// scheduler via `tokio::task::block_in_place` (the binary uses a
    /// multi-thread runtime, see `src/main.rs`), then issues
    /// `nearest_to(...).distance_type(Cosine)` against the IVF_Flat
    /// index. The schema-metadata pin check that used to live here was
    /// hoisted into `SearchEngine::open` so vector / hybrid callers
    /// share one check site and the response can be a plain
    /// `SearchResponse`.
    pub async fn vector(
        &self,
        req: &SearchRequest,
        embedder: &dyn Embedder,
    ) -> Result<SearchResponse, SearchError> {
        self.validate(req)?;
        info!(
            target: "mdya::search",
            query = %req.query,
            collections = ?req.collections,
            limit = req.limit,
            level = req.level.as_str(),
            mode = "vector",
            "search start",
        );
        let qv = embed_query_blocking(embedder, &req.query)?;
        let stream = self
            .build_vector_query(req, qv)?
            .execute()
            .await
            .map_err(SearchError::Query)?;
        let batches = stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(SearchError::Query)?;
        let chunks = batches_to_chunks(&batches, &req.query, ScoreSource::CosineDistance);
        Ok(finalize_response(req, SearchMode::Vector, chunks))
    }

    fn build_vector_query(
        &self,
        req: &SearchRequest,
        qv: Vec<f32>,
    ) -> Result<VectorQuery, SearchError> {
        let mut q = self
            .table
            .query()
            .nearest_to(qv)
            .map_err(SearchError::Query)?
            .distance_type(DistanceType::Cosine)
            .limit(query_chunk_limit(req))
            .select(Select::Columns(vec![
                COL_COLLECTION.to_string(),
                COL_PATH.to_string(),
                COL_CHUNK_SEQUENCE.to_string(),
                COL_BODY.to_string(),
                DIST_COL.to_string(),
            ]));
        if !req.collections.is_empty() {
            let candidates: Vec<_> = req.collections.iter().map(|c| lit(c.as_str())).collect();
            q = q.only_if_expr(col(COL_COLLECTION).in_list(candidates, false));
        }
        Ok(q)
    }

    /// RRF hybrid via LanceDB's `execute_hybrid` auto-dispatch. Builds
    /// a `VectorQuery` with both `full_text_search` and `nearest_to`
    /// set, then calls `.execute()` — the
    /// `VectorQuery::execute_with_options` impl at
    /// `lancedb-0.29.0/src/query.rs:1304-1316` routes that to
    /// `execute_hybrid`, which runs FTS + vector in parallel via
    /// `try_join!`, applies `hybrid::normalize_scores`, and reranks with
    /// `RRFReranker::default()` (k=60). The result column
    /// `_relevance_score` is the RRF reciprocal-rank sum, with raw range
    /// `[0, 2/(k+1)] ≈ [0, 0.033]` — mdya passes it through to
    /// `SearchHit.score` without further normalisation.
    pub async fn hybrid(
        &self,
        req: &SearchRequest,
        embedder: &dyn Embedder,
    ) -> Result<SearchResponse, SearchError> {
        self.validate(req)?;
        info!(
            target: "mdya::search",
            query = %req.query,
            collections = ?req.collections,
            limit = req.limit,
            level = req.level.as_str(),
            mode = "hybrid",
            "search start",
        );
        let qv = embed_query_blocking(embedder, &req.query)?;
        let stream = self
            .build_hybrid_query(req, qv)?
            .execute()
            .await
            .map_err(SearchError::Query)?;
        let batches = stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(SearchError::Query)?;
        let chunks = batches_to_chunks(&batches, &req.query, ScoreSource::RelevanceScore);
        Ok(finalize_response(req, SearchMode::Hybrid, chunks))
    }

    fn build_hybrid_query(
        &self,
        req: &SearchRequest,
        qv: Vec<f32>,
    ) -> Result<VectorQuery, SearchError> {
        // Important: `_relevance_score` cannot be listed in the
        // `select(...)` projection. `execute_hybrid` applies the
        // projection per sub-query (FTS plan + vector plan, see
        // `lancedb-0.29.0/src/query.rs:1196-1206`) before
        // `RRFReranker::rerank_hybrid` attaches `_relevance_score`,
        // so naming it here produces `Schema error: No field named
        // _relevance_score` from `lance-datafusion::projection`. The
        // column is added downstream by the reranker and we read it
        // off the final RecordBatch via `RELEVANCE_SCORE_COL`.
        let mut q = self
            .table
            .query()
            .full_text_search(FullTextSearchQuery::new(req.query.clone()))
            .nearest_to(qv)
            .map_err(SearchError::Query)?
            .distance_type(DistanceType::Cosine)
            .limit(query_chunk_limit(req))
            .select(Select::Columns(vec![
                COL_COLLECTION.to_string(),
                COL_PATH.to_string(),
                COL_CHUNK_SEQUENCE.to_string(),
                COL_BODY.to_string(),
            ]));
        if !req.collections.is_empty() {
            let candidates: Vec<_> = req.collections.iter().map(|c| lit(c.as_str())).collect();
            q = q.only_if_expr(col(COL_COLLECTION).in_list(candidates, false));
        }
        Ok(q)
    }

    /// BM25 full-text search over the `body` FTS index. Heading text lives
    /// in `body`, so heading words match here too.
    pub async fn fts(&self, req: &SearchRequest) -> Result<SearchResponse, SearchError> {
        self.validate(req)?;
        info!(
            target: "mdya::search",
            query = %req.query,
            collections = ?req.collections,
            limit = req.limit,
            level = req.level.as_str(),
            mode = "fts",
            "search start",
        );
        let stream = self
            .build_fts_query(req)
            .execute()
            .await
            .map_err(SearchError::Query)?;
        let batches = stream
            .try_collect::<Vec<_>>()
            .await
            .map_err(SearchError::Query)?;
        let chunks = batches_to_chunks(&batches, &req.query, ScoreSource::FtsScore);
        Ok(finalize_response(req, SearchMode::Fts, chunks))
    }

    /// Public re-exposure of [`Self::validate`] so callers can reject a
    /// bad request before paying the cost of loading an embedder. Used
    /// by `cli/search.rs::run_vector` to avoid downloading the
    /// `cl-nagoya/ruri-v3-30m` weights on a `-n 0` typo.
    pub fn validate_request(&self, req: &SearchRequest) -> Result<(), SearchError> {
        self.validate(req)
    }

    fn validate(&self, req: &SearchRequest) -> Result<(), SearchError> {
        if req.query.trim().is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        if req.limit == 0 {
            return Err(SearchError::InvalidLimit);
        }
        for name in &req.collections {
            if !self.known_collections.iter().any(|known| known == name) {
                return Err(SearchError::UnknownCollection { name: name.clone() });
            }
        }
        Ok(())
    }

    fn build_fts_query(&self, req: &SearchRequest) -> lancedb::query::Query {
        let mut q = self
            .table
            .query()
            .full_text_search(FullTextSearchQuery::new(req.query.clone()))
            .limit(query_chunk_limit(req))
            .select(Select::Columns(vec![
                COL_COLLECTION.to_string(),
                COL_PATH.to_string(),
                COL_CHUNK_SEQUENCE.to_string(),
                COL_BODY.to_string(),
                SCORE_COL.to_string(),
            ]));
        if !req.collections.is_empty() {
            let candidates: Vec<_> = req.collections.iter().map(|c| lit(c.as_str())).collect();
            q = q.only_if_expr(col(COL_COLLECTION).in_list(candidates, false));
        }
        q
    }
}

/// Decide what to do with a `metadata_check::Outcome` on the search
/// side: `Mismatch` logs `warn` and lets search continue, `Missing`
/// aborts. The ingest side calls the parallel `enforce_schema_metadata`
/// in `writer.rs` which aborts on either.
fn warn_or_abort_on_metadata_mismatch(
    outcome: MetadataOutcome,
    declared_model: &str,
    declared_dim: i32,
) -> Result<(), SearchError> {
    match outcome {
        MetadataOutcome::Pass => Ok(()),
        MetadataOutcome::Mismatch {
            actual_embedding_model,
            actual_vector_dim,
        } => {
            tracing::warn!(
                target: "mdya::search",
                declared_model = declared_model,
                declared_dim = declared_dim,
                actual_embedding_model = %actual_embedding_model,
                actual_vector_dim = %actual_vector_dim,
                "schema metadata pin mismatch: declared values disagree with the chunks table; \
                 search will continue but results may reflect a stale embedding model"
            );
            Ok(())
        }
        MetadataOutcome::Missing { absent_keys } => {
            Err(SearchError::SchemaMetadataMissing { absent_keys })
        }
    }
}

async fn open_chunks_table(config_dir: &Path) -> Result<Table, SearchError> {
    let index_dir = config_dir.join("index");
    let index_str = index_dir
        .to_str()
        .ok_or_else(|| SearchError::LancedbPathNotUtf8 {
            path: index_dir.clone(),
        })?;
    let db = lancedb::connect(index_str)
        .execute()
        .await
        .map_err(|source| SearchError::LancedbConnect {
            path: index_dir.clone(),
            source,
        })?;
    db.open_table(CHUNKS_TABLE_NAME)
        .execute()
        .await
        .map_err(SearchError::OpenChunksTable)
}

#[derive(Clone, Copy)]
enum ScoreSource {
    /// Lance FTS already emits BM25 in the `_score` column, "higher
    /// = better"; passed through unchanged.
    FtsScore,
    /// Lance vector search emits cosine distance in `_distance`,
    /// "lower = better", range `[0, 2]`. Convert to similarity via
    /// `(1 - distance).max(0.0)` so the result lives in `[0, 1]` and
    /// shares the FTS sort direction.
    CosineDistance,
    /// LanceDB `execute_hybrid` reranker emits the RRF reciprocal-rank
    /// sum in `_relevance_score`, "higher = better", raw range
    /// `[0, 2/(k+1)] ≈ [0, 0.033]` for `RRFReranker::default()`
    /// (k=60). Passed through unchanged — clients read the score scale
    /// from the `mode` envelope field.
    RelevanceScore,
}

/// Internal chunk-level hit. Always built first regardless of
/// `SearchLevel`; `SearchLevel::Doc` then folds these into [`DocHit`]
/// via [`dedup_chunks_to_docs`].
#[derive(Debug, Clone, PartialEq)]
struct ChunkHit {
    collection: String,
    path: String,
    chunk_sequence: u32,
    score: f32,
    snippet: String,
}

impl ChunkHit {
    fn into_search_hit(self) -> SearchHit {
        SearchHit::Chunk {
            collection: self.collection,
            path: self.path,
            chunk_sequence: self.chunk_sequence,
            score: self.score,
            snippet: self.snippet,
        }
    }
}

/// Internal doc-level hit produced by [`dedup_chunks_to_docs`]: one row
/// per `(collection, path)` carrying the max chunk score, the breadth
/// signal (`matched_chunks`), and the top-scoring chunk's snippet.
#[derive(Debug, Clone, PartialEq)]
struct DocHit {
    collection: String,
    path: String,
    score: f32,
    snippet: String,
    matched_chunks: u32,
}

impl DocHit {
    fn into_search_hit(self) -> SearchHit {
        SearchHit::Doc {
            collection: self.collection,
            path: self.path,
            score: self.score,
            snippet: self.snippet,
            matched_chunks: self.matched_chunks,
        }
    }
}

/// LanceDB row budget for one mode's underlying query. Doc-level
/// over-fetches by [`DOC_LEVEL_OVERFETCH_K`] so the post-query dedup
/// has enough material to return roughly `limit` unique documents;
/// chunk-level stays at `limit` since no dedup is applied.
fn query_chunk_limit(req: &SearchRequest) -> usize {
    let base = req.limit as usize;
    match req.level {
        SearchLevel::Doc => base.saturating_mul(DOC_LEVEL_OVERFETCH_K),
        SearchLevel::Chunk => base,
    }
}

/// Wrap a `Vec<ChunkHit>` into a `SearchResponse` honouring
/// `req.level`: doc-level folds + truncates + emits `SearchHit::Doc`,
/// chunk-level just sorts + emits `SearchHit::Chunk`. `total` reflects
/// the unit returned (doc-count or chunk-count).
fn finalize_response(
    req: &SearchRequest,
    mode: SearchMode,
    chunks: Vec<ChunkHit>,
) -> SearchResponse {
    let (hits, total) = match req.level {
        SearchLevel::Doc => {
            let mut docs = dedup_chunks_to_docs(chunks);
            sort_doc_hits_stable(&mut docs);
            docs.truncate(req.limit as usize);
            let total = docs.len() as u32;
            let hits: Vec<SearchHit> = docs.into_iter().map(DocHit::into_search_hit).collect();
            (hits, total)
        }
        SearchLevel::Chunk => {
            let mut chunks = chunks;
            sort_chunk_hits_stable(&mut chunks);
            // Symmetric with the doc-level branch above: LanceDB's
            // `.limit()` is contractually a top-N cap today, but
            // truncating here protects the wire `limit` invariant if
            // that ever loosens (and the cost is a no-op when LanceDB
            // already returned `<= req.limit` rows).
            chunks.truncate(req.limit as usize);
            let total = chunks.len() as u32;
            let hits: Vec<SearchHit> = chunks.into_iter().map(ChunkHit::into_search_hit).collect();
            (hits, total)
        }
    };
    info!(
        target: "mdya::search",
        mode = mode.as_str(),
        level = req.level.as_str(),
        total = total,
        returned = hits.len(),
        "search done",
    );
    SearchResponse {
        query: req.query.clone(),
        mode,
        level: req.level,
        collections: req.collections.clone(),
        limit: req.limit,
        total,
        hits,
    }
}

/// Fold chunk-level hits into doc-level hits by `(collection, path)`.
/// For each document we keep the max chunk score,
/// the snippet from that max-score chunk, and a `matched_chunks` count
/// — the breadth signal that lets consumers tell "1 chunk hit hard"
/// apart from "many chunks hit moderately" without inspecting the
/// chunk-level pass-through (which they can still request via
/// `SearchLevel::Chunk`).
fn dedup_chunks_to_docs(chunks: Vec<ChunkHit>) -> Vec<DocHit> {
    let mut aggregators: HashMap<(String, String), DocHit> = HashMap::new();
    for chunk in chunks {
        let key = (chunk.collection.clone(), chunk.path.clone());
        aggregators
            .entry(key)
            .and_modify(|agg| {
                agg.matched_chunks += 1;
                // Strict `>` keeps the *first*-seen max chunk's snippet on
                // ties — important so the output is reproducible against a
                // given chunk batch ordering. NaN scores can't beat the
                // current max (`NaN > x` is false), matching the defensive
                // NaN handling in `sort_doc_hits_stable`.
                if chunk.score > agg.score {
                    agg.score = chunk.score;
                    agg.snippet = chunk.snippet.clone();
                }
            })
            .or_insert(DocHit {
                collection: chunk.collection,
                path: chunk.path,
                score: chunk.score,
                snippet: chunk.snippet,
                matched_chunks: 1,
            });
    }
    aggregators.into_values().collect()
}

fn batches_to_chunks(batches: &[RecordBatch], query: &str, source: ScoreSource) -> Vec<ChunkHit> {
    let mut chunks = Vec::new();
    for batch in batches {
        append_chunks_from_batch(batch, query, source, &mut chunks);
    }
    chunks
}

fn append_chunks_from_batch(
    batch: &RecordBatch,
    query: &str,
    source: ScoreSource,
    chunks: &mut Vec<ChunkHit>,
) {
    let columns = match batch_columns(batch, source) {
        Some(cols) => cols,
        None => return,
    };
    for row in 0..batch.num_rows() {
        if !columns.row_is_valid(row) {
            tracing::warn!(target: "mdya::search", row, "non-nullable column carried null, skipping");
            continue;
        }
        let body = columns.body.value(row);
        let snippet = build_snippet(body, query, source);
        debug!(target: "mdya::search", row, snippet_len = snippet.len(), "snippet built");
        let raw = columns.score.value(row);
        let score = match source {
            ScoreSource::FtsScore => raw,
            ScoreSource::CosineDistance => cosine_distance_to_score(raw),
            ScoreSource::RelevanceScore => raw,
        };
        chunks.push(ChunkHit {
            collection: columns.collection.value(row).to_string(),
            path: columns.path.value(row).to_string(),
            chunk_sequence: columns.chunk_sequence.value(row),
            score,
            snippet,
        });
    }
}

/// Pick the per-`ScoreSource` snippet strategy. FTS keeps the query-
/// centred window so the matched token stays visible in the rendered
/// snippet; vector / hybrid (semantic queries that rarely appear
/// verbatim in `body`) take the body head, which doubles as a display
/// label for sub-chunk 0 because the heading text is folded into
/// `body`'s first line.
fn build_snippet(body: &str, query: &str, source: ScoreSource) -> String {
    match source {
        ScoreSource::FtsScore => extract_snippet(body, query, DEFAULT_SNIPPET_CHARS),
        ScoreSource::CosineDistance | ScoreSource::RelevanceScore => {
            extract_snippet_head(body, DEFAULT_SNIPPET_CHARS)
        }
    }
}

/// Clamp negative cosine similarity (= opposite-direction hits) to
/// zero so `SearchHit.score` lives in `[0, 1]` and reads as "0 % to
/// 100 % similar".
///
/// NaN distance (which a healthy IVF_Flat index never emits, but a
/// corrupted one might) resolves to `0.0`: `1.0 - NaN = NaN`, and
/// `NaN.max(0.0)` returns `0.0` because `f32::max` returns the
/// non-NaN argument when exactly one input is NaN. The NaN does not
/// propagate into `sort_*_hits_stable`, which both have their own NaN
/// defensive path.
fn cosine_distance_to_score(distance: f32) -> f32 {
    (1.0 - distance).max(0.0)
}

struct BatchColumns<'a> {
    collection: &'a StringArray,
    path: &'a StringArray,
    chunk_sequence: &'a UInt32Array,
    body: &'a StringArray,
    score: &'a arrow_array::PrimitiveArray<Float32Type>,
}

impl BatchColumns<'_> {
    fn row_is_valid(&self, row: usize) -> bool {
        self.collection.is_valid(row)
            && self.path.is_valid(row)
            && self.chunk_sequence.is_valid(row)
            && self.body.is_valid(row)
            && self.score.is_valid(row)
    }
}

fn batch_columns<'a>(batch: &'a RecordBatch, source: ScoreSource) -> Option<BatchColumns<'a>> {
    let score_column = match source {
        ScoreSource::FtsScore => SCORE_COL,
        ScoreSource::CosineDistance => DIST_COL,
        ScoreSource::RelevanceScore => RELEVANCE_SCORE_COL,
    };
    Some(BatchColumns {
        collection: batch.column_by_name(COL_COLLECTION)?.as_string(),
        path: batch.column_by_name(COL_PATH)?.as_string(),
        chunk_sequence: batch
            .column_by_name(COL_CHUNK_SEQUENCE)?
            .as_primitive::<UInt32Type>(),
        body: batch.column_by_name(COL_BODY)?.as_string(),
        score: batch
            .column_by_name(score_column)?
            .as_primitive::<Float32Type>(),
    })
}

/// Run the embedder on `query` off the async scheduler so the
/// synchronous candle forward pass cannot stall sibling tasks. The
/// binary uses `new_multi_thread().enable_all()` (see `src/main.rs`),
/// so `block_in_place` can hand the current worker off to the pool
/// while the embed work runs in place — keeping the borrow of
/// `&dyn Embedder` valid across the blocking section without needing
/// `Send + 'static` or an `Arc` wrapper.
fn embed_query_blocking(embedder: &dyn Embedder, query: &str) -> Result<Vec<f32>, SearchError> {
    let vectors = tokio::task::block_in_place(|| embedder.embed_queries(&[query]))?;
    Ok(vectors.into_iter().next().expect(
        "Embedder::embed_queries promises one vector per input text \
         (see src/embedding/mod.rs trait contract)",
    ))
}

/// Re-sort chunk-level hits with a stable, fully-specified key so test
/// results are reproducible regardless of LanceDB's internal row
/// ordering. LanceDB guarantees "order of BM25 scores" but not the DESC
/// direction nor tie-break order; this re-sort makes both explicit.
/// NaN scores collapse to `Equal` so the tie-break fields decide a
/// deterministic order — `unwrap()` on `partial_cmp` would panic, which
/// is the wrong failure mode for a corrupted score column.
fn sort_chunk_hits_stable(hits: &mut [ChunkHit]) {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.chunk_sequence.cmp(&b.chunk_sequence))
    });
}

/// Doc-level counterpart to [`sort_chunk_hits_stable`]. There is no
/// `chunk_sequence` tie-break at the doc level because each
/// `(collection, path)` appears at most once after
/// [`dedup_chunks_to_docs`], so the score / collection / path key is
/// already total.
fn sort_doc_hits_stable(hits: &mut [DocHit]) {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.collection.cmp(&b.collection))
            .then_with(|| a.path.cmp(&b.path))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(collection: &str, path: &str, chunk_sequence: u32, score: f32) -> ChunkHit {
        ChunkHit {
            collection: collection.to_string(),
            path: path.to_string(),
            chunk_sequence,
            score,
            snippet: format!("snippet for {collection}/{path}#{chunk_sequence}"),
        }
    }

    #[test]
    fn cosine_distance_zero_maps_to_full_similarity() {
        assert_eq!(cosine_distance_to_score(0.0), 1.0);
    }

    #[test]
    fn cosine_distance_one_maps_to_zero_similarity() {
        assert_eq!(cosine_distance_to_score(1.0), 0.0);
    }

    #[test]
    fn cosine_distance_two_clamps_to_zero_not_negative_one() {
        // distance 2 = opposite direction = similarity -1, but the
        // CLI / MCP `score` contract is `[0, 1]` so we clamp.
        assert_eq!(cosine_distance_to_score(2.0), 0.0);
    }

    #[test]
    fn cosine_distance_nan_resolves_to_zero_not_propagated() {
        // `f32::max(NaN, 0.0)` returns 0.0 (non-NaN wins), so a NaN
        // from a corrupted index scores as 0.0 instead of poisoning
        // sort comparisons downstream.
        assert_eq!(cosine_distance_to_score(f32::NAN), 0.0);
    }

    #[test]
    fn cosine_distance_partial_maps_to_partial_similarity() {
        let s = cosine_distance_to_score(0.188);
        assert!((s - 0.812).abs() < 1e-6, "got {s}");
    }

    #[test]
    fn sort_chunk_hits_stable_orders_by_score_then_collection_path_sequence() {
        let mut hits = vec![
            chunk("z", "a.md", 1, 0.5),
            chunk("a", "b.md", 0, 0.8),
            chunk("a", "a.md", 2, 0.8),
            chunk("a", "a.md", 1, 0.8),
            chunk("a", "a.md", 0, 0.3),
        ];
        sort_chunk_hits_stable(&mut hits);
        let order: Vec<_> = hits
            .iter()
            .map(|h| (h.collection.as_str(), h.path.as_str(), h.chunk_sequence))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a", "a.md", 1),
                ("a", "a.md", 2),
                ("a", "b.md", 0),
                ("z", "a.md", 1),
                ("a", "a.md", 0),
            ]
        );
    }

    #[test]
    fn sort_doc_hits_stable_orders_by_score_then_collection_path() {
        let mut docs = vec![
            DocHit {
                collection: "z".into(),
                path: "a.md".into(),
                score: 0.5,
                snippet: "s".into(),
                matched_chunks: 1,
            },
            DocHit {
                collection: "a".into(),
                path: "b.md".into(),
                score: 0.8,
                snippet: "s".into(),
                matched_chunks: 1,
            },
            DocHit {
                collection: "a".into(),
                path: "a.md".into(),
                score: 0.8,
                snippet: "s".into(),
                matched_chunks: 2,
            },
        ];
        sort_doc_hits_stable(&mut docs);
        let order: Vec<_> = docs
            .iter()
            .map(|d| (d.collection.as_str(), d.path.as_str()))
            .collect();
        assert_eq!(order, vec![("a", "a.md"), ("a", "b.md"), ("z", "a.md"),]);
    }

    #[test]
    fn dedup_collapses_chunks_of_one_doc_into_single_doc_with_max_score() {
        let chunks = vec![
            chunk("notes", "a.md", 0, 0.4),
            chunk("notes", "a.md", 1, 0.9),
            chunk("notes", "a.md", 2, 0.6),
        ];
        let docs = dedup_chunks_to_docs(chunks);
        assert_eq!(docs.len(), 1);
        let doc = &docs[0];
        assert_eq!(doc.collection, "notes");
        assert_eq!(doc.path, "a.md");
        assert!((doc.score - 0.9).abs() < 1e-6);
        assert_eq!(doc.matched_chunks, 3);
        // The top-scoring chunk (chunk_sequence=1) wins the snippet,
        // not the first-seen chunk (chunk_sequence=0) or the last
        // (chunk_sequence=2). This is the visible contract the CLI /
        // MCP rendered snippet relies on.
        assert_eq!(doc.snippet, "snippet for notes/a.md#1");
    }

    #[test]
    fn dedup_keeps_separate_docs_when_path_differs() {
        let chunks = vec![
            chunk("notes", "a.md", 0, 0.5),
            chunk("notes", "b.md", 0, 0.7),
        ];
        let docs = dedup_chunks_to_docs(chunks);
        assert_eq!(docs.len(), 2);
        let mut paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a.md", "b.md"]);
    }

    #[test]
    fn dedup_keeps_separate_docs_when_collection_differs_even_if_path_matches() {
        // `(collection, path)` is the dedup key — same path under two
        // collections must stay as two distinct documents.
        let chunks = vec![
            chunk("notes", "shared.md", 0, 0.5),
            chunk("work", "shared.md", 0, 0.5),
        ];
        let docs = dedup_chunks_to_docs(chunks);
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn dedup_on_empty_input_returns_empty_vec_not_panic() {
        let docs = dedup_chunks_to_docs(Vec::new());
        assert!(docs.is_empty());
    }

    #[test]
    fn dedup_strict_greater_keeps_first_seen_snippet_on_tied_scores() {
        // Two chunks tie at score 0.5. The first-seen chunk's snippet
        // should stay (because `>` is strict) so that the result is
        // reproducible against the chunk-batch ordering returned by
        // LanceDB.
        let chunks = vec![
            chunk("notes", "a.md", 0, 0.5),
            chunk("notes", "a.md", 1, 0.5),
        ];
        let docs = dedup_chunks_to_docs(chunks);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].snippet, "snippet for notes/a.md#0");
        assert_eq!(docs[0].matched_chunks, 2);
    }

    /// `MATCH` sits at byte 80, past the head window of `extract_snippet_head`,
    /// so an FTS-routed snippet must surface it (query-centred window) while
    /// a vector / hybrid-routed snippet must NOT (body head only).
    fn dispatch_body() -> String {
        format!("{} MATCH {}", "x".repeat(80), "y".repeat(20))
    }

    #[test]
    fn build_snippet_routes_fts_through_query_centered_window() {
        let body = dispatch_body();
        let snippet = build_snippet(&body, "MATCH", ScoreSource::FtsScore);
        assert!(
            snippet.contains("MATCH"),
            "FTS snippet must surface the query token: {snippet:?}"
        );
    }

    #[test]
    fn build_snippet_routes_vector_through_body_head() {
        let body = dispatch_body();
        let snippet = build_snippet(&body, "MATCH", ScoreSource::CosineDistance);
        assert!(
            !snippet.contains("MATCH"),
            "vector snippet must ignore the query and stay at the head: {snippet:?}"
        );
        assert!(snippet.starts_with('x'));
    }

    #[test]
    fn build_snippet_routes_hybrid_through_body_head() {
        let body = dispatch_body();
        let snippet = build_snippet(&body, "MATCH", ScoreSource::RelevanceScore);
        assert!(
            !snippet.contains("MATCH"),
            "hybrid snippet must ignore the query and stay at the head: {snippet:?}"
        );
        assert!(snippet.starts_with('x'));
    }

    #[test]
    fn query_chunk_limit_overfetches_for_doc_level_by_factor_k() {
        let req = SearchRequest {
            query: "q".into(),
            collections: vec![],
            limit: 20,
            level: SearchLevel::Doc,
        };
        assert_eq!(query_chunk_limit(&req), 20 * DOC_LEVEL_OVERFETCH_K);
    }

    #[test]
    fn query_chunk_limit_passes_through_limit_for_chunk_level() {
        let req = SearchRequest {
            query: "q".into(),
            collections: vec![],
            limit: 20,
            level: SearchLevel::Chunk,
        };
        assert_eq!(query_chunk_limit(&req), 20);
    }
}
