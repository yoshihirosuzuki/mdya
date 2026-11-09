use std::sync::Arc;

use lancedb::arrow::arrow_array::{
    Array, ArrayRef, ListArray, RecordBatch, StringArray, StructArray, UInt64Array,
};
use lancedb::arrow::arrow_buffer::OffsetBuffer;
use lancedb::arrow::arrow_schema::{DataType, Field, Fields};
use lancedb::table::{Duration as TimeDelta, OptimizeAction};

use super::StoredDocument;
use crate::index::IndexError;
use crate::index::test_double::{IndexCall, TestIndex};
use crate::query::test_tables::{
    TestDocument, TestTables, document_rows, index_handing_out, test_embedding,
};
use crate::query::{ChunkRangeRequest, Query, QueryError, create};
use crate::types::{BoxError, CollectionName, Key};

const COLLECTION: &str = "notes";

const WHOLE: ChunkRangeRequest = ChunkRangeRequest {
    start: None,
    end: None,
};

fn only_start(start: usize) -> ChunkRangeRequest {
    ChunkRangeRequest {
        start: Some(start),
        end: None,
    }
}

fn start_and_end(start: usize, end: usize) -> ChunkRangeRequest {
    ChunkRangeRequest {
        start: Some(start),
        end: Some(end),
    }
}

fn query_over(tables: &TestTables) -> Arc<dyn Query> {
    create(Arc::new(test_embedding()), Arc::new(tables.index()))
}

async fn get(
    query: &Arc<dyn Query>,
    collection: &str,
    key: &str,
    range: ChunkRangeRequest,
) -> Result<String, QueryError> {
    let collection = CollectionName(collection.to_owned());
    query
        .get_document(&collection, &Key(key.to_owned()), range)
        .await
}

const THREE_CHUNKS: &[(u64, u64)] = &[(0, 6), (6, 13), (13, 18)];
const THREE_CHUNKS_BODY: &str = "first second third";

async fn three_chunks() -> TestTables {
    let document = TestDocument::new(COLLECTION, "doc", THREE_CHUNKS_BODY, THREE_CHUNKS);
    TestTables::create(&[document]).await
}

#[tokio::test]
async fn returns_the_whole_body_without_a_range() {
    let tables = three_chunks().await;
    let text = get(&query_over(&tables), COLLECTION, "doc", WHOLE).await;
    assert_eq!(text.unwrap(), THREE_CHUNKS_BODY);
}

#[tokio::test]
async fn returns_the_text_of_one_chunk_with_only_a_start() {
    let tables = three_chunks().await;
    let text = get(&query_over(&tables), COLLECTION, "doc", only_start(1)).await;
    assert_eq!(text.unwrap(), "second ");
}

#[tokio::test]
async fn returns_the_text_from_the_start_chunk_to_the_end_chunk() {
    let tables = three_chunks().await;
    let text = get(&query_over(&tables), COLLECTION, "doc", start_and_end(0, 1)).await;
    assert_eq!(text.unwrap(), "first second ");
}

#[tokio::test]
async fn returns_one_chunk_when_the_start_and_the_end_are_the_same() {
    let tables = three_chunks().await;
    let text = get(&query_over(&tables), COLLECTION, "doc", start_and_end(2, 2)).await;
    assert_eq!(text.unwrap(), "third");
}

#[tokio::test]
async fn an_end_past_the_last_chunk_reads_to_the_end_of_the_body() {
    let tables = three_chunks().await;
    let text = get(
        &query_over(&tables),
        COLLECTION,
        "doc",
        start_and_end(1, 99),
    )
    .await;
    assert_eq!(text.unwrap(), "second third");
}

#[tokio::test]
async fn overlapping_chunks_read_as_one_continuous_part_of_the_body() {
    let body = "abcdefghijklmnopqr";
    let document = TestDocument::new(COLLECTION, "doc", body, &[(0, 8), (4, 12), (8, 18)]);
    let tables = TestTables::create(&[document]).await;
    let text = get(&query_over(&tables), COLLECTION, "doc", start_and_end(0, 1)).await;
    assert_eq!(text.unwrap(), "abcdefghijkl");
}

#[tokio::test]
async fn reads_the_chunks_of_a_body_of_multibyte_characters() {
    let body = "日本語の文章です";
    let document = TestDocument::new(COLLECTION, "doc", body, &[(0, 12), (12, 24)]);
    let tables = TestTables::create(&[document]).await;
    let query = query_over(&tables);
    assert_eq!(
        get(&query, COLLECTION, "doc", only_start(1)).await.unwrap(),
        "文章です"
    );
}

