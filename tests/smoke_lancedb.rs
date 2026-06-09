//! LanceDB smoke: open db → create table with a text column → build FTS
//! index → insert 1 row → BM25 search hits the row. Verifies the lancedb
//! foundation wire-up across all 3 supported platforms.

use std::sync::Arc;

use anyhow::Result;
use arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::query::{ExecutableQuery, QueryBase};

use mdya::store;

#[tokio::test]
async fn lancedb_bm25_roundtrip() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let db = store::connect(tmp.path()).await?;

    let schema = Arc::new(Schema::new(vec![Field::new("body", DataType::Utf8, false)]));
    let body = Arc::new(StringArray::from(vec!["hello world"]));
    let batch = RecordBatch::try_new(schema.clone(), vec![body])?;
    let batches = RecordBatchIterator::new(vec![Ok(batch)], schema.clone());
    let reader: Box<dyn RecordBatchReader + Send> = Box::new(batches);

    let tbl = db.create_table("docs", reader).execute().await?;
    tbl.create_index(&["body"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;

    let stream = tbl
        .query()
        .full_text_search(FullTextSearchQuery::new("hello".into()))
        .limit(10)
        .execute()
        .await?;
    let hits: Vec<RecordBatch> = stream.try_collect().await?;

    let row_count: usize = hits.iter().map(|b| b.num_rows()).sum();
    assert!(
        row_count >= 1,
        "expected at least one BM25 hit, got {row_count}"
    );
    Ok(())
}
