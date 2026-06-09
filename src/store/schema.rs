//! `chunks` table schema.
//!
//! Shared by `mdya init` (table creation, src/cli/init.rs) and the
//! ingest writer (Arrow batch construction, src/ingest/writer.rs).
//! Living in `src/store/` keeps both call sites pointing at a single
//! source of truth — bumping a column type in one place automatically
//! synchronises the other.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, TimeUnit};

use super::metadata_check::{METADATA_KEY_EMBEDDING_MODEL, METADATA_KEY_VECTOR_DIM};

/// LanceDB table name.
pub const CHUNKS_TABLE_NAME: &str = "chunks";

/// LanceDB table name for the full-document `sources` store. One
/// row per ingested file holds the faithful original text that `mdya
/// get` / MCP `get_document` return — the `chunks.body` column is
/// lossy (front matter stripped, section bodies trimmed, overflow
/// sub-chunks overlap), so the raw source cannot be reconstructed from
/// `chunks` and lives here instead.
pub const SOURCES_TABLE_NAME: &str = "sources";

// Column names. `chunks_schema` and every SQL predicate / `Select` /
// FTS / vector `create_index` call site must agree on these strings, so
// pin them here. `modified_at` and `source_hash` are intentionally
// excluded — they are ingest-time bookkeeping that the search layer
// does not project or filter on. `embedding_model` is no longer a
// column: the pin lives in `Schema::metadata` instead.
pub const COL_COLLECTION: &str = "collection";
pub const COL_PATH: &str = "path";
pub const COL_CHUNK_SEQUENCE: &str = "chunk_sequence";
pub const COL_BODY: &str = "body";
pub const COL_EMBEDDING: &str = "embedding";
/// SHA-256 hex of the source file. A `chunks` field (ingest bookkeeping)
/// and the consistency comparator between `chunks` and `sources`:
/// `update-all` only skips re-ingesting a file when this value matches
/// across both tables, so a crash that wrote one table but not the other
/// self-heals on the next run regardless of write order.
pub const COL_SOURCE_HASH: &str = "source_hash";
/// Faithful original document text, only in the `sources` table.
pub const COL_CONTENT: &str = "content";

/// Build the Arrow schema for the `chunks` table. `vector_dim` is the
/// `FixedSizeList<Float32, N>` width and must match the embedder's
/// `dim()` (256 for `cl-nagoya/ruri-v3-30m`).
///
/// `embedding_model` is the declared model id from
/// `config.yml::embedding.model`; it is embedded into the schema as
/// `Schema::metadata["embedding_model"]`. `vector_dim` is
/// also written to `Schema::metadata["vector_dim"]` for symmetry; the
/// `FixedSizeList` is the structural source of truth and rejects
/// mismatching inserts/queries at the Arrow / LanceDB layer.
pub fn chunks_schema(vector_dim: i32, embedding_model: &str) -> Schema {
    let mut metadata = HashMap::new();
    metadata.insert(
        METADATA_KEY_EMBEDDING_MODEL.to_string(),
        embedding_model.to_string(),
    );
    metadata.insert(METADATA_KEY_VECTOR_DIM.to_string(), vector_dim.to_string());
    // 7 columns total; heading text is folded into `body`.
    Schema::new(vec![
        Field::new("collection", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("chunk_sequence", DataType::UInt32, false),
        Field::new("body", DataType::Utf8, false),
        // Nullable: a file with no chunkable body (empty /
        // whitespace / front-matter-only document) still gets one
        // placeholder chunk so every file owns a `chunks` row (uniform
        // skip marker + `sources` mirror). That placeholder carries a
        // null embedding so it never enters the IVF_Flat vector index —
        // empirically verified (lancedb 0.29.0) that null vectors are
        // tolerated at `create_index` time and excluded from
        // `nearest_to` results. `vector_dim` (the FixedSizeList N, the
        // structural pin) is unaffected by nullability.
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                vector_dim,
            ),
            true,
        ),
        Field::new(
            "modified_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("source_hash", DataType::Utf8, false),
    ])
    .with_metadata(metadata)
}

/// Build the Arrow schema for the `sources` table. One row per
/// ingested file: the composite key `(collection, path)` plus the
/// `source_hash` consistency comparator and the faithful original
/// `content`. No embedding / vector column lives here — retrieval is a
/// point lookup, not a search — so the schema carries no
/// `Schema::metadata` pin (the embedding-model pin stays on `chunks`).
pub fn sources_schema() -> Schema {
    Schema::new(vec![
        Field::new(COL_COLLECTION, DataType::Utf8, false),
        Field::new(COL_PATH, DataType::Utf8, false),
        Field::new(COL_SOURCE_HASH, DataType::Utf8, false),
        Field::new(COL_CONTENT, DataType::Utf8, false),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_embedding_is_nullable_for_placeholder_rows() {
        // Placeholder chunks (zero-body files) carry a null embedding
        // so they stay out of the vector index. If this regresses to
        // non-null, the placeholder insert fails at the Arrow layer.
        let schema = chunks_schema(256, "cl-nagoya/ruri-v3-30m");
        let embedding = schema
            .field_with_name(COL_EMBEDDING)
            .expect("embedding column exists");
        assert!(
            embedding.is_nullable(),
            "embedding must be nullable so placeholder chunks can store null"
        );
    }

    #[test]
    fn sources_schema_has_key_hash_and_content_columns() {
        let schema = sources_schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            names,
            vec![COL_COLLECTION, COL_PATH, COL_SOURCE_HASH, COL_CONTENT]
        );
        for name in [COL_COLLECTION, COL_PATH, COL_SOURCE_HASH, COL_CONTENT] {
            let f = schema.field_with_name(name).expect("field exists");
            assert_eq!(f.data_type(), &DataType::Utf8, "{name} is Utf8");
            assert!(!f.is_nullable(), "{name} is non-null");
        }
    }

    #[test]
    fn sources_schema_carries_no_metadata_pin() {
        // The embedding-model / vector_dim pin lives on `chunks` only.
        // `sources` has no vectors, so it must not duplicate the pin.
        assert!(sources_schema().metadata().is_empty());
    }
}