#[tokio::test]
async fn a_start_at_the_number_of_chunks_is_chunk_not_found() {
    let tables = three_chunks().await;
    let result = get(&query_over(&tables), COLLECTION, "doc", only_start(3)).await;
    assert!(matches!(result, Err(QueryError::ChunkNotFound)));
}

#[tokio::test]
async fn a_start_past_the_chunks_is_chunk_not_found_even_with_an_end() {
    let tables = three_chunks().await;
    let result = get(&query_over(&tables), COLLECTION, "doc", start_and_end(7, 9)).await;
    assert!(matches!(result, Err(QueryError::ChunkNotFound)));
}

#[tokio::test]
async fn a_document_with_an_empty_body_has_an_empty_text_and_no_chunk() {
    let empty = TestDocument::new(COLLECTION, "empty", "", &[]);
    let other = TestDocument::new(COLLECTION, "other", "text", &[(0, 4)]);
    let tables = TestTables::create(&[empty, other]).await;
    let query = query_over(&tables);
    assert_eq!(get(&query, COLLECTION, "empty", WHOLE).await.unwrap(), "");
    let result = get(&query, COLLECTION, "empty", only_start(0)).await;
    assert!(matches!(result, Err(QueryError::ChunkNotFound)));
}

#[tokio::test]
async fn a_key_that_is_not_in_the_collection_is_document_not_found() {
    let tables = three_chunks().await;
    let result = get(&query_over(&tables), COLLECTION, "missing", WHOLE).await;
    assert!(matches!(result, Err(QueryError::DocumentNotFound)));
}

#[tokio::test]
async fn an_index_without_tables_has_no_document() {
    let index = TestIndex::new();
    let query = create(Arc::new(test_embedding()), Arc::new(index));
    let result = get(&query, COLLECTION, "doc", WHOLE).await;
    assert!(matches!(result, Err(QueryError::DocumentNotFound)));
}

#[tokio::test]
async fn reads_the_document_of_the_named_collection() {
    let first = TestDocument::new("first", "doc", "in first", &[(0, 8)]);
    let second = TestDocument::new("second", "doc", "in second", &[(0, 9)]);
    let tables = TestTables::create(&[first, second]).await;
    let query = query_over(&tables);
    assert_eq!(
        get(&query, "first", "doc", WHOLE).await.unwrap(),
        "in first"
    );
    assert_eq!(
        get(&query, "second", "doc", WHOLE).await.unwrap(),
        "in second"
    );
}

#[tokio::test]
async fn a_collection_name_and_a_key_match_only_themselves_whatever_they_contain() {
    let names = [
        ("c1", "a'b"),
        ("c1", "a\\b"),
        ("c1", "x' OR '1'='1"),
        ("c1", "100%"),
        ("c1", "a_b"),
        ("c1", ""),
        ("c1", "日本語"),
        ("c'2", "plain"),
    ];
    let documents: Vec<TestDocument> = names
        .iter()
        .map(|(collection, key)| TestDocument::new(collection, key, key, &[]))
        .collect();
    let tables = TestTables::create(&documents).await;
    let query = query_over(&tables);
    for (collection, key) in names {
        assert_eq!(get(&query, collection, key, WHOLE).await.unwrap(), key);
    }
}

#[tokio::test]
async fn a_range_that_is_not_valid_is_refused_before_reading_the_index() {
    let index = Arc::new(TestIndex::new());
    let query = create(Arc::new(test_embedding()), index.clone());
    for range in [
        ChunkRangeRequest {
            start: None,
            end: Some(1),
        },
        start_and_end(2, 1),
    ] {
        let result = get(&query, COLLECTION, "doc", range).await;
        assert!(matches!(result, Err(QueryError::InvalidChunkRange)));
    }
    assert_eq!(index.calls(), Vec::new());
}

#[tokio::test]
async fn asks_for_the_tables_once_for_each_call() {
    let tables = three_chunks().await;
    let index = Arc::new(tables.index());
    let query = create(Arc::new(test_embedding()), index.clone());
    get(&query, COLLECTION, "doc", WHOLE).await.unwrap();
    get(&query, COLLECTION, "doc", WHOLE).await.unwrap();
    assert_eq!(
        index.calls(),
        vec![IndexCall::TablesForQuery, IndexCall::TablesForQuery]
    );
}

