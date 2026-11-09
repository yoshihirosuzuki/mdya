//! Reading document rows: the body and the byte ranges of the chunks.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use futures::TryStreamExt;
use lancedb::Table;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::UInt64Type;
use lancedb::arrow::arrow_array::{ArrayRef, RecordBatch};
use lancedb::expr::{DfExpr, col, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use super::{ChunkRangeRequest, QueryError};
use crate::types::{BoxError, CollectionName, Key};

#[cfg(test)]
mod tests;

/// What tells a document apart in the index.
pub(super) type DocumentId = (CollectionName, Key);

/// What the document table holds about a document, for reading its text.
#[derive(Debug)]
pub(super) struct StoredDocument {
    body: String,
    chunk_ranges: Vec<Range<u64>>,
}

impl StoredDocument {
    /// The document's row, or `None` when the table has none.
    pub(super) async fn read(
        table: &Table,
        collection: &CollectionName,
        key: &Key,
    ) -> Result<Option<Self>, BoxError> {
        let rows: Vec<RecordBatch> = table
            .query()
            .only_if_expr(document_filter(collection, key))
            .select(Select::columns(&["body", "chunk_ranges"]))
            .limit(1)
            .execute()
            .await?
            .try_collect()
            .await?;
        let found = rows.iter().find(|batch| batch.num_rows() > 0);
        found.map(|batch| Self::from_row(batch, 0)).transpose()
    }

    /// The rows of the documents in `ids`, which holds each document once. A
    /// document without a row is not in the answer.
    pub(super) async fn read_many(
        table: &Table,
        ids: &[DocumentId],
    ) -> Result<HashMap<DocumentId, Self>, BoxError> {
        let Some(filter) = documents_filter(ids) else {
            return Ok(HashMap::new());
        };
        let rows: Vec<RecordBatch> = table
            .query()
            .only_if_expr(filter)
            .select(Select::columns(&[
                "collection",
                "key",
                "body",
                "chunk_ranges",
            ]))
            .limit(ids.len())
            .execute()
            .await?
            .try_collect()
            .await?;
        let mut documents = HashMap::with_capacity(ids.len());
        for batch in &rows {
            for row in 0..batch.num_rows() {
                documents.insert(document_id(batch, row)?, Self::from_row(batch, row)?);
            }
        }
        Ok(documents)
    }

    pub(super) fn from_row(rows: &RecordBatch, row: usize) -> Result<Self, BoxError> {
        let bodies = rows
            .column_by_name("body")
            .and_then(|column| column.as_string_opt::<i32>())
            .ok_or_else(|| unexpected_column("body"))?;
        let ranges = rows
            .column_by_name("chunk_ranges")
            .and_then(|column| column.as_list_opt::<i32>())
            .ok_or_else(|| unexpected_column("chunk_ranges"))?;
        Ok(Self {
            body: bodies.value(row).to_owned(),
            chunk_ranges: chunk_ranges_of(&ranges.value(row))?,
        })
    }

    /// The text `range` asks for. `range` has passed
    /// [`super::check_chunk_range`], so an end comes with a start.
    pub(super) fn text(&self, range: ChunkRangeRequest) -> Result<String, QueryError> {
        let Some(start) = range.start else {
            return Ok(self.body.clone());
        };
        let first = self
            .chunk_ranges
            .get(start)
            .ok_or(QueryError::ChunkNotFound)?;
        let end = match range.end {
            None => first.end,
            Some(end) => self
                .chunk_ranges
                .get(end)
                .map_or(self.body_len(), |c| c.end),
        };
        let bytes = first.start..end;
        match self.in_body(&bytes) {
            Some(text) => Ok(text.to_owned()),
            None => Err(QueryError::CannotRead {
                source: outside_the_body(&bytes),
            }),
        }
    }

    pub(super) fn chunk_count(&self) -> usize {
        self.chunk_ranges.len()
    }

    /// The text of the chunk numbered `index`, or an empty text when the
    /// document has no such chunk.
    pub(super) fn chunk_text(&self, index: usize) -> Result<&str, BoxError> {
        let Some(bytes) = self.chunk_ranges.get(index) else {
            return Ok("");
        };
        self.in_body(bytes).ok_or_else(|| outside_the_body(bytes))
    }

    /// Whether a row of the chunk table agrees with this document: the
    /// document has a chunk with that number and range, and the body holds
    /// that text there. The text is compared too, because the body can change
    /// without the ranges changing.
    pub(super) fn holds_chunk(&self, index: usize, bytes: &Range<u64>, text: &str) -> bool {
        self.chunk_ranges.get(index) == Some(bytes) && self.in_body(bytes) == Some(text)
    }

    fn body_len(&self) -> u64 {
        self.body.len() as u64
    }

    fn in_body(&self, bytes: &Range<u64>) -> Option<&str> {
        let start = usize::try_from(bytes.start).ok()?;
        let end = usize::try_from(bytes.end).ok()?;
        self.body.get(start..end)
    }
}

fn outside_the_body(bytes: &Range<u64>) -> BoxError {
    format!("the stored chunk range {bytes:?} is not in the document's body").into()
}

/// The collection and the key in one row of a table.
pub(super) fn document_id(rows: &RecordBatch, row: usize) -> Result<DocumentId, BoxError> {
    let text = |name| {
        let column = rows.column_by_name(name);
        let column = column.and_then(|column| column.as_string_opt::<i32>());
        column
            .map(|column| column.value(row).to_owned())
            .ok_or_else(|| unexpected_column(name))
    };
    Ok((CollectionName(text("collection")?), Key(text("key")?)))
}

fn document_filter(collection: &CollectionName, key: &Key) -> DfExpr {
    let collection = col("collection").eq(lit(collection.0.as_str()));
    collection.and(col("key").eq(lit(key.0.as_str())))
}

/// The condition that selects the rows of the documents in `ids`, or `None`
/// when there are none.
pub(super) fn documents_filter(ids: &[DocumentId]) -> Option<DfExpr> {
    let mut keys_by_collection: BTreeMap<&str, Vec<DfExpr>> = BTreeMap::new();
    for (collection, key) in ids {
        let keys = keys_by_collection.entry(collection.0.as_str()).or_default();
        keys.push(lit(key.0.as_str()));
    }
    keys_by_collection
        .into_iter()
        .map(|(collection, keys)| {
            let collection = col("collection").eq(lit(collection));
            collection.and(col("key").in_list(keys, false))
        })
        .reduce(DfExpr::or)
}

/// The byte ranges in one value of the `chunk_ranges` column.
fn chunk_ranges_of(ranges: &ArrayRef) -> Result<Vec<Range<u64>>, BoxError> {
    let ranges = ranges
        .as_struct_opt()
        .ok_or_else(|| unexpected_column("chunk_ranges"))?;
    let bound = |name| {
        let values = ranges.column_by_name(name);
        let values = values.and_then(|values| values.as_primitive_opt::<UInt64Type>());
        values.ok_or_else(|| unexpected_column("chunk_ranges"))
    };
    let (starts, ends) = (bound("start")?, bound("end")?);
    let pairs = starts.values().iter().zip(ends.values().iter());
    Ok(pairs.map(|(start, end)| *start..*end).collect())
}

pub(super) fn unexpected_column(name: &str) -> BoxError {
    format!("the {name} column of the index is missing or has an unexpected type").into()
}
