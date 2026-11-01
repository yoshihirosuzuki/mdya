//! mdya: local retrieval over your own documents, from the command line or as
//! an MCP server.

// Clippy has no setting that allows `unimplemented!` in tests, so allow it for
// the library's test build. The non-test build still checks the code outside
// tests.
#![cfg_attr(test, allow(clippy::unimplemented))]

pub mod gateway;
