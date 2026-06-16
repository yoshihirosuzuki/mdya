//! Structured error returned by every MCP tool: the `Err` branch is
//! a typed JSON body so clients can branch on a stable code instead
//! of regex-scraping `message`.
//!
//! The tool methods return `Result<Json<T>, Json<McpToolError>>`. rmcp
//! serialises the `Err` branch into a `CallToolResult` with `is_error:
//! true` and the JSON in `structured_content` (its `content` text block
//! holds the same JSON for backwards compatibility). A client can branch
//! on the stable [`McpErrorCode`] and read machine-extractable context
//! from [`McpToolError::details`] instead of regex-scraping `message` —
//! the MCP spec (2025-06-18) classes invalid-input / business-logic
//! failures as *tool execution errors*, not JSON-RPC protocol errors.
//!
//! The CLI keeps the `anyhow` `Caused by:` chain (source shared,
//! sink differs): this projection lives in the MCP layer so
//! `search` / `get` stay transport-agnostic.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::get::GetError;
use crate::introspect::IntrospectError;
use crate::search::SearchError;

/// Stable, machine-branchable error code. Only distinctions a client can
/// act on differently earn their own code; operational failures (LanceDB
/// connect/query, embedding, config load, non-UTF-8 paths) collapse into
/// [`McpErrorCode::Internal`] because the request that triggered them
/// cannot be fixed by changing the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorCode {
    EmptyQuery,
    InvalidLimit,
    UnknownCollection,
    NotFound,
    /// A full `get_document` response exceeded `get.mcp_max_bytes`. The
    /// client cannot retry around it (there is no MCP bypass), but it can
    /// read `details.size_bytes` / `details.limit_bytes` to decide whether
    /// to narrow the request (e.g. fetch a single `chunk` instead).
    PayloadTooLarge,
    SchemaMetadataMissing,
    Internal,
}

/// Structured error body placed in a tool result's `structured_content`.
/// `message` is the source error's `Display` (the same sentence the CLI
/// prints); `details` carries the per-code context a client would
/// otherwise have to parse out of `message`, and is omitted when the code
/// needs no extra context.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct McpToolError {
    pub code: McpErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl From<SearchError> for McpToolError {
    fn from(err: SearchError) -> Self {
        let message = err.to_string();
        let (code, details) = match &err {
            SearchError::EmptyQuery => (McpErrorCode::EmptyQuery, None),
            SearchError::InvalidLimit => (McpErrorCode::InvalidLimit, None),
            SearchError::UnknownCollection { name } => (
                McpErrorCode::UnknownCollection,
                Some(json!({ "collection": name })),
            ),
            SearchError::SchemaMetadataMissing { absent_keys } => (
                McpErrorCode::SchemaMetadataMissing,
                Some(json!({ "absent_keys": absent_keys })),
            ),
            SearchError::LancedbPathNotUtf8 { .. }
            | SearchError::LancedbConnect { .. }
            | SearchError::OpenChunksTable(_)
            | SearchError::Query(_)
            | SearchError::Embed(_)
            | SearchError::Config(_) => (McpErrorCode::Internal, None),
        };
        Self {
            code,
            message,
            details,
        }
    }
}

impl From<GetError> for McpToolError {
    fn from(err: GetError) -> Self {
        let message = err.to_string();
        let (code, details) = match &err {
            GetError::UnknownCollection { name } => (
                McpErrorCode::UnknownCollection,
                Some(json!({ "collection": name })),
            ),
            GetError::NotFound { collection, path } => (
                McpErrorCode::NotFound,
                Some(json!({ "collection": collection, "path": path })),
            ),
            GetError::ChunkNotFound {
                collection,
                path,
                chunk_sequence,
            } => (
                McpErrorCode::NotFound,
                Some(json!({
                    "collection": collection,
                    "path": path,
                    "chunk_sequence": chunk_sequence,
                })),
            ),
            GetError::DocumentTooLarge {
                size_bytes,
                limit_bytes,
            } => (
                McpErrorCode::PayloadTooLarge,
                Some(json!({ "size_bytes": size_bytes, "limit_bytes": limit_bytes })),
            ),
            GetError::Config(_)
            | GetError::LancedbPathNotUtf8 { .. }
            | GetError::LancedbConnect { .. }
            | GetError::OpenSourcesTable(_)
            | GetError::QuerySources(_)
            | GetError::OpenChunksTable(_)
            | GetError::QueryChunks(_) => (McpErrorCode::Internal, None),
        };
        Self {
            code,
            message,
            details,
        }
    }
}

