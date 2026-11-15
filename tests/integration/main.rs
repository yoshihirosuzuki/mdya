//! The integration tests. They are one crate, so that they are linked with
//! mdya into one executable rather than into one for each file.

mod collection_operations;
mod collection_sources;
mod command_line;
mod common;
mod concurrent_operations;
mod embedding_model_change;
mod index_and_settings;
mod ingest_operations;
mod interruption;
mod mcp_server;
mod memory_limit;
mod query_operations;
mod running_environment;
mod shared_parts;
