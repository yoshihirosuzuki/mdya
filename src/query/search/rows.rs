//! Reading the rows a search returns.

use std::ops::Range;

use lancedb::arrow::arrow_array::RecordBatch;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::{Float32Type, UInt32Type, UInt64Type};

use crate::query::document::{DocumentId, StoredDocument, document_id, unexpected_column};
use crate::types::BoxError;

/// What a row's score column means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scoring {
    /// The BM25 value of a full-text search.
    Bm25,
    /// The cosine similarity from the distance of a vector search.
    CosineSimilarity,
    /// The nearer the higher, for choosing the nearest chunk. Unlike
    /// [`Self::CosineSimilarity`], negative similarities stay apart.
    Nearness,
    /// The reciprocal rank value of a hybrid search.
    ReciprocalRank,
}

impl Scoring {
    fn column(self) -> &'static str {
        match self {
            Self::Bm25 => "_score",
            Self::CosineSimilarity | Self::Nearness => "_distance",
            Self::ReciprocalRank => "_relevance_score",
        }
    }

    pub(super) fn score(self, value: f32) -> f64 {
        let value = f64::from(value);
        match self {
            Self::Bm25 | Self::ReciprocalRank => value,
            Self::CosineSimilarity => (1.0 - value).max(0.0),
            Self::Nearness => -value,
        }
    }
}

pub(super) struct ScoredDocument {
    pub(super) id: DocumentId,
    pub(super) score: f64,
    pub(super) document: StoredDocument,
}

pub(super) struct ScoredChunk {
    pub(super) id: DocumentId,
    pub(super) score: f64,
    pub(super) index: usize,
    pub(super) bytes: Range<u64>,
    pub(super) text: String,
}

pub(super) fn scored_documents(
    rows: &[RecordBatch],
    scoring: Scoring,
) -> Result<Vec<ScoredDocument>, BoxError> {
    let mut documents = Vec::new();
    for batch in with_rows(rows) {
        let scores = scores(batch, scoring)?;
        for (row, score) in scores.into_iter().enumerate() {
            documents.push(ScoredDocument {
                id: document_id(batch, row)?,
                score,
                document: StoredDocument::from_row(batch, row)?,
            });
        }
    }
    Ok(documents)
}

pub(super) fn scored_chunks(
    rows: &[RecordBatch],
    scoring: Scoring,
) -> Result<Vec<ScoredChunk>, BoxError> {
    let mut chunks = Vec::new();
    for batch in with_rows(rows) {
        let scores = scores(batch, scoring)?;
        let indexes = column(batch, "chunk_index", |c| c.as_primitive_opt::<UInt32Type>())?;
        let starts = column(batch, "range_start", |c| c.as_primitive_opt::<UInt64Type>())?;
        let ends = column(batch, "range_end", |c| c.as_primitive_opt::<UInt64Type>())?;
        let texts = column(batch, "text", |c| c.as_string_opt::<i32>())?;
        for (row, score) in scores.into_iter().enumerate() {
            chunks.push(ScoredChunk {
                id: document_id(batch, row)?,
                score,
                index: indexes.value(row) as usize,
                bytes: starts.value(row)..ends.value(row),
                text: texts.value(row).to_owned(),
            });
        }
    }
    Ok(chunks)
}

/// A search with no rows at all returns a batch without the table's columns,
/// so batches without rows are not read.
fn with_rows(rows: &[RecordBatch]) -> impl Iterator<Item = &RecordBatch> {
    rows.iter().filter(|batch| batch.num_rows() > 0)
}

fn scores(batch: &RecordBatch, scoring: Scoring) -> Result<Vec<f64>, BoxError> {
    let values = column(batch, scoring.column(), |c| {
        c.as_primitive_opt::<Float32Type>()
    })?;
    Ok(values
        .values()
        .iter()
        .map(|value| scoring.score(*value))
        .collect())
}

fn column<'a, T: 'a>(
    batch: &'a RecordBatch,
    name: &str,
    cast: impl FnOnce(&'a lancedb::arrow::arrow_array::ArrayRef) -> Option<&'a T>,
) -> Result<&'a T, BoxError> {
    let found = batch.column_by_name(name).and_then(cast);
    found.ok_or_else(|| unexpected_column(name))
}
