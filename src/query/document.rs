//! Reading a document row: its body and the byte ranges of its chunks.

use std::ops::Range;

use futures::TryStreamExt;
use lancedb::Table;
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::UInt64Type;
use lancedb::arrow::arrow_array::{ArrayRef, RecordBatch};
use lancedb::expr::{col, lit};
use lancedb::query::{ExecutableQuery, QueryBase, Select};

use super::{ChunkRangeRequest, QueryError};
use crate::types::{BoxError, CollectionName, Key};

#[cfg(test)]
mod tests;

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
        found.map(Self::from_first_row).transpose()
    }

    fn from_first_row(rows: &RecordBatch) -> Result<Self, BoxError> {
        let bodies = rows
            .column_by_name("body")
            .and_then(|column| column.as_string_opt::<i32>())
            .ok_or_else(|| unexpected_column("body"))?;
        let ranges = rows
            .column_by_name("chunk_ranges")
            .and_then(|column| column.as_list_opt::<i32>())
            .ok_or_else(|| unexpected_column("chunk_ranges"))?;
        Ok(Self {
            body: bodies.value(0).to_owned(),
            chunk_ranges: chunk_ranges_of(&ranges.value(0))?,
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
        self.slice(first.start..end)
    }

    fn body_len(&self) -> u64 {
        self.body.len() as u64
    }

    fn slice(&self, bytes: Range<u64>) -> Result<String, QueryError> {
        let start = usize::try_from(bytes.start).ok();
        let end = usize::try_from(bytes.end).ok();
        let text = start
            .zip(end)
            .and_then(|(start, end)| self.body.get(start..end));
        text.map(str::to_owned)
            .ok_or_else(|| QueryError::CannotRead {
                source: format!("the stored chunk range {bytes:?} is not in the document's body")
                    .into(),
            })
    }
}

fn document_filter(collection: &CollectionName, key: &Key) -> lancedb::expr::DfExpr {
    let collection = col("collection").eq(lit(collection.0.as_str()));
    collection.and(col("key").eq(lit(key.0.as_str())))
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

fn unexpected_column(name: &str) -> BoxError {
    format!("the {name} column of the index is missing or has an unexpected type").into()
}
