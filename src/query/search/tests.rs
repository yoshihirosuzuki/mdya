use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use lancedb::DistanceType;
use lancedb::arrow::arrow_array::{ArrayRef, Float32Array, RecordBatch, StringArray};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
use lancedb::index::Index as LanceIndex;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::vector::IvfPqIndexBuilder;
use lancedb::table::{Duration as TimeDelta, OptimizeAction};
use tracing::Level;

use super::field_model;
use super::rows::{Scoring, scored_chunks, scored_documents};
use crate::embedding::test_double::{EmbeddingCall, EmbeddingFailure, TestEmbedding};
use crate::embedding::{Embedding, EmbeddingError};
use crate::index::test_double::{IndexCall, TestIndex};
use crate::index::{IndexError, QueryTables};
use crate::query::test_tables::{
    TestChunkRow, TestDocument, TestTables, chunk_rows_of, document_rows, index_handing_out,
    test_embedding,
};
use crate::query::{
    IndexedQuery, Query, QueryError, SearchMode, SearchRequest, SearchResult, SearchUnit, create,
};
use crate::test_support::log_receiver::capture_async;
use crate::types::{ChunkEmbedding, CollectionName, EmbeddingModel, EmbeddingVector};

const NOTES: &str = "notes";
/// The largest limit of the external specification, or the largest `usize`
/// where that is smaller.
fn largest_limit() -> usize {
    usize::try_from(9_007_199_254_740_991_u64).unwrap_or(usize::MAX)
}

use SearchMode::{FullText, Hybrid, Vector};
use SearchUnit::{Chunk, Document};

fn request(query: &str, mode: SearchMode, unit: SearchUnit) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        mode,
        collections: Vec::new(),
        limit: 20,
        unit,
    }
}

fn document(key: &str, chunks: &[&str]) -> TestDocument {
    TestDocument::from_chunks(NOTES, key, chunks)
}

fn query_over(tables: &TestTables) -> Arc<dyn Query> {
    create(Arc::new(test_embedding()), Arc::new(tables.index()))
}

async fn search(tables: &TestTables, request: SearchRequest) -> SearchResult {
    query_over(tables).search(request).await.unwrap()
}

fn query_with_other_model(tables: &TestTables) -> Arc<dyn Query> {
    let embedding = TestEmbedding::new("other-model", 8);
    create(Arc::new(embedding), Arc::new(tables.index()))
}

fn keys(result: &SearchResult) -> Vec<&str> {
    result.hits.iter().map(|hit| hit.key.0.as_str()).collect()
}

fn scores(result: &SearchResult) -> Vec<f64> {
    result.hits.iter().map(|hit| hit.score).collect()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-5,
        "{actual} is not close to {expected}"
    );
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    let length = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    f64::from(1.0 - dot / (length(a) * length(b)))
}

// ---------------------------------------------------------------- full-text search of documents

#[tokio::test]
async fn full_text_search_finds_the_documents_with_a_word_best_first() {
    let tables = TestTables::searchable(&[
        document("many", &["alpha alpha alpha beta"]),
        document("few", &["alpha gamma gamma gamma gamma gamma"]),
        document("none", &["delta epsilon"]),
    ])
    .await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(keys(&result), ["many", "few"]);
    assert!(result.hits[0].score > result.hits[1].score);
    assert!(result.hits[1].score > 0.0);
    assert_eq!(result.warnings, Vec::<String>::new());
}

#[tokio::test]
async fn full_text_search_finds_a_document_with_any_of_the_words() {
    let tables = TestTables::searchable(&[
        document("first", &["alpha"]),
        document("second", &["beta"]),
        document("neither", &["gamma"]),
    ])
    .await;
    let mut result = search(&tables, request("alpha beta", FullText, Document)).await;
    result.hits.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(keys(&result), ["first", "second"]);
}

#[tokio::test]
async fn a_document_hit_shows_the_chunk_with_the_highest_bm25_value() {
    let chunks = &[
        "gamma gamma gamma",
        "alpha alpha alpha",
        "alpha delta delta delta",
    ];
    let tables = TestTables::searchable(&[document("doc", chunks)]).await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    let hit = &result.hits[0];
    assert_eq!(hit.chunk_index, 1);
    assert_eq!(hit.chunk_count, 3);
    assert_eq!(hit.snippet, "alpha alpha alpha");
    assert_eq!(hit.collection, CollectionName(NOTES.to_owned()));
}

