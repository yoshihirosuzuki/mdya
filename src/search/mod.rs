//! Search module.

pub mod engine;
pub mod error;
pub mod output;
pub mod request;
pub mod response;
pub mod snippet;

pub use engine::SearchEngine;
pub use error::SearchError;
pub use output::{print_human, print_json, print_md, print_xml};
pub use request::SearchRequest;
pub use response::{SearchHit, SearchLevel, SearchMode, SearchResponse};
pub use snippet::{DEFAULT_SNIPPET_CHARS, extract_snippet, extract_snippet_head};
