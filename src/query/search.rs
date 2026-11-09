//! Searching the index: which rows match, in what order, and which chunk each
//! hit shows.

use std::collections::{HashMap, HashSet};
use std::future::Future;

use futures::TryStreamExt;
use lancedb::arrow::SendableRecordBatchStream;
use lancedb::arrow::arrow_array::RecordBatch;
use lancedb::arrow::arrow_schema::{DataType, Field};
use lancedb::expr::{DfExpr, col, lit};
use lancedb::index::scalar::{FtsQuery, FullTextSearchQuery, MatchQuery};
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions, Select, VectorQuery};
use lancedb::{DistanceType, Table};

use self::rows::{ScoredChunk, ScoredDocument, Scoring, scored_chunks, scored_documents};
use super::document::{DocumentId, StoredDocument, documents_filter};
use super::snippet::{Tokenizers, query_words, snippet};
use super::{
    Hit, QueryError, SearchMode, SearchRequest, SearchResult, SearchUnit, check_search_request,
    run_blocking,
};
use crate::embedding::Embedding;
use crate::index::{Index, QueryTables};
use crate::types::{BoxError, CollectionName, EmbeddingModel, EmbeddingVector};

mod rows;
#[cfg(test)]
mod tests;

/// How many times the limit a vector search reads again as full vectors. Any
/// value makes the distances exact; a larger one finds more of the nearest
/// rows that the compressed vectors ranked too low. LanceDB's documentation
/// says to start between 5 and 50.
const REFINE_FACTOR: u32 = 25;

/// The constant of the reciprocal rank fusion that LanceDB uses.
const RECIPROCAL_RANK_K: f64 = 60.0;

/// The limit for reading all the chunk rows of the documents of one search,
/// which are far fewer than this. Their number cannot be taken from the
/// documents, because rows that disagree with them can be there too.
const EVERY_ROW: usize = u32::MAX as usize;

const MODEL_METADATA_KEY: &str = "mdya.embedding_model";

const DOCUMENT_COLUMNS: &[&str] = &["collection", "key", "body", "chunk_ranges"];
const CHUNK_COLUMNS: &[&str] = &[
    "collection",
    "key",
    "chunk_index",
    "range_start",
    "range_end",
    "text",
];

pub(super) async fn run(
    embedding: &dyn Embedding,
    index: &dyn Index,
    tokenizers: &Tokenizers,
    request: SearchRequest,
) -> Result<SearchResult, QueryError> {
    check_search_request(&request)?;
    // The query is embedded before the tables are taken: making the first
    // embedding can take a long time, and the tables are pinned to a version
    // that maintenance can clean up.
    let embedded = embed_query(embedding, &request).await?;
    let tables = index.tables_for_query().await;
    let Some(tables) = tables.map_err(QueryError::Index)? else {
        return Ok(SearchResult {
            hits: Vec::new(),
            warnings: Vec::new(),
        });
    };
    search(tokenizers, &tables, request, embedded)
        .await
        .map_err(|source| QueryError::CannotRead { source })
}

struct QueryEmbedding {
    vector: EmbeddingVector,
    model: EmbeddingModel,
}

async fn embed_query(
    embedding: &dyn Embedding,
    request: &SearchRequest,
) -> Result<Option<QueryEmbedding>, QueryError> {
    if request.mode == SearchMode::FullText {
        return Ok(None);
    }
    let vector = embedding.embed_query(&request.query).await;
    let vector = vector.map_err(QueryError::Embedding)?;
    let model = embedding.model().await.map_err(QueryError::Embedding)?;
    Ok(Some(QueryEmbedding { vector, model }))
}

async fn search(
    tokenizers: &Tokenizers,
    tables: &QueryTables,
    request: SearchRequest,
    embedded: Option<QueryEmbedding>,
) -> Result<SearchResult, BoxError> {
    let tokenizer = tokenizers.get().await?;
    let (vector_side, warnings) = vector_side(tables, embedded).await?;
    let search = Search {
        tables,
        request: &request,
        vector_side: &vector_side,
    };
    let drafts = match request.unit {
        SearchUnit::Document => search.document_drafts().await?,
        SearchUnit::Chunk => search.chunk_drafts().await?,
    };
    let query = request.query;
    let hits = run_blocking(move || finish_hits(tokenizer, &query, drafts)).await?;
    Ok(SearchResult { hits, warnings })
}