impl From<IntrospectError> for McpToolError {
    fn from(err: IntrospectError) -> Self {
        let message = err.to_string();
        let (code, details) = match &err {
            IntrospectError::SchemaMetadataMissing { absent_keys } => (
                McpErrorCode::SchemaMetadataMissing,
                Some(json!({ "absent_keys": absent_keys })),
            ),
            IntrospectError::Config(_)
            | IntrospectError::LancedbPathNotUtf8 { .. }
            | IntrospectError::LancedbConnect { .. }
            | IntrospectError::OpenTable { .. }
            | IntrospectError::Query { .. }
            | IntrospectError::Schema(_)
            | IntrospectError::MalformedVectorDimPin { .. } => (McpErrorCode::Internal, None),
        };
        Self {
            code,
            message,
            details,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_maps_to_code_without_details() {
        let err = McpToolError::from(SearchError::EmptyQuery);
        assert_eq!(err.code, McpErrorCode::EmptyQuery);
        assert_eq!(err.message, "query must be non-empty");
        assert!(err.details.is_none());
    }

    #[test]
    fn invalid_limit_maps_to_code_without_details() {
        let err = McpToolError::from(SearchError::InvalidLimit);
        assert_eq!(err.code, McpErrorCode::InvalidLimit);
        assert!(err.details.is_none());
    }

    #[test]
    fn unknown_collection_carries_collection_in_details() {
        let err = McpToolError::from(SearchError::UnknownCollection {
            name: "ghost".to_string(),
        });
        assert_eq!(err.code, McpErrorCode::UnknownCollection);
        assert_eq!(err.details, Some(json!({ "collection": "ghost" })));
    }

    #[test]
    fn schema_metadata_missing_carries_absent_keys_in_details() {
        let err = McpToolError::from(SearchError::SchemaMetadataMissing {
            absent_keys: vec!["embedding_model"],
        });
        assert_eq!(err.code, McpErrorCode::SchemaMetadataMissing);
        assert_eq!(
            err.details,
            Some(json!({ "absent_keys": ["embedding_model"] }))
        );
    }

    #[test]
    fn get_not_found_carries_locator_in_details() {
        let err = McpToolError::from(GetError::NotFound {
            collection: "notes".to_string(),
            path: "ghost.md".to_string(),
        });
        assert_eq!(err.code, McpErrorCode::NotFound);
        assert_eq!(
            err.details,
            Some(json!({ "collection": "notes", "path": "ghost.md" }))
        );
    }

    #[test]
    fn get_unknown_collection_carries_collection_in_details() {
        let err = McpToolError::from(GetError::UnknownCollection {
            name: "ghost".to_string(),
        });
        assert_eq!(err.code, McpErrorCode::UnknownCollection);
        assert_eq!(err.details, Some(json!({ "collection": "ghost" })));
    }

    #[test]
    fn get_chunk_not_found_carries_locator_and_sequence_in_details() {
        let err = McpToolError::from(GetError::ChunkNotFound {
            collection: "notes".to_string(),
            path: "release.md".to_string(),
            chunk_sequence: 7,
        });
        assert_eq!(err.code, McpErrorCode::NotFound);
        assert_eq!(
            err.details,
            Some(json!({
                "collection": "notes",
                "path": "release.md",
                "chunk_sequence": 7,
            }))
        );
    }

    #[test]
    fn get_document_too_large_maps_to_payload_too_large_with_byte_details() {
        let err = McpToolError::from(GetError::DocumentTooLarge {
            size_bytes: 2_621_440,
            limit_bytes: 1_048_576,
        });
        assert_eq!(err.code, McpErrorCode::PayloadTooLarge);
        assert_eq!(
            err.details,
            Some(json!({ "size_bytes": 2_621_440, "limit_bytes": 1_048_576 }))
        );
    }

    #[test]
    fn payload_too_large_serialises_as_snake_case_token() {
        assert_eq!(
            serde_json::to_value(McpErrorCode::PayloadTooLarge).unwrap(),
            json!("payload_too_large")
        );
    }

    #[test]
    fn code_serialises_as_snake_case_token() {
        assert_eq!(
            serde_json::to_value(McpErrorCode::SchemaMetadataMissing).unwrap(),
            json!("schema_metadata_missing")
        );
    }

    #[test]
    fn error_without_details_omits_the_field() {
        let err = McpToolError::from(SearchError::EmptyQuery);
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(value.get("details"), None);
    }

    #[test]
    fn introspect_schema_metadata_missing_carries_absent_keys_in_details() {
        let err = McpToolError::from(IntrospectError::SchemaMetadataMissing {
            absent_keys: vec!["vector_dim"],
        });
        assert_eq!(err.code, McpErrorCode::SchemaMetadataMissing);
        assert_eq!(err.details, Some(json!({ "absent_keys": ["vector_dim"] })));
    }

    #[test]
    fn introspect_operational_failure_maps_to_internal() {
        let err = McpToolError::from(IntrospectError::MalformedVectorDimPin {
            value: "xyz".to_string(),
        });
        assert_eq!(err.code, McpErrorCode::Internal);
        assert!(err.details.is_none());
    }
}