#[tokio::test]
async fn chunks_with_the_same_bm25_value_show_the_smallest_number() {
    let tables = TestTables::searchable(&[document("doc", &["alpha beta ", "alpha beta "])]).await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 0);
}

#[tokio::test]
async fn full_text_search_does_not_use_the_embedding() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let embedding = Arc::new(test_embedding());
    let query = create(embedding.clone(), Arc::new(tables.index()));
    query
        .search(request("alpha", FullText, Document))
        .await
        .unwrap();
    assert_eq!(embedding.calls(), Vec::<EmbeddingCall>::new());
}

#[tokio::test]
async fn hits_with_the_same_score_are_in_collection_and_key_order() {
    let same = |collection, key| TestDocument::from_chunks(collection, key, &["alpha beta"]);
    let tables = TestTables::searchable(&[same("b", "a"), same("a", "z"), same("a", "b")]).await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    let order: Vec<(&str, &str)> = result
        .hits
        .iter()
        .map(|hit| (hit.collection.0.as_str(), hit.key.0.as_str()))
        .collect();
    assert_eq!(order, [("a", "b"), ("a", "z"), ("b", "a")]);
}

#[tokio::test]
async fn chunks_with_the_same_score_are_in_the_order_of_their_numbers() {
    let tables = TestTables::searchable(&[document("doc", &["alpha beta ", "alpha beta "])]).await;
    let result = search(&tables, request("alpha", FullText, Chunk)).await;
    let numbers: Vec<usize> = result.hits.iter().map(|hit| hit.chunk_index).collect();
    assert_eq!(numbers, [0, 1]);
}

#[tokio::test]
async fn the_limit_keeps_the_first_of_the_hits_with_the_same_score() {
    let same = |key| document(key, &["alpha beta"]);
    let tables = TestTables::searchable(&[same("d"), same("c"), same("b"), same("a")]).await;
    for unit in [Document, Chunk] {
        let mut one = request("alpha", FullText, unit);
        one.limit = 1;
        assert_eq!(keys(&search(&tables, one).await), ["a"], "{unit:?}");
    }
}

#[tokio::test]
async fn a_hybrid_search_keeps_the_first_of_the_hits_tied_at_the_limit() {
    // "z" is first in the full-text search and second in the vector search,
    // "a" the other way around, so their scores are the same. "c" is found by
    // the vector search alone.
    let tables = TestTables::searchable(&[
        document("z", &["alpha alpha"]),
        document("a", &["alpha"]),
        document("c", &["unrelated words"]),
    ])
    .await;
    for unit in [Document, Chunk] {
        let mut one = request("alpha", Hybrid, unit);
        one.limit = 1;
        let result = search(&tables, one).await;
        assert_eq!(keys(&result), ["a"], "{unit:?}");
        assert_close(result.hits[0].score, 1.0 / 60.0 + 1.0 / 61.0);
    }
}

#[tokio::test]
async fn disagreeing_chunks_do_not_keep_the_chunk_with_the_words_from_being_shown() {
    let documents = [document(
        "doc",
        &["alpha one two three four five six ", "alpha alpha"],
    )];
    let mut rows = chunk_rows_of(&documents);
    rows.extend(
        (10..13).map(|index| TestChunkRow::new(NOTES, "doc", index, (0, 5), "alpha alpha alpha")),
    );
    let tables = TestTables::searchable_with(&documents, &rows, "test-model").await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 1);
}