/// A hit before its snippet is cut.
struct Draft {
    id: DocumentId,
    score: f64,
    chunk_index: usize,
    chunk_count: usize,
    chunk_text: String,
}

fn finish_hits(
    mut tokenizer: Box<
        dyn lance_index::scalar::inverted::tokenizer::document_tokenizer::LanceTokenizer,
    >,
    query: &str,
    drafts: Vec<Draft>,
) -> Vec<Hit> {
    let words = query_words(&mut tokenizer, query);
    drafts
        .into_iter()
        .map(|draft| Hit {
            snippet: snippet(&mut tokenizer, &draft.chunk_text, &words),
            collection: draft.id.0,
            key: draft.id.1,
            score: draft.score,
            chunk_index: draft.chunk_index,
            chunk_count: draft.chunk_count,
        })
        .collect()
}

/// What the vector search side of a search does.
enum VectorSide {
    /// A full-text search, which has no vector search side.
    Unused,
    /// The index's vectors are not comparable with the query's: no hits.
    NoHits,
    Search(EmbeddingVector),
}

async fn vector_side(
    tables: &QueryTables,
    embedded: Option<QueryEmbedding>,
) -> Result<(VectorSide, Vec<String>), BoxError> {
    let Some(embedded) = embedded else {
        return Ok((VectorSide::Unused, Vec::new()));
    };
    let document = column_model(&tables.document, "vectors").await?;
    let chunk = column_model(&tables.chunk, "vector").await?;
    let query_model = Some(&embedded.model);
    if document.as_ref() == query_model && chunk.as_ref() == query_model {
        return Ok((VectorSide::Search(embedded.vector), Vec::new()));
    }
    let warning = model_warning(&embedded.model, document, chunk);
    tracing::warn!("{warning}");
    Ok((VectorSide::NoHits, vec![warning]))
}

/// The embedding model of a vector column, or `None` when the table has no
/// such column, as partway through switching to rebuilt vectors.
async fn column_model(table: &Table, column: &str) -> Result<Option<EmbeddingModel>, BoxError> {
    let schema = table.schema().await?;
    Ok(schema.field_with_name(column).ok().and_then(field_model))
}

fn field_model(field: &Field) -> Option<EmbeddingModel> {
    let name = field.metadata().get(MODEL_METADATA_KEY)?;
    Some(EmbeddingModel {
        name: name.clone(),
        dimension: vector_dimension(field.data_type())?,
    })
}

fn vector_dimension(data_type: &DataType) -> Option<usize> {
    match data_type {
        DataType::FixedSizeList(_, size) => usize::try_from(*size).ok(),
        DataType::List(item) => vector_dimension(item.data_type()),
        _ => None,
    }
}

fn model_warning(
    query_model: &EmbeddingModel,
    document: Option<EmbeddingModel>,
    chunk: Option<EmbeddingModel>,
) -> String {
    let index_model = match (document, chunk) {
        (Some(document), Some(chunk)) if document == chunk => model_text(&document),
        _ => "more than one model, because a rebuild has not finished".to_owned(),
    };
    format!(
        "The index's embeddings were made with {index_model}, but the query's embedding was made with {}. Vector search finds nothing until you run mdya embedding rebuild, or restart the MCP server if the model setting changed after it started.",
        model_text(query_model)
    )
}

fn model_text(model: &EmbeddingModel) -> String {
    format!("{} ({} dimensions)", model.name, model.dimension)
}

struct Search<'a> {
    tables: &'a QueryTables,
    request: &'a SearchRequest,
    vector_side: &'a VectorSide,
}

/// A table to search, and the columns to search and return.
struct Target<'a> {
    table: &'a Table,
    text_column: &'static str,
    vector_column: &'static str,
    columns: &'static [&'static str],
}

