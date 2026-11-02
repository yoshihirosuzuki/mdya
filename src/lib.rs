//! mdya: local retrieval over your own documents, from the command line or as
//! an MCP server.

// Clippy has no setting that allows `unimplemented!` in tests, so allow it for
// the library's test build. The non-test build still checks the code outside
// tests.
#![cfg_attr(test, allow(clippy::unimplemented))]

pub mod archive_ingest;
pub mod collection;
pub mod config;
pub mod embedding;
pub mod file_ingest;
pub mod format;
pub mod gateway;
pub mod index;
pub mod ingest;
pub mod path;
pub mod query;
pub mod raw_ingest;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;
pub mod watchdog;