#[tokio::test]
async fn disagreeing_chunks_do_not_keep_the_nearest_chunk_from_being_shown() {
    let embedding = test_embedding();
    let query_vector = embedding.vector_for("needle");
    let distance = |text: &str| cosine_distance(&query_vector, &embedding.vector_for(text));
    // The nearest chunk is not the first one, which a document falls back to.
    let chunks = if distance("gamma one") < distance("delta two") {
        ["delta two", "gamma one"]
    } else {
        ["gamma one", "delta two"]
    };
    let documents = [document("doc", &chunks)];
    let mut rows = chunk_rows_of(&documents);
    rows.extend((10..13).map(|index| TestChunkRow::new(NOTES, "doc", index, (0, 6), "needle")));
    let tables = TestTables::searchable_with(&documents, &rows, "test-model").await;
    let result = search(&tables, request("needle", Vector, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 1);
    assert_eq!(result.hits[0].snippet, chunks[1]);
}

#[tokio::test]
async fn a_document_hit_without_chunks_shows_an_empty_snippet() {
    let without_chunks = TestDocument::new(NOTES, "doc", "alpha", &[]);
    let other = document("other", &["gamma"]);
    let tables = TestTables::searchable(&[without_chunks, other]).await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(result.hits[0].chunk_count, 0);
    assert_eq!(result.hits[0].chunk_index, 0);
    assert_eq!(result.hits[0].snippet, "");
}

#[tokio::test]
async fn a_stored_chunk_range_outside_the_body_cannot_be_read() {
    let broken = TestDocument::new(NOTES, "doc", "alpha", &[(0, 99)]);
    let tables = TestTables::searchable(&[broken]).await;
    let result = query_over(&tables)
        .search(request("alpha", FullText, Document))
        .await;
    assert!(
        matches!(result, Err(QueryError::CannotRead { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn searches_only_the_named_collections_whatever_their_names_contain() {
    let tables = TestTables::searchable(&[
        TestDocument::from_chunks("c'2", "quoted", &["alpha"]),
        TestDocument::from_chunks("plain", "other", &["alpha"]),
        TestDocument::from_chunks("third", "named", &["alpha"]),
    ])
    .await;
    for (mode, unit) in [(FullText, Document), (Hybrid, Chunk), (Vector, Document)] {
        let mut named = request("alpha", mode, unit);
        named.collections = vec![
            CollectionName("c'2".to_owned()),
            CollectionName("third".to_owned()),
        ];
        let mut result = search(&tables, named).await;
        result.hits.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(keys(&result), ["named", "quoted"], "{mode:?} {unit:?}");
    }
}

#[tokio::test]
async fn returns_no_more_hits_than_the_limit() {
    let tables = TestTables::searchable(&[
        document("a", &["alpha"]),
        document("b", &["alpha"]),
        document("c", &["alpha"]),
    ])
    .await;
    for (mode, unit) in [(FullText, Document), (Vector, Chunk), (Hybrid, Document)] {
        let mut limited = request("alpha", mode, unit);
        limited.limit = 2;
        assert_eq!(search(&tables, limited).await.hits.len(), 2, "{mode:?}");
    }
}

#[tokio::test]
async fn the_largest_limit_returns_every_hit() {
    let tables =
        TestTables::searchable(&[document("a", &["alpha"]), document("b", &["alpha beta"])]).await;
    for mode in [FullText, Vector, Hybrid] {
        for unit in [Document, Chunk] {
            let mut largest = request("alpha", mode, unit);
            largest.limit = largest_limit();
            let result = search(&tables, largest).await;
            assert_eq!(result.hits.len(), 2, "{mode:?} {unit:?}");
        }
    }
}

#[tokio::test]
async fn a_search_that_matches_nothing_has_no_hits() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let result = search(&tables, request("absent", FullText, Document)).await;
    assert_eq!(keys(&result), Vec::<&str>::new());
    let mut nothing_in_it = request("absent", Hybrid, Chunk);
    nothing_in_it.collections = vec![CollectionName("empty".to_owned())];
    assert_eq!(search(&tables, nothing_in_it).await.hits.len(), 0);
}

// ---------------------------------------------------------------- vector search of documents

#[tokio::test]
async fn vector_search_scores_a_document_by_its_nearest_chunk() {
    let tables = TestTables::searchable(&[
        document("near", &["other words", "needle"]),
        document("far", &["unrelated text"]),
    ])
    .await;
    let result = search(&tables, request("needle", Vector, Document)).await;
    assert_eq!(keys(&result), ["near", "far"]);
    assert_close(result.hits[0].score, 1.0);
    assert_eq!(result.hits[0].chunk_index, 1);
    assert_eq!(result.hits[0].snippet, "needle");
    assert_eq!(result.hits[0].chunk_count, 2);
}

#[tokio::test]
async fn the_score_of_a_vector_search_is_the_exact_cosine_similarity() {
    let tables = TestTables::searchable(&[document("doc", &["some text"])]).await;
    let embedding = test_embedding();
    let result = search(&tables, request("another query", Vector, Document)).await;
    let expected = 1.0
        - cosine_distance(
            &embedding.vector_for("another query"),
            &embedding.vector_for("some text"),
        );
    assert_close(result.hits[0].score, expected);
}

#[test]
fn a_cosine_similarity_below_zero_is_zero() {
    assert_eq!(Scoring::CosineSimilarity.score(1.5), 0.0);
    assert_eq!(Scoring::CosineSimilarity.score(0.25), 0.75);
}

#[tokio::test]
async fn a_nearest_chunk_without_a_vector_is_not_shown() {
    let the_document = document("doc", &["needle", "other words"]);
    let rows = vec![
        TestChunkRow::new(NOTES, "doc", 0, (0, 6), "needle").without_vector(),
        TestChunkRow::new(NOTES, "doc", 1, (6, 17), "other words"),
    ];
    let tables = TestTables::searchable_with(&[the_document], &rows, "test-model").await;
    let result = search(&tables, request("needle", Vector, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 1);
}

async fn indexed_tables(count: usize) -> TestTables {
    let documents: Vec<TestDocument> = (0..count)
        .map(|i| document(&format!("key-{i}"), &[&format!("word{i} text")]))
        .collect();
    let tables = TestTables::searchable(&documents).await;
    let settings = || {
        let builder = IvfPqIndexBuilder::default().distance_type(DistanceType::Cosine);
        LanceIndex::IvfPq(builder)
    };
    let index = tables.document.create_index(&["vectors"], settings());
    index.execute().await.unwrap();
    let index = tables.chunk.create_index(&["vector"], settings());
    index.execute().await.unwrap();
    tables
}

#[tokio::test]
async fn a_vector_search_over_a_trained_index_returns_exact_similarities() {
    let tables = indexed_tables(300).await;
    let embedding = test_embedding();
    let query_vector = embedding.vector_for("word7 text");
    for unit in [Document, Chunk] {
        let mut limited = request("word7 text", Vector, unit);
        limited.limit = 5;
        let result = search(&tables, limited).await;
        // The index may miss the nearest rows, so only the scores of what it
        // finds are checked.
        assert_eq!(result.hits.len(), 5, "{unit:?}");
        assert!(scores(&result).is_sorted_by(|a, b| a >= b), "{unit:?}");
        for hit in &result.hits {
            let number: usize = hit.key.0["key-".len()..].parse().unwrap();
            let chunk_vector = embedding.vector_for(&format!("word{number} text"));
            assert_close(
                hit.score,
                1.0 - cosine_distance(&query_vector, &chunk_vector),
            );
        }
    }
}

#[tokio::test]
async fn the_largest_limit_works_over_a_trained_index() {
    let tables = indexed_tables(300).await;
    for mode in [FullText, Vector, Hybrid] {
        for unit in [Document, Chunk] {
            let mut largest = request("word7 text", mode, unit);
            largest.limit = largest_limit();
            let result = search(&tables, largest).await;
            assert!(!result.hits.is_empty(), "{mode:?} {unit:?}");
        }
    }
}

// ---------------------------------------------------------------- hybrid search of documents

#[tokio::test]
async fn a_hybrid_search_adds_the_reciprocal_ranks_of_both_sides() {
    let tables = TestTables::searchable(&[
        document("best", &["alpha"]),
        document("words", &["alpha beta gamma delta"]),
    ])
    .await;
    let result = search(&tables, request("alpha", Hybrid, Document)).await;
    assert_eq!(result.hits[0].key.0, "best");
    assert_close(result.hits[0].score, 2.0 / 60.0);
    assert_eq!(result.hits[0].snippet, "alpha");
}

#[tokio::test]
async fn a_document_found_only_by_vector_shows_its_nearest_chunk() {
    let embedding = test_embedding();
    let chunks = ["gamma one", "delta two", "epsilon three"];
    let tables = TestTables::searchable(&[
        document("vector-only", &chunks),
        document("words", &["alpha"]),
    ])
    .await;
    let query_vector = embedding.vector_for("alpha");
    let nearest = (0..chunks.len())
        .min_by(|a, b| {
            let distance =
                |i: usize| cosine_distance(&query_vector, &embedding.vector_for(chunks[i]));
            distance(*a).total_cmp(&distance(*b))
        })
        .unwrap();
    let result = search(&tables, request("alpha", Hybrid, Document)).await;
    let hit = result
        .hits
        .iter()
        .find(|hit| hit.key.0 == "vector-only")
        .unwrap();
    assert_eq!(hit.chunk_index, nearest);
}

#[tokio::test]
async fn a_hybrid_search_prefers_the_chunk_with_the_words_over_the_nearest() {
    let tables = TestTables::searchable(&[document("doc", &["needle other", "beta words"])]).await;
    let result = search(&tables, request("beta", Hybrid, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 1);
}

// ---------------------------------------------------------------- chunk hits

#[tokio::test]
async fn a_chunk_search_returns_each_matching_chunk_as_a_hit() {
    let tables = TestTables::searchable(&[
        document("doc", &["alpha one ", "beta two ", "alpha three"]),
        document("other", &["gamma"]),
    ])
    .await;
    let mut result = search(&tables, request("alpha", FullText, Chunk)).await;
    result.hits.sort_by_key(|hit| hit.chunk_index);
    let shown: Vec<(usize, usize, &str)> = result
        .hits
        .iter()
        .map(|hit| (hit.chunk_index, hit.chunk_count, hit.snippet.as_str()))
        .collect();
    assert_eq!(shown, [(0, 3, "alpha one "), (2, 3, "alpha three")]);
}

#[tokio::test]
async fn a_vector_search_of_chunks_scores_each_chunk() {
    let tables = TestTables::searchable(&[document("doc", &["other words", "needle"])]).await;
    let result = search(&tables, request("needle", Vector, Chunk)).await;
    assert_eq!(result.hits[0].chunk_index, 1);
    assert_close(result.hits[0].score, 1.0);
    assert_eq!(result.hits.len(), 2);
}

#[tokio::test]
async fn a_hybrid_search_of_chunks_adds_both_sides() {
    let tables = TestTables::searchable(&[document("doc", &["alpha", " beta gamma delta"])]).await;
    let result = search(&tables, request("alpha", Hybrid, Chunk)).await;
    assert_eq!(result.hits[0].chunk_index, 0);
    assert_close(result.hits[0].score, 2.0 / 60.0);
}

#[tokio::test]
async fn chunks_that_disagree_with_the_document_table_are_not_hits() {
    let documents = [document("doc", &["alpha words"])];
    let tables = TestTables::searchable(&documents).await;
    tables
        .add_chunk_rows(&[
            // Another number than the document has.
            TestChunkRow::new(NOTES, "doc", 5, (0, 11), "alpha words"),
            // Another range than the document's chunk.
            TestChunkRow::new(NOTES, "doc", 0, (0, 5), "alpha"),
            // Another text than the body holds there.
            TestChunkRow::new(NOTES, "doc", 0, (0, 11), "alpha alpha"),
            // No document at all.
            TestChunkRow::new(NOTES, "gone", 0, (0, 5), "alpha"),
        ])
        .await;
    let result = search(&tables, request("alpha", FullText, Chunk)).await;
    assert_eq!(keys(&result), ["doc"]);
    assert_eq!(result.hits[0].snippet, "alpha words");
}

#[tokio::test]
async fn chunks_that_disagree_do_not_use_up_the_limit() {
    let documents = [document("doc", &["alpha beta gamma delta"])];
    let tables = TestTables::searchable(&documents).await;
    let stale: Vec<TestChunkRow> = (0..4)
        .map(|i| TestChunkRow::new(NOTES, &format!("gone-{i}"), 0, (0, 5), "alpha"))
        .collect();
    tables.add_chunk_rows(&stale).await;
    let mut one = request("alpha", FullText, Chunk);
    one.limit = 1;
    assert_eq!(keys(&search(&tables, one).await), ["doc"]);
}

#[tokio::test]
async fn a_chunk_that_disagrees_is_not_shown_for_a_document() {
    let documents = [document("doc", &["alpha one", "two"])];
    let mut rows = chunk_rows_of(&documents);
    rows.push(TestChunkRow::new(
        NOTES,
        "doc",
        1,
        (9, 12),
        "alpha alpha alpha",
    ));
    let tables = TestTables::searchable_with(&documents, &rows, "test-model").await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 0);
}

#[tokio::test]
async fn a_document_without_a_usable_chunk_shows_its_first_chunk() {
    let documents = [
        document("doc", &["alpha one", " two"]),
        document("other", &["gamma"]),
    ];
    let rows = [TestChunkRow::new(NOTES, "other", 0, (0, 5), "gamma")];
    let tables = TestTables::searchable_with(&documents, &rows, "test-model").await;
    let result = search(&tables, request("alpha", FullText, Document)).await;
    assert_eq!(result.hits[0].chunk_index, 0);
    assert_eq!(result.hits[0].snippet, "alpha one");
}

// ---------------------------------------------------------------- other embedding models

#[tokio::test]
async fn a_vector_search_with_another_model_has_no_hits_and_warns() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let query = query_with_other_model(&tables);
    // While only one receiver exists, tracing asks the receiver of the thread
    // that first reaches a log call whether the call is enabled, and keeps the
    // answer. A test without a receiver reaching this call after the capture
    // began would turn it off. Reaching it before the capture begins makes
    // creating the receiver decide again.
    query
        .search(request("alpha", Vector, Document))
        .await
        .unwrap();
    let (result, logs) = capture_async(query.search(request("alpha", Vector, Document))).await;
    let result = result.unwrap();
    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.warnings.len(), 1);
    let warning = &result.warnings[0];
    assert!(warning.contains("test-model"), "{warning}");
    assert!(warning.contains("other-model"), "{warning}");
    assert!(warning.contains("mdya embedding rebuild"), "{warning}");
    assert!(warning.contains("restart the MCP server"), "{warning}");
    let warned = logs.iter().find(|log| log.level == Level::WARN).unwrap();
    assert_eq!(&warned.message, warning);
}

#[tokio::test]
async fn a_model_of_another_dimension_is_another_model() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let embedding = TestEmbedding::new("test-model", 4);
    let query = create(Arc::new(embedding), Arc::new(tables.index()));
    let result = query.search(request("alpha", Vector, Chunk)).await.unwrap();
    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.warnings.len(), 1);
}

#[tokio::test]
async fn a_full_text_search_with_another_model_does_not_warn() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let result = query_with_other_model(&tables)
        .search(request("alpha", FullText, Document))
        .await
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.warnings, Vec::<String>::new());
}

