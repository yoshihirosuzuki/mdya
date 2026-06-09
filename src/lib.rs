//! mdya library crate. Re-exports module entry points so binary and tests
//! share the same surface.

pub mod chunking;
pub mod cli;
pub mod config;
pub mod embedding;
pub mod extract;
pub mod format;
pub mod get;
pub mod ingest;
pub mod introspect;
pub mod mcp;
pub mod runtime;
pub mod search;
pub mod store;