struct RankedRows {
    rows: Vec<RecordBatch>,
    scoring: Scoring,
    /// Whether the scores are replaced by the reciprocal rank of the order
    /// they give: a hybrid search whose vector side has no hits.
    rank_only: bool,
}

impl Search<'_> {
    fn document_target(&self) -> Target<'_> {
        Target {
            table: &self.tables.document,
            text_column: "body",
            vector_column: "vectors",
            columns: DOCUMENT_COLUMNS,
        }
    }

    fn chunk_target(&self) -> Target<'_> {
        Target {
            table: &self.tables.chunk,
            text_column: "text",
            vector_column: "vector",
            columns: CHUNK_COLUMNS,
        }
    }

    async fn document_drafts(&self) -> Result<Vec<Draft>, BoxError> {
        let target = self.document_target();
        let limit = self.request.limit;
        let mut fetch = limit.saturating_add(1);
        let (mut documents, rank_only) = loop {
            let ranked = self.ranked_rows(&target, fetch).await?;
            let mut documents = scored_documents(&ranked.rows, ranked.scoring)?;
            documents.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
            if documents.len() < fetch || !tied_at_limit(&documents, limit, |row| row.score) {
                break (documents, ranked.rank_only);
            }
            fetch = fetch.saturating_mul(2);
        };
        documents.truncate(limit);
        if rank_only {
            set_reciprocal_ranks(documents.iter_mut().map(|document| &mut document.score));
        }
        let shown = self.chunks_to_show(&documents).await?;
        documents
            .into_iter()
            .map(|scored| {
                let chunk_index = shown.get(&scored.id).copied().unwrap_or(0);
                Ok(Draft {
                    chunk_index,
                    chunk_count: scored.document.chunk_count(),
                    chunk_text: scored.document.chunk_text(chunk_index)?.to_owned(),
                    id: scored.id,
                    score: scored.score,
                })
            })
            .collect()
    }

    async fn chunk_drafts(&self) -> Result<Vec<Draft>, BoxError> {
        let target = self.chunk_target();
        let wanted = self.request.limit;
        let mut fetch = wanted.saturating_add(1);
        // Rows that disagree with the document table are dropped, so asking
        // for exactly the limit can return too few. Fewer rows than asked for
        // means there are no more.
        loop {
            let ranked = self.ranked_rows(&target, fetch).await?;
            let chunks = scored_chunks(&ranked.rows, ranked.scoring)?;
            let fetched = chunks.len();
            let mut agreeing = self.chunks_that_agree(chunks).await?;
            agreeing.sort_by(|a, b| {
                let (a_chunk, b_chunk) = (&a.0, &b.0);
                b_chunk
                    .score
                    .total_cmp(&a_chunk.score)
                    .then_with(|| a_chunk.id.cmp(&b_chunk.id))
                    .then_with(|| a_chunk.index.cmp(&b_chunk.index))
            });
            let enough = agreeing.len() > wanted
                && !tied_at_limit(&agreeing, wanted, |(chunk, _)| chunk.score);
            if enough || fetched < fetch {
                agreeing.truncate(wanted);
                if ranked.rank_only {
                    set_reciprocal_ranks(agreeing.iter_mut().map(|(chunk, _)| &mut chunk.score));
                }
                return Ok(agreeing
                    .into_iter()
                    .map(|(chunk, chunk_count)| Draft {
                        id: chunk.id,
                        score: chunk.score,
                        chunk_index: chunk.index,
                        chunk_count,
                        chunk_text: chunk.text,
                    })
                    .collect());
            }
            fetch = fetch.saturating_mul(2);
        }
    }

    /// The chunks whose rows agree with the document table, each with its
    /// document's number of chunks.
    async fn chunks_that_agree(
        &self,
        chunks: Vec<ScoredChunk>,
    ) -> Result<Vec<(ScoredChunk, usize)>, BoxError> {
        let ids = distinct_ids(chunks.iter().map(|chunk| &chunk.id));
        let documents = StoredDocument::read_many(&self.tables.document, &ids).await?;
        Ok(chunks
            .into_iter()
            .filter_map(|chunk| {
                let document = documents.get(&chunk.id)?;
                document
                    .holds_chunk(chunk.index, &chunk.bytes, &chunk.text)
                    .then(|| {
                        let chunk_count = document.chunk_count();
                        (chunk, chunk_count)
                    })
            })
            .collect())
    }

    async fn ranked_rows(&self, target: &Target<'_>, limit: usize) -> Result<RankedRows, BoxError> {
        let query = target
            .table
            .query()
            .select(Select::columns(target.columns))
            .limit(limit);
        let query = with_filter(query, collections_filter(&self.request.collections));
        let words = match_query(&self.request.query, target.text_column);
        let ranked = |rows, scoring| RankedRows {
            rows,
            scoring,
            rank_only: false,
        };
        Ok(match (self.request.mode, self.vector_side) {
            (SearchMode::FullText, _) => {
                let rows = collect(query.full_text_search(words).execute()).await?;
                ranked(rows, Scoring::Bm25)
            }
            (SearchMode::Hybrid, VectorSide::NoHits) => RankedRows {
                rows: collect(query.full_text_search(words).execute()).await?,
                scoring: Scoring::Bm25,
                rank_only: true,
            },
            (SearchMode::Vector, VectorSide::Search(vector)) => {
                let rows = collect(nearest(query, vector, target)?.execute()).await?;
                ranked(rows, Scoring::CosineSimilarity)
            }
            (SearchMode::Hybrid, VectorSide::Search(vector)) => {
                let query = nearest(query.full_text_search(words), vector, target)?;
                let options = QueryExecutionOptions::default();
                let rows = collect(query.execute_hybrid(options)).await?;
                ranked(rows, Scoring::ReciprocalRank)
            }
            (SearchMode::Vector | SearchMode::Hybrid, _) => {
                ranked(Vec::new(), Scoring::CosineSimilarity)
            }
        })
    }

    /// Which chunk each document shows, for the documents that have a chunk
    /// to choose. A document without one shows its first chunk.
    async fn chunks_to_show(
        &self,
        documents: &[ScoredDocument],
    ) -> Result<HashMap<DocumentId, usize>, BoxError> {
        let by_id: HashMap<&DocumentId, &ScoredDocument> = documents
            .iter()
            .map(|scored| (&scored.id, scored))
            .collect();
        let mut best = Best::default();
        if self.request.mode != SearchMode::Vector {
            let chunks = self.chunks_with_the_words(documents).await?;
            best.consider(&by_id, chunks);
        }
        if let (SearchMode::Vector | SearchMode::Hybrid, VectorSide::Search(vector)) =
            (self.request.mode, self.vector_side)
        {
            let rest: Vec<&ScoredDocument> = documents
                .iter()
                .filter(|scored| !best.chosen.contains_key(&scored.id))
                .collect();
            let chunks = self.nearest_chunks(&rest, vector).await?;
            best.consider(&by_id, chunks);
        }
        Ok(best
            .chosen
            .into_iter()
            .map(|(id, (index, _))| (id, index))
            .collect())
    }

    async fn chunks_with_the_words(
        &self,
        documents: &[ScoredDocument],
    ) -> Result<Vec<ScoredChunk>, BoxError> {
        let Some(filter) = documents_filter(&ids_of(documents.iter())) else {
            return Ok(Vec::new());
        };
        let words = match_query(&self.request.query, "text");
        let query = self
            .tables
            .chunk
            .query()
            .select(Select::columns(CHUNK_COLUMNS))
            .only_if_expr(filter)
            .full_text_search(words)
            .limit(EVERY_ROW);
        scored_chunks(&collect(query.execute()).await?, Scoring::Bm25)
    }

    /// The chunks of `documents`, nearest first. It reads every vector of
    /// these documents instead of the vector index, so it does not miss the
    /// nearest chunk.
    async fn nearest_chunks(
        &self,
        documents: &[&ScoredDocument],
        vector: &[f32],
    ) -> Result<Vec<ScoredChunk>, BoxError> {
        let Some(filter) = documents_filter(&ids_of(documents.iter().copied())) else {
            return Ok(Vec::new());
        };
        let query = self
            .tables
            .chunk
            .query()
            .select(Select::columns(CHUNK_COLUMNS))
            .only_if_expr(filter.and(col("vector").is_not_null()))
            .limit(EVERY_ROW)
            .nearest_to(vector)?
            .column("vector")
            .distance_type(DistanceType::Cosine)
            .bypass_vector_index();
        scored_chunks(&collect(query.execute()).await?, Scoring::Nearness)
    }
}