#[tokio::test]
async fn a_hybrid_search_with_another_model_ranks_the_full_text_hits_alone() {
    let tables = TestTables::searchable(&[
        document("first", &["alpha"]),
        document("second", &["alpha beta gamma delta"]),
    ])
    .await;
    for unit in [Document, Chunk] {
        let result = query_with_other_model(&tables)
            .search(request("alpha", Hybrid, unit))
            .await
            .unwrap();
        assert_eq!(keys(&result), ["first", "second"], "{unit:?}");
        assert_close(scores(&result)[0], 1.0 / 60.0);
        assert_close(scores(&result)[1], 1.0 / 61.0);
        assert_eq!(result.warnings.len(), 1);
    }
}

#[tokio::test]
async fn a_table_without_its_vector_column_has_no_vector_hits() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    tables.chunk.drop_columns(&["vector"]).await.unwrap();
    let vector = search(&tables, request("alpha", Vector, Document)).await;
    assert_eq!(vector.hits.len(), 0);
    assert!(vector.warnings[0].contains("rebuild has not finished"));
    let hybrid = search(&tables, request("alpha", Hybrid, Document)).await;
    assert_eq!(keys(&hybrid), ["doc"]);
    assert_close(hybrid.hits[0].score, 1.0 / 60.0);
}

