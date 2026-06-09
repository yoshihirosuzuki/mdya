//! Verify that the `chunks` table's Arrow `Schema::metadata` matches
//! the `embedding.model` + vector dim declared in `config.yml`. Schema
//! metadata is read with one `Table::schema().await?` call and the
//! comparison is a HashMap lookup.
//!
//! Run from `mdya update-all` writer entry and from `SearchEngine::open`
//! exactly once per process. Schema metadata is immutable within a
//! given dataset version, so per-query checks would be redundant.

use std::sync::Arc;

use arrow_schema::Schema;
use lancedb::Table;

/// Schema metadata key for the embedding model id pin. Written by
/// `mdya init` from `config.yml::embedding.model` and re-confirmed by
/// every `update-all` / `search`.
pub const METADATA_KEY_EMBEDDING_MODEL: &str = "embedding_model";

/// Schema metadata key for the vector dimension pin. The same value
/// is also encoded structurally in `FixedSizeList<Float32, N>`, which
/// rejects mismatching inserts / queries at the Arrow / LanceDB layer
/// (`tests/verify_schema_metadata.rs` axes 4 / 5). The metadata key
/// is the human-readable mirror that lets us surface a meaningful
/// mismatch error before the structural reject fires.
pub const METADATA_KEY_VECTOR_DIM: &str = "vector_dim";

/// Result of checking declared vs actual schema metadata pins.
///
/// `Mismatch` and `Missing` are disjoint (a key cannot be both absent
/// and differing). `Missing` takes precedence — see `classify` and the
/// inline tests for the disambiguation rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Mismatch {
        actual_embedding_model: String,
        actual_vector_dim: String,
    },
    Missing {
        absent_keys: Vec<&'static str>,
    },
}

/// Read the `chunks` table's schema metadata and classify it against
/// the declared pins. Thin I/O wrapper around `classify`, so unit tests
/// can target the decision logic without spinning up a LanceDB table.
pub async fn check(
    table: &Table,
    declared_embedding_model: &str,
    declared_vector_dim: i32,
) -> Result<Outcome, lancedb::Error> {
    let schema: Arc<Schema> = table.schema().await?;
    let meta = schema.metadata();
    let actual_model = meta.get(METADATA_KEY_EMBEDDING_MODEL).map(String::as_str);
    let actual_dim = meta.get(METADATA_KEY_VECTOR_DIM).map(String::as_str);
    Ok(classify(
        declared_embedding_model,
        declared_vector_dim,
        actual_model,
        actual_dim,
    ))
}

fn classify(
    declared_model: &str,
    declared_dim: i32,
    actual_model: Option<&str>,
    actual_dim: Option<&str>,
) -> Outcome {
    let mut absent_keys = Vec::new();
    if actual_model.is_none() {
        absent_keys.push(METADATA_KEY_EMBEDDING_MODEL);
    }
    if actual_dim.is_none() {
        absent_keys.push(METADATA_KEY_VECTOR_DIM);
    }
    if !absent_keys.is_empty() {
        return Outcome::Missing { absent_keys };
    }
    let am = actual_model.expect("checked absent_keys above");
    let ad = actual_dim.expect("checked absent_keys above");
    if am == declared_model && ad == declared_dim.to_string() {
        Outcome::Pass
    } else {
        Outcome::Mismatch {
            actual_embedding_model: am.to_string(),
            actual_vector_dim: ad.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_declared_and_actual_returns_pass() {
        assert_eq!(
            classify("ruri", 256, Some("ruri"), Some("256")),
            Outcome::Pass
        );
    }

    #[test]
    fn different_model_returns_mismatch() {
        assert_eq!(
            classify("ruri", 256, Some("other"), Some("256")),
            Outcome::Mismatch {
                actual_embedding_model: "other".to_string(),
                actual_vector_dim: "256".to_string(),
            }
        );
    }

    #[test]
    fn different_dim_returns_mismatch() {
        assert_eq!(
            classify("ruri", 256, Some("ruri"), Some("128")),
            Outcome::Mismatch {
                actual_embedding_model: "ruri".to_string(),
                actual_vector_dim: "128".to_string(),
            }
        );
    }

    #[test]
    fn missing_embedding_model_returns_missing_with_that_key() {
        assert_eq!(
            classify("ruri", 256, None, Some("256")),
            Outcome::Missing {
                absent_keys: vec![METADATA_KEY_EMBEDDING_MODEL],
            }
        );
    }

    #[test]
    fn missing_vector_dim_returns_missing_with_that_key() {
        assert_eq!(
            classify("ruri", 256, Some("ruri"), None),
            Outcome::Missing {
                absent_keys: vec![METADATA_KEY_VECTOR_DIM],
            }
        );
    }

    #[test]
    fn missing_both_returns_missing_with_both_keys() {
        assert_eq!(
            classify("ruri", 256, None, None),
            Outcome::Missing {
                absent_keys: vec![METADATA_KEY_EMBEDDING_MODEL, METADATA_KEY_VECTOR_DIM],
            }
        );
    }

    #[test]
    fn missing_takes_precedence_over_mismatch_when_only_one_key_present() {
        // Structurally `actual_dim = Some(...)` with `actual_model = None`
        // cannot disagree with `declared_model` because we never reach the
        // Mismatch branch; document the precedence here so future readers
        // don't try to "fix" it by merging Missing into Mismatch.
        assert_eq!(
            classify("ruri", 256, None, Some("other-dim")),
            Outcome::Missing {
                absent_keys: vec![METADATA_KEY_EMBEDDING_MODEL],
            }
        );
    }
}
