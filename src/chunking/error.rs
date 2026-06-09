//! Error type for the chunking module.
//!
//! The chunker is pure-text processing — it does not touch I/O, so error
//! variants stay narrow. Variants will grow only when concrete failure
//! modes are discovered during integration.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChunkingError {
    /// Reserved for input shapes the chunker cannot currently handle. Today
    /// there is no concrete trigger — pulldown-cmark accepts any UTF-8 input
    /// — but keeping the variant lets callers `?`-propagate without churn
    /// when stricter validation lands later (e.g. encoding sniffing).
    #[error("invalid markdown input: {reason}")]
    InvalidInput { reason: String },
}