#[tokio::test]
async fn tables_of_different_models_have_no_vector_hits() {
    let documents = [document("doc", &["alpha"])];
    let rows = chunk_rows_of(&documents);
    let tables = TestTables::searchable_with(&documents, &rows, "other-model").await;
    let result = search(&tables, request("alpha", Vector, Chunk)).await;
    assert_eq!(result.hits.len(), 0);
    assert_eq!(result.warnings.len(), 1);
}

// ---------------------------------------------------------------- what stops a search

#[tokio::test]
async fn an_empty_query_and_a_limit_of_0_are_refused_before_reading_anything() {
    let index = Arc::new(TestIndex::new());
    let embedding = Arc::new(test_embedding());
    let query = create(embedding.clone(), index.clone());
    let empty = query.search(request("  ", Hybrid, Document)).await;
    assert!(matches!(empty, Err(QueryError::EmptyQuery)));
    let mut no_hits_wanted = request("alpha", Hybrid, Document);
    no_hits_wanted.limit = 0;
    let zero = query.search(no_hits_wanted).await;
    assert!(matches!(zero, Err(QueryError::ZeroLimit)));
    assert_eq!(index.calls(), Vec::<IndexCall>::new());
    assert_eq!(embedding.calls(), Vec::<EmbeddingCall>::new());
}

