//! Test doubles for the components that only `gateway` uses: `config`, the
//! memory watchdog, `collection`, `query`, and the ingest implementations.
//! The doubles of `embedding`, `index`, and `format`, which other components
//! use too, are in those components' modules. Also a writer that the tests
//! of several submodules read back.

pub(crate) mod captured;
pub(crate) mod collections;
pub(crate) mod config;
pub(crate) mod ingest;
pub(crate) mod query;
pub(crate) mod watchdog;
