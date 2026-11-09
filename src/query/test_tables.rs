//! Real LanceDB tables for the unit tests of `query`. The tests replace `index`
//! with a test double but still read real tables, made here in the shape that
//! `index` publishes.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use lancedb::Table;
use lancedb::arrow::arrow_array::{
    ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, ListArray, RecordBatch, StringArray,
    StructArray, TimestampMicrosecondArray, UInt32Array, UInt64Array,
};
use lancedb::arrow::arrow_buffer::{NullBuffer, OffsetBuffer};
use lancedb::arrow::arrow_schema::{
    DataType, Field, FieldRef, Fields, Schema, SchemaRef, TimeUnit,
};
use tempfile::TempDir;

use crate::embedding::test_double::TestEmbedding;
use crate::index::QueryTables;
use crate::index::test_double::TestIndex;

const MODEL_NAME: &str = "test-model";
const DIMENSION: usize = 8;
const MODEL_METADATA_KEY: &str = "mdya.embedding_model";

/// The embedding the vectors in the tables are made with.
pub(super) fn test_embedding() -> TestEmbedding {
    TestEmbedding::new(MODEL_NAME, DIMENSION)
}

/// A document to put in the tables.
pub(super) struct TestDocument {
    collection: String,
    key: String,
    body: String,
    chunk_ranges: Vec<Range<u64>>,
}

impl TestDocument {
    pub(super) fn new(
        collection: &str,
        key: &str,
        body: &str,
        chunk_ranges: &[(u64, u64)],
    ) -> Self {
        Self {
            collection: collection.to_owned(),
            key: key.to_owned(),
            body: body.to_owned(),
            chunk_ranges: chunk_ranges.iter().map(|(s, e)| *s..*e).collect(),
        }
    }

    fn chunk_text(&self, range: &Range<u64>) -> &str {
        self.body
            .get(range.start as usize..range.end as usize)
            .unwrap_or("")
    }
}

/// The document table and the chunk table, in a temporary directory that lasts
/// as long as this value.
pub(super) struct TestTables {
    directory: TempDir,
    pub(super) document: Table,
    pub(super) chunk: Table,
}

impl TestTables {
    pub(super) async fn create(documents: &[TestDocument]) -> Self {
        let directory = TempDir::new().unwrap();
        let connection = lancedb::connect(directory.path().to_str().unwrap());
        let connection = connection.execute().await.unwrap();
        let document = connection.create_table("document", document_rows(documents));
        let document = document.execute().await.unwrap();
        let chunk = connection.create_table("chunk", chunk_rows(documents));
        let chunk = chunk.execute().await.unwrap();
        Self {
            directory,
            document,
            chunk,
        }
    }

    /// The tables, not pinned to a version, for a test that does not depend on
    /// what is written to them later.
    pub(super) fn for_query(&self) -> QueryTables {
        QueryTables {
            document: self.document.clone(),
            chunk: self.chunk.clone(),
        }
    }

    /// An `index` test double that hands out these tables.
    pub(super) fn index(&self) -> TestIndex {
        index_handing_out(self.for_query())
    }

    /// The tables opened again on a new connection and pinned to their latest
    /// versions, as `index` hands them to `query`.
    pub(super) async fn pinned(&self) -> QueryTables {
        let connection = lancedb::connect(self.directory.path().to_str().unwrap());
        let connection = connection.execute().await.unwrap();
        QueryTables {
            document: pinned_table(&connection, "document").await,
            chunk: pinned_table(&connection, "chunk").await,
        }
    }
}

/// An `index` test double that hands out `tables` on every call.
pub(super) fn index_handing_out(tables: QueryTables) -> TestIndex {
    TestIndex::new().on_tables_for_query(move || {
        Ok(Some(QueryTables {
            document: tables.document.clone(),
            chunk: tables.chunk.clone(),
        }))
    })
}

async fn pinned_table(connection: &lancedb::Connection, name: &str) -> Table {
    let table = connection.open_table(name).execute().await.unwrap();
    let version = table.version().await.unwrap();
    table.checkout(version).await.unwrap();
    table
}

fn vector_item() -> FieldRef {
    Arc::new(Field::new("item", DataType::Float32, true))
}

fn vector_type() -> DataType {
    DataType::FixedSizeList(vector_item(), DIMENSION as i32)
}

fn vectors_item() -> FieldRef {
    Arc::new(Field::new("item", vector_type(), true))
}

fn chunk_range_fields() -> Fields {
    Fields::from(vec![
        Field::new("start", DataType::UInt64, false),
        Field::new("end", DataType::UInt64, false),
    ])
}

fn chunk_range_item() -> FieldRef {
    Arc::new(Field::new(
        "item",
        DataType::Struct(chunk_range_fields()),
        true,
    ))
}