/// The best chunk found so far for each document: the highest score, then the
/// smallest number.
#[derive(Default)]
struct Best {
    chosen: HashMap<DocumentId, (usize, f64)>,
}

impl Best {
    fn consider(
        &mut self,
        documents: &HashMap<&DocumentId, &ScoredDocument>,
        chunks: Vec<ScoredChunk>,
    ) {
        for chunk in chunks {
            let Some(scored) = documents.get(&chunk.id) else {
                continue;
            };
            if !scored
                .document
                .holds_chunk(chunk.index, &chunk.bytes, &chunk.text)
            {
                continue;
            }
            let better = self.chosen.get(&chunk.id).is_none_or(|(index, score)| {
                chunk.score > *score || (chunk.score == *score && chunk.index < *index)
            });
            if better {
                self.chosen.insert(chunk.id, (chunk.index, chunk.score));
            }
        }
    }
}

fn ids_of<'a>(documents: impl Iterator<Item = &'a ScoredDocument>) -> Vec<DocumentId> {
    documents.map(|scored| scored.id.clone()).collect()
}

/// Whether the last row within `limit` ties with the first row past it, in
/// rows sorted by score. The rows LanceDB returns at a limit need not be the
/// tied ones that come first in the order of the hits, so which of them to
/// keep is only known after reading them all.
fn tied_at_limit<T>(sorted: &[T], limit: usize, score: impl Fn(&T) -> f64) -> bool {
    let last_kept = limit.checked_sub(1).and_then(|last| sorted.get(last));
    match (last_kept, sorted.get(limit)) {
        (Some(kept), Some(left_out)) => score(kept) == score(left_out),
        _ => false,
    }
}