#[tokio::test]
async fn a_failure_to_hand_over_the_tables_is_an_index_error() {
    let index = TestIndex::new().on_tables_for_query(|| {
        Err(IndexError::CannotRead {
            source: "the disk is gone".into(),
        })
    });
    let query = create(Arc::new(test_embedding()), Arc::new(index));
    let result = get(&query, COLLECTION, "doc", WHOLE).await;
    assert!(matches!(
        result,
        Err(QueryError::Index(IndexError::CannotRead { .. }))
    ));
}

#[tokio::test]
async fn a_pinned_version_that_maintenance_cleaned_up_cannot_be_read() {
    let tables = three_chunks().await;
    let index = index_handing_out(tables.pinned().await);
    let query = create(Arc::new(test_embedding()), Arc::new(index));

    let replacement = TestDocument::new(COLLECTION, "new", "new text", &[(0, 8)]);
    let newer = document_rows(&[replacement]);
    tables.document.add(newer).execute().await.unwrap();
    tables.document.delete("key = 'doc'").await.unwrap();
    tables.document.optimize(OptimizeAction::All).await.unwrap();
    tables
        .document
        .optimize(OptimizeAction::Prune {
            older_than: Some(TimeDelta::zero()),
            delete_unverified: Some(true),
            error_if_tagged_old_versions: None,
        })
        .await
        .unwrap();

    let result = get(&query, COLLECTION, "doc", WHOLE).await;
    assert!(
        matches!(result, Err(QueryError::CannotRead { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_stored_range_outside_the_body_cannot_be_read() {
    let document = TestDocument::new(COLLECTION, "doc", "short", &[(0, 99)]);
    let tables = TestTables::create(&[document]).await;
    let result = get(&query_over(&tables), COLLECTION, "doc", only_start(0)).await;
    assert!(matches!(result, Err(QueryError::CannotRead { .. })));
}

#[tokio::test]
async fn a_stored_range_inside_a_character_cannot_be_read() {
    let document = TestDocument::new(COLLECTION, "doc", "日本語", &[(0, 1)]);
    let tables = TestTables::create(&[document]).await;
    let result = get(&query_over(&tables), COLLECTION, "doc", only_start(0)).await;
    assert!(matches!(result, Err(QueryError::CannotRead { .. })));
}

fn body_column() -> ArrayRef {
    Arc::new(StringArray::from(vec!["text"]))
}

fn list_of(items: ArrayRef) -> ArrayRef {
    let field = Arc::new(Field::new("item", items.data_type().clone(), true));
    let offsets = OffsetBuffer::from_lengths([items.len()]);
    Arc::new(ListArray::try_new(field, offsets, items, None).unwrap())
}

fn read_row(columns: Vec<(&str, ArrayRef)>) -> Result<StoredDocument, BoxError> {
    StoredDocument::from_first_row(&RecordBatch::try_from_iter(columns).unwrap())
}

#[test]
fn a_row_without_a_body_column_cannot_be_read() {
    let ranges = list_of(Arc::new(UInt64Array::from(vec![0u64])));
    let error = read_row(vec![("chunk_ranges", ranges)]).unwrap_err();
    assert!(error.to_string().contains("body"), "{error}");
}

#[test]
fn a_row_with_a_chunk_ranges_column_of_another_type_cannot_be_read() {
    let ranges = body_column();
    let error = read_row(vec![("body", body_column()), ("chunk_ranges", ranges)]).unwrap_err();
    assert!(error.to_string().contains("chunk_ranges"), "{error}");
}

#[test]
fn chunk_ranges_that_are_not_structs_cannot_be_read() {
    let ranges = list_of(Arc::new(UInt64Array::from(vec![0u64])));
    let error = read_row(vec![("body", body_column()), ("chunk_ranges", ranges)]).unwrap_err();
    assert!(error.to_string().contains("chunk_ranges"), "{error}");
}

#[test]
fn chunk_ranges_without_an_end_cannot_be_read() {
    let fields = Fields::from(vec![Field::new("start", DataType::UInt64, false)]);
    let starts: ArrayRef = Arc::new(UInt64Array::from(vec![0u64]));
    let items = StructArray::try_new(fields, vec![starts], None).unwrap();
    let ranges = list_of(Arc::new(items));
    let error = read_row(vec![("body", body_column()), ("chunk_ranges", ranges)]).unwrap_err();
    assert!(error.to_string().contains("chunk_ranges"), "{error}");
}