#[tokio::test]
async fn a_query_longer_than_the_model_accepts_stops_before_any_search() {
    let index = Arc::new(TestIndex::new());
    let embedding = test_embedding().failing_with(EmbeddingFailure::QueryExceedsContextLength);
    let query = create(Arc::new(embedding), index.clone());
    for mode in [Vector, Hybrid] {
        let result = query.search(request("alpha", mode, Document)).await;
        assert!(matches!(
            result,
            Err(QueryError::Embedding(
                EmbeddingError::QueryExceedsContextLength
            ))
        ));
    }
    assert_eq!(index.calls(), Vec::<IndexCall>::new());
}

#[tokio::test]
async fn a_model_that_cannot_be_used_stops_a_vector_and_a_hybrid_search() {
    let embedding = test_embedding().failing_with(EmbeddingFailure::ModelUnavailable);
    let query = create(Arc::new(embedding), Arc::new(TestIndex::new()));
    for mode in [Vector, Hybrid] {
        let result = query.search(request("alpha", mode, Chunk)).await;
        assert!(matches!(
            result,
            Err(QueryError::Embedding(
                EmbeddingError::ModelUnavailable { .. }
            ))
        ));
    }
}

/// Embeds a query but cannot say which model it is.
struct EmbeddingWithoutAModel;