fn distinct_ids<'a>(ids: impl Iterator<Item = &'a DocumentId>) -> Vec<DocumentId> {
    let mut seen = HashSet::new();
    ids.filter(|id| seen.insert(*id)).cloned().collect()
}

fn set_reciprocal_ranks<'a>(scores: impl Iterator<Item = &'a mut f64>) {
    for (rank, score) in scores.enumerate() {
        *score = 1.0 / (RECIPROCAL_RANK_K + rank as f64);
    }
}

fn collections_filter(collections: &[CollectionName]) -> Option<DfExpr> {
    if collections.is_empty() {
        return None;
    }
    let names = collections
        .iter()
        .map(|name| lit(name.0.as_str()))
        .collect();
    Some(col("collection").in_list(names, false))
}

fn with_filter<Q: QueryBase>(query: Q, filter: Option<DfExpr>) -> Q {
    match filter {
        Some(filter) => query.only_if_expr(filter),
        None => query,
    }
}

fn match_query(query: &str, column: &str) -> FullTextSearchQuery {
    let words = MatchQuery::new(query.to_owned()).with_column(Some(column.to_owned()));
    FullTextSearchQuery::new_query(FtsQuery::from(words))
}

fn nearest(
    query: lancedb::query::Query,
    vector: &[f32],
    target: &Target<'_>,
) -> lancedb::Result<VectorQuery> {
    Ok(query
        .nearest_to(vector)?
        .column(target.vector_column)
        .distance_type(DistanceType::Cosine)
        .refine_factor(REFINE_FACTOR))
}

async fn collect(
    stream: impl Future<Output = lancedb::Result<SendableRecordBatchStream>>,
) -> Result<Vec<RecordBatch>, BoxError> {
    Ok(stream.await?.try_collect().await?)
}
