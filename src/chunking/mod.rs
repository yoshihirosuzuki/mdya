//! Chunking module — turns extracted text into `Vec<Chunk>` ready for the
//! ingest writer to combine with `(collection, path, chunk_sequence)`
//! and persist. Markdown ([`chunk_markdown`]) and PDF ([`chunk_pdf`])
//! paths are dispatched by [`crate::format::FileFormat::chunk`]. Both
//! share [`WINDOW_CHARS`] / [`OVERLAP_CHARS`] and the [`placeholder_chunk`]
//! helper so the empty-input contract stays uniform across formats.
//!
//! Design choices:
//!
//! - heading boundary: every heading level (h1–h6) starts a new chunk
//! - unit: chars (Unicode-safe via `str::chars`)
//! - body segmentation: a section's body is collected as block-level
//!   segments (paragraphs, thematic breaks, fenced code blocks). On
//!   overflow the segments are packed greedily into chunks of at most 700
//!   chars at segment boundaries, with no inter-chunk overlap; only a
//!   single segment larger than the window falls back to a 700 / 70-char
//!   split. 700 stays a hard upper bound
//! - chunk body: the heading text followed by its section text. Heading
//!   words are thus searchable via both FTS and vector embedding.
//!   Headings carry only their own (leaf) text, not an ancestor
//!   breadcrumb
//! - heading with empty body: still emitted as a chunk whose body is the
//!   heading text, so section names (often the document's most important
//!   words) stay searchable
//! - empty result (empty / whitespace / front-matter-only document): one
//!   placeholder chunk (empty body) so every file owns >=1 chunk row.
//!   `body.is_empty()` uniquely marks the placeholder; the ingest writer
//!   stores it with a null embedding so it stays out of the vector
//!   index. A headings-only document is not a placeholder — each heading
//!   emits a real chunk
//! - front matter (`---\n…\n---` or `---\n…\n...` at doc head): stripped
//!   via pulldown-cmark's metadata block parsing
//! - fenced code block: kept as one atomic segment — its contents stay in
//!   the body verbatim (searchable via FTS / vector), `#` lines inside the
//!   fence do not open new chunks, and packing never splits a fence across
//!   chunks unless the fence alone exceeds the window
//!
//! These rules are pinned in code only; no chunking knob lives in the
//! YAML, so altering any of them in a future release is a soft change
//! communicated via the changelog.
//!
//! This module is pure-text and stateless; embedding model and
//! vector_dim pinning live elsewhere.

mod error;
mod markdown;
mod pdf;

pub use error::ChunkingError;
pub use markdown::chunk_markdown;
pub use pdf::chunk_pdf;

/// One emitted chunk. `chunk_sequence` is **not** here — the caller
/// (the ingest writer) assigns it. This keeps the chunker stateless
/// across files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk text: the section's heading (if any) followed by its body,
    /// rendered to plain text with Markdown formatting stripped. An
    /// empty body marks the placeholder (no chunkable content).
    pub body: String,
}

/// Window size in **chars**. A section whose body exceeds this is packed
/// into chunks at block-segment boundaries; only a single segment larger
/// than this is split into successive sub-chunks with [`OVERLAP_CHARS`]
/// chars overlap. 700 sets the *retrieval granularity* — smaller windows
/// keep each embedding / FTS hit tightly scoped. It is not bounded by the
/// embedding model: ruri-v3-30m (ModernBERT-Ja) handles 8192 tokens, so 700
/// chars sits well within capacity (see `embedding::ruri_v3`, which sets no
/// truncation).
pub const WINDOW_CHARS: usize = 700;

/// Overlap between sub-chunks when a single block segment exceeds
/// [`WINDOW_CHARS`] and must be char-split. 10 % of the window, matching
/// common practice (e.g. LangChain's default). Segment-boundary packing
/// adds no overlap; this applies only to the oversized-segment fallback.
pub const OVERLAP_CHARS: usize = 70;

/// The single chunk emitted for a file with no chunkable content: an
/// empty / whitespace-only / front-matter-only Markdown document, or a
/// PDF whose extracted text is
/// empty (e.g. completely blank pages — see `extract::pdf` for the
/// extractor's actual behaviour on scan-only image PDFs). Every file
/// must own at least one `chunks` row so it has a re-ingest skip marker
/// and a `sources` mirror. The empty body is the marker the ingest
/// writer keys on to store a null embedding (placeholders stay out of
/// the vector index) — a real section never flushes an empty body (a
/// heading-only Markdown section carries the heading text as its body),
/// so `body.is_empty()` uniquely identifies the placeholder. The
/// faithful original text is held in the `sources` table, not here.
pub(super) fn placeholder_chunk() -> Chunk {
    Chunk {
        body: String::new(),
    }
}