fn model_field(name: &str, data_type: DataType) -> Field {
    let metadata = HashMap::from([(MODEL_METADATA_KEY.to_owned(), MODEL_NAME.to_owned())]);
    Field::new(name, data_type, true).with_metadata(metadata)
}

fn document_schema() -> SchemaRef {
    let time = DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()));
    Arc::new(Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("time", time, true),
        Field::new("hash", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("chunk_ranges", DataType::List(chunk_range_item()), false),
        model_field("vectors", DataType::List(vectors_item())),
    ]))
}

fn chunk_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("chunk_index", DataType::UInt32, false),
        Field::new("range_start", DataType::UInt64, false),
        Field::new("range_end", DataType::UInt64, false),
        Field::new("text", DataType::Utf8, false),
        model_field("vector", vector_type()),
        Field::new("covers_whole", DataType::Boolean, true),
    ]))
}

fn strings<'a>(values: impl Iterator<Item = &'a str>) -> ArrayRef {
    Arc::new(StringArray::from(values.collect::<Vec<_>>()))
}

/// One vector of the embedding for each text, as the items of a list column.
fn vectors<'a>(texts: impl Iterator<Item = &'a str>) -> FixedSizeListArray {
    let embedding = test_embedding();
    let numbers: Vec<f32> = texts.flat_map(|text| embedding.vector_for(text)).collect();
    let values = Arc::new(Float32Array::from(numbers));
    FixedSizeListArray::try_new(vector_item(), DIMENSION as i32, values, None).unwrap()
}

pub(super) fn document_rows(documents: &[TestDocument]) -> RecordBatch {
    let chunk_counts = || documents.iter().map(|document| document.chunk_ranges.len());
    let columns: Vec<ArrayRef> = vec![
        strings(
            documents
                .iter()
                .map(|document| document.collection.as_str()),
        ),
        strings(documents.iter().map(|document| document.key.as_str())),
        Arc::new(TimestampMicrosecondArray::from(vec![None; documents.len()]).with_timezone("UTC")),
        strings(documents.iter().map(|_| "test-hash")),
        strings(documents.iter().map(|document| document.body.as_str())),
        chunk_ranges_column(documents),
        Arc::new(
            ListArray::try_new(
                vectors_item(),
                OffsetBuffer::from_lengths(chunk_counts()),
                Arc::new(vectors(documents.iter().flat_map(|document| {
                    let ranges = document.chunk_ranges.iter();
                    ranges.map(|range| document.chunk_text(range))
                }))),
                Some(NullBuffer::from_iter(chunk_counts().map(|count| count > 0))),
            )
            .unwrap(),
        ),
    ];
    RecordBatch::try_new(document_schema(), columns).unwrap()
}

fn chunk_ranges_column(documents: &[TestDocument]) -> ArrayRef {
    let ranges = || documents.iter().flat_map(|document| &document.chunk_ranges);
    let bounds: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from_iter_values(ranges().map(|r| r.start))),
        Arc::new(UInt64Array::from_iter_values(ranges().map(|r| r.end))),
    ];
    let items = StructArray::try_new(chunk_range_fields(), bounds, None).unwrap();
    let lengths = documents.iter().map(|document| document.chunk_ranges.len());
    let offsets = OffsetBuffer::from_lengths(lengths);
    Arc::new(ListArray::try_new(chunk_range_item(), offsets, Arc::new(items), None).unwrap())
}

fn chunk_rows(documents: &[TestDocument]) -> RecordBatch {
    let rows: Vec<(&TestDocument, usize, &Range<u64>)> = documents
        .iter()
        .flat_map(|document| {
            let ranges = document.chunk_ranges.iter().enumerate();
            ranges.map(move |(index, range)| (document, index, range))
        })
        .collect();
    let text = |row: &(&TestDocument, usize, &Range<u64>)| row.0.chunk_text(row.2).to_owned();
    let texts: Vec<String> = rows.iter().map(text).collect();
    let columns: Vec<ArrayRef> = vec![
        strings(rows.iter().map(|row| row.0.collection.as_str())),
        strings(rows.iter().map(|row| row.0.key.as_str())),
        Arc::new(UInt32Array::from_iter_values(
            rows.iter().map(|row| row.1 as u32),
        )),
        Arc::new(UInt64Array::from_iter_values(
            rows.iter().map(|row| row.2.start),
        )),
        Arc::new(UInt64Array::from_iter_values(
            rows.iter().map(|row| row.2.end),
        )),
        strings(texts.iter().map(String::as_str)),
        Arc::new(vectors(texts.iter().map(String::as_str))),
        Arc::new(BooleanArray::from(vec![Some(true); rows.len()])),
    ];
    RecordBatch::try_new(chunk_schema(), columns).unwrap()
}
