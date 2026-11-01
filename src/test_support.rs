//! Shared setup for tests. The log receiver test double is in
//! [`log_receiver`]; the rest of this module prepares full-text search for the
//! tests that use it.
//!
//! LanceDB reads the full-text search dictionary configuration from the
//! directory in `LANCE_LANGUAGE_MODEL_HOME`. Tests never set that variable:
//! `.cargo/config.toml` sets it for every process cargo runs, to one directory
//! that all tests share. Before a test creates a full-text search index, it
//! calls [`prepare_full_text_search`], which places the configuration in that
//! directory once per process. A test crate under `tests/` cannot use this
//! module; it does the same with `mdya::index::place_fts_dictionary_config` and
//! the parent directory of `env!("LANCE_LANGUAGE_MODEL_HOME")`.

use std::path::Path;

use tokio::sync::OnceCell;

use crate::index::{DICTIONARY_CONFIG, place_fts_dictionary_config};

pub(crate) mod log_receiver;

static PREPARED: OnceCell<()> = OnceCell::const_new();

/// The data directory whose language model directory `.cargo/config.toml`
/// points `LANCE_LANGUAGE_MODEL_HOME` to. The value is read at compile time,
/// because tests do not read environment variables at run time.
pub(crate) fn full_text_search_data_dir() -> &'static Path {
    Path::new(env!("LANCE_LANGUAGE_MODEL_HOME"))
        .parent()
        .expect("LANCE_LANGUAGE_MODEL_HOME names a directory inside the data directory")
}

/// Places the full-text search dictionary configuration where LanceDB reads it
/// in tests. Only the first call in a process places it. The directory outlives
/// the test run and an existing file is kept, so the call fails when the file
/// there differs from the current configuration, instead of testing with an
/// old one.
pub(crate) async fn prepare_full_text_search() {
    PREPARED
        .get_or_init(|| async {
            let data_dir = full_text_search_data_dir();
            place_fts_dictionary_config(data_dir)
                .await
                .expect("the dictionary configuration for tests can be placed");
            let path = Path::new(env!("LANCE_LANGUAGE_MODEL_HOME"))
                .join("lindera")
                .join("ipadic")
                .join("config.yml");
            let placed = std::fs::read_to_string(&path).unwrap();
            assert!(
                placed == DICTIONARY_CONFIG,
                "{} differs from the current dictionary configuration; delete it and run the tests again",
                path.display()
            );
        })
        .await;
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Arc;

    use arrow_array::{RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use futures::TryStreamExt;
    use lancedb::index::Index;
    use lancedb::index::scalar::FullTextSearchQuery;
    use lancedb::query::{ExecutableQuery, QueryBase};

    use super::*;
    use crate::index::{fts_index_params, process_environment};

    #[test]
    fn cargo_points_lance_where_the_index_component_would_for_the_test_data_directory() {
        let environment = process_environment(full_text_search_data_dir());

        // Compared as paths: on Windows, cargo's value mixes `\` and `/`.
        assert_eq!(environment.len(), 1);
        assert_eq!(
            environment[0].0,
            OsString::from("LANCE_LANGUAGE_MODEL_HOME")
        );
        assert_eq!(
            Path::new(&environment[0].1),
            Path::new(env!("LANCE_LANGUAGE_MODEL_HOME"))
        );
    }

    #[tokio::test]
    async fn a_full_text_search_index_finds_japanese_text_after_the_preparation() {
        prepare_full_text_search().await;
        let directory = tempfile::tempdir().unwrap();
        let db = lancedb::connect(directory.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, false)]));
        let texts = StringArray::from(vec!["東京都に住んでいます", "大阪の天気は晴れです"]);
        let batch = RecordBatch::try_new(schema, vec![Arc::new(texts)]).unwrap();
        let table = db.create_table("t", batch).execute().await.unwrap();
        table
            .create_index(&["text"], Index::FTS(fts_index_params()))
            .execute()
            .await
            .unwrap();

        let results: Vec<RecordBatch> = table
            .query()
            .full_text_search(FullTextSearchQuery::new("東京".to_owned()))
            .execute()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();

        let hits: Vec<String> = results
            .iter()
            .flat_map(|batch| {
                let texts = batch
                    .column_by_name("text")
                    .unwrap()
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap();
                texts
                    .iter()
                    .map(|text| text.unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(hits, vec!["東京都に住んでいます".to_owned()]);
    }
}