#[async_trait]
impl Embedding for EmbeddingWithoutAModel {
    fn model_name(&self) -> &str {
        "unknown"
    }

    fn max_chunks_per_call(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
    }

    async fn model(&self) -> Result<EmbeddingModel, EmbeddingError> {
        Err(EmbeddingError::ModelUnavailable {
            source: "the model cannot be prepared".into(),
        })
    }

    async fn embed_chunks(&self, _: &[&str]) -> Result<Vec<ChunkEmbedding>, EmbeddingError> {
        panic!("a search does not embed chunks")
    }

    async fn embed_query(&self, _: &str) -> Result<EmbeddingVector, EmbeddingError> {
        Ok(vec![0.0; 8])
    }
}

#[tokio::test]
async fn a_model_that_cannot_say_what_it_is_stops_the_search() {
    let query = create(Arc::new(EmbeddingWithoutAModel), Arc::new(TestIndex::new()));
    let result = query.search(request("alpha", Vector, Document)).await;
    assert!(matches!(
        result,
        Err(QueryError::Embedding(
            EmbeddingError::ModelUnavailable { .. }
        ))
    ));
}

#[tokio::test]
async fn an_index_without_tables_has_no_hits() {
    let query = create(Arc::new(test_embedding()), Arc::new(TestIndex::new()));
    for mode in [FullText, Vector, Hybrid] {
        let result = query
            .search(request("alpha", mode, Document))
            .await
            .unwrap();
        assert_eq!(
            result,
            SearchResult {
                hits: Vec::new(),
                warnings: Vec::new()
            }
        );
    }
}

#[tokio::test]
async fn a_failure_to_hand_over_the_tables_is_an_index_error() {
    let index = TestIndex::new().on_tables_for_query(|| {
        Err(IndexError::CannotRead {
            source: "the disk is gone".into(),
        })
    });
    let query = create(Arc::new(test_embedding()), Arc::new(index));
    let result = query.search(request("alpha", FullText, Document)).await;
    assert!(matches!(
        result,
        Err(QueryError::Index(IndexError::CannotRead { .. }))
    ));
}

#[tokio::test]
async fn asks_for_the_tables_once_for_each_search() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let index = Arc::new(tables.index());
    let query = create(Arc::new(test_embedding()), index.clone());
    for mode in [FullText, Vector, Hybrid] {
        query
            .search(request("alpha", mode, Document))
            .await
            .unwrap();
    }
    assert_eq!(index.calls(), vec![IndexCall::TablesForQuery; 3]);
}

async fn clean_up_old_versions(tables: &TestTables, pinned: &QueryTables) {
    let _ = pinned;
    let newer = document_rows(&[document("new", &["new text"])]);
    tables.document.add(newer).execute().await.unwrap();
    tables.document.delete("key = 'doc'").await.unwrap();
    tables.chunk.delete("key = 'doc'").await.unwrap();
    let rows = chunk_rows_of(&[document("new", &["new text"])]);
    tables.add_chunk_rows(&rows).await;
    for table in [&tables.document, &tables.chunk] {
        table.optimize(OptimizeAction::All).await.unwrap();
        let prune = OptimizeAction::Prune {
            older_than: Some(TimeDelta::zero()),
            delete_unverified: Some(true),
            error_if_tagged_old_versions: None,
        };
        table.optimize(prune).await.unwrap();
    }
}

#[tokio::test]
async fn pinned_versions_that_maintenance_cleaned_up_cannot_be_read() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let pinned = tables.pinned().await;
    clean_up_old_versions(&tables, &pinned).await;
    let query = create(
        Arc::new(test_embedding()),
        Arc::new(index_handing_out(pinned)),
    );
    for mode in [FullText, Vector, Hybrid] {
        let result = query.search(request("alpha", mode, Document)).await;
        assert!(
            matches!(result, Err(QueryError::CannotRead { .. })),
            "{mode:?}: {result:?}"
        );
    }
}

fn vector_item() -> Arc<Field> {
    Arc::new(Field::new("item", DataType::Float32, true))
}

fn field_of_model(data_type: DataType) -> Field {
    let metadata = HashMap::from([("mdya.embedding_model".to_owned(), "a-model".to_owned())]);
    Field::new("vector", data_type, true).with_metadata(metadata)
}

fn a_model(dimension: usize) -> Option<EmbeddingModel> {
    Some(EmbeddingModel {
        name: "a-model".to_owned(),
        dimension,
    })
}

#[test]
fn a_vector_column_has_the_model_of_its_metadata_and_type() {
    let vector = DataType::FixedSizeList(vector_item(), 4);
    assert_eq!(field_model(&field_of_model(vector.clone())), a_model(4));
    let vectors = DataType::List(Arc::new(Field::new("item", vector, true)));
    assert_eq!(field_model(&field_of_model(vectors)), a_model(4));
}

#[test]
fn a_column_without_the_model_metadata_has_no_model() {
    let vector = DataType::FixedSizeList(vector_item(), 4);
    assert_eq!(field_model(&Field::new("vector", vector, true)), None);
}

#[test]
fn a_column_that_is_not_a_vector_column_has_no_model() {
    assert_eq!(field_model(&field_of_model(DataType::Utf8)), None);
}

fn strings(values: &[&str]) -> ArrayRef {
    Arc::new(StringArray::from(values.to_vec()))
}

fn score_column() -> ArrayRef {
    Arc::new(Float32Array::from(vec![1.0_f32]))
}

fn message_of<T>(result: Result<Vec<T>, crate::types::BoxError>) -> String {
    match result {
        Ok(_) => panic!("the rows were read"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn rows_without_the_score_column_cannot_be_read() {
    let rows = RecordBatch::try_from_iter([("collection", strings(&["c"]))]).unwrap();
    let message = message_of(scored_chunks(&[rows], Scoring::Bm25));
    assert!(message.contains("_score"), "{message}");
}

#[test]
fn chunk_rows_without_a_number_cannot_be_read() {
    let rows = RecordBatch::try_from_iter([("_score", score_column())]).unwrap();
    let message = message_of(scored_chunks(&[rows], Scoring::Bm25));
    assert!(message.contains("chunk_index"), "{message}");
}

#[test]
fn document_rows_without_a_collection_cannot_be_read() {
    let rows = RecordBatch::try_from_iter([("_score", score_column())]).unwrap();
    let message = message_of(scored_documents(&[rows], Scoring::Bm25));
    assert!(message.contains("collection"), "{message}");
}

#[test]
fn batches_without_rows_are_not_read() {
    let no_columns = RecordBatch::new_empty(Arc::new(Schema::empty()));
    assert_eq!(
        scored_chunks(std::slice::from_ref(&no_columns), Scoring::Bm25)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        scored_documents(&[no_columns], Scoring::Bm25)
            .unwrap()
            .len(),
        0
    );
}

fn settings_of_a_tokenizer_that_does_not_exist() -> FtsIndexBuilder {
    FtsIndexBuilder::default().base_tokenizer("lindera/no-such-dictionary".to_owned())
}

#[tokio::test]
async fn a_tokenizer_that_cannot_be_built_is_an_index_that_cannot_be_read() {
    let tables = TestTables::searchable(&[document("doc", &["alpha"])]).await;
    let query = IndexedQuery::new(
        Arc::new(test_embedding()),
        Arc::new(tables.index()),
        settings_of_a_tokenizer_that_does_not_exist,
    );
    for mode in [FullText, Vector, Hybrid] {
        let result = query.search(request("alpha", mode, Document)).await;
        assert!(
            matches!(result, Err(QueryError::CannotRead { .. })),
            "{mode:?}: {result:?}"
        );
    }
}
