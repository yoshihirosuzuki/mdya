//! Chunking module — turns extracted text into `Vec<Chunk>` ready for the
//! ingest writer to combine with `(collection, path, chunk_sequence)`
//! and persist. Markdown ([`chunk_markdown`]) and PDF ([`chunk_pdf`])
//! paths are dispatched by [`crate::format::FileFormat::chunk`]. Both
//! share [`WINDOW_CHARS`] / [`OVERLAP_CHARS`], the fixed-width split, and
//! the source-range tiling so the range contract stays uniform across
//! formats.
//!
//! Design choices:
//!
//! - heading boundary: every heading level (h1–h6) starts a new chunk
//! - unit: chars (Unicode-safe via `str::chars`)
//! - body segmentation: every block start and block end (paragraph, list
//!   item, block quote, code block, HTML block, thematic break, …) closes
//!   the current segment, and only inline content (text, emphasis, link
//!   text, inline code) and code-block contents are collected as text. List
//!   items therefore never run together, whether or not the list is loose.
//!   On overflow the segments are packed greedily into chunks of at most
//!   700 chars at segment boundaries, with no inter-chunk overlap; only a
//!   single segment larger than the window falls back to a 700 / 70-char
//!   split. 700 stays a hard upper bound
//! - chunk body: the heading text followed by its section text, as plain
//!   text for FTS and embedding. Link targets, image paths, and HTML are
//!   not part of the body. Heading words are thus searchable via both FTS
//!   and vector embedding. Headings carry only their own (leaf) text, not
//!   an ancestor breadcrumb
//! - source range: each chunk records the byte range of the original
//!   document it covers (see [`Chunk::source_range`]). The ranges tile the
//!   document: chunk N runs from its own start to chunk N+1's start, the
//!   first starts at byte 0 and the last ends at the document's end, so
//!   content with no body text (front matter, HTML blocks, image-only
//!   paragraphs) still falls inside some chunk's range. Ranges overlap only
//!   where a single over-window segment was split with overlap. A piece
//!   whose plain text differs from its source (an escape, an entity, inline
//!   code) maps only as a whole, so sub-chunks cut from inside one long such
//!   piece all widen to that piece's range
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
//!   from the body via pulldown-cmark's metadata block parsing
//! - fenced code block: kept as one atomic segment — its contents stay in
//!   the body verbatim (searchable via FTS / vector), `#` lines inside the
//!   fence do not open new chunks, and packing never splits a fence across
//!   chunks unless the fence alone exceeds the window
//! - Markdown documents pulldown-cmark cannot report offsets for
//!   (<https://github.com/pulldown-cmark/pulldown-cmark/issues/1129>):
//!   chunked like a PDF (plain text, 700 / 70-char window, Markdown syntax
//!   and any front matter kept in the body), so the whole document is still
//!   indexed
//!
//! These rules are pinned in code only; no chunking knob lives in the
//! YAML, so altering any of them in a future release is a soft change
//! communicated via the changelog.
//!
//! This module is pure-text and stateless; embedding model and
//! vector_dim pinning live elsewhere.

mod error;
mod mapped_text;
mod markdown;
mod pdf;

use std::ops::Range;

pub use error::ChunkingError;
pub use markdown::chunk_markdown;
pub use pdf::chunk_pdf;

use mapped_text::MappedText;

/// One emitted chunk. `chunk_sequence` is **not** here — the caller
/// (the ingest writer) assigns it. This keeps the chunker stateless
/// across files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk text: the section's heading (if any) followed by its body,
    /// rendered to plain text with Markdown formatting stripped — except
    /// in a document pulldown-cmark cannot report offsets for, which is
    /// split into fixed-width windows with its Markdown syntax and any
    /// front matter kept (see the module docs). An empty body marks the
    /// placeholder (no chunkable content).
    pub body: String,
    /// Byte range of the original document this chunk covers — the same
    /// string the ingest writer stores in the `sources` table, so the range
    /// slices `sources.content` directly. Ranges tile the document (see
    /// the module docs) and always contain the bytes the body was read
    /// from.
    pub source_range: Range<usize>,
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

/// A chunk before its final source range is known: where its first block
/// starts and where its body text ends in the document. [`tile`] turns a
/// document's drafts into chunks whose ranges cover the whole document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftChunk {
    body: String,
    source_start: usize,
    body_end: usize,
}

impl DraftChunk {
    /// Draft from mapped text whose first block starts at `source_start`.
    fn new(text: MappedText, source_start: usize) -> Self {
        let body_end = text.source_end().unwrap_or(source_start).max(source_start);
        Self {
            body: text.into_string(),
            source_start,
            body_end,
        }
    }
}

/// Give every draft its final range so the ranges tile `0..doc_len`: chunk
/// N runs from its start to chunk N+1's start, the first from byte 0 and
/// the last to `doc_len`. Only a split sub-chunk's body can reach past the
/// next start (its overlap), so the end is the further of the next start
/// and the body's own end — the range always contains the body.
///
/// No drafts means the document had no chunkable content: emit the single
/// placeholder covering the whole document.
fn tile(drafts: Vec<DraftChunk>, doc_len: usize) -> Vec<Chunk> {
    if drafts.is_empty() {
        return vec![placeholder_chunk(doc_len)];
    }
    let next_starts: Vec<usize> = drafts
        .iter()
        .skip(1)
        .map(|d| d.source_start)
        .chain(std::iter::once(doc_len))
        .collect();
    drafts
        .into_iter()
        .zip(next_starts)
        .enumerate()
        .map(|(i, (draft, next_start))| {
            let start = if i == 0 { 0 } else { draft.source_start };
            debug_assert!(start <= next_start, "draft starts must not decrease");
            Chunk {
                body: draft.body,
                source_range: start..next_start.max(draft.body_end),
            }
        })
        .collect()
}

/// Split one over-window block into successive sub-chunks of
/// [`WINDOW_CHARS`] chars with [`OVERLAP_CHARS`] chars of overlap. The first
/// sub-chunk starts at `block_start` (the block's own start, e.g. a code
/// fence's opening line); later ones start where their text came from. The
/// walk over `char_indices` is done once (O(n)) so very large blocks stay
/// linear instead of degrading quadratically with repeated
/// `skip(start).take(...)`.
fn split_with_overlap(text: &MappedText, block_start: usize) -> Vec<DraftChunk> {
    let plain = text.as_str();
    let boundaries: Vec<usize> = plain
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(plain.len()))
        .collect();
    let char_count = boundaries.len() - 1;
    let step = WINDOW_CHARS - OVERLAP_CHARS;
    let mut drafts = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + WINDOW_CHARS).min(char_count);
        let piece = text.slice(boundaries[start]..boundaries[end]);
        let source_start = match start {
            0 => block_start,
            _ => text
                .source_start_at(boundaries[start])
                .unwrap_or(block_start),
        };
        drafts.push(DraftChunk::new(piece, source_start));
        if end == char_count {
            return drafts;
        }
        start += step;
    }
}

/// Chunk text with no structure to follow by sliding the window over it.
/// Leading and trailing whitespace is left out of the bodies; the mapped
/// text keeps the trimmed text's offset in `text`, so each chunk's source
/// range still slices the untrimmed original.
fn chunk_plain_text(text: &str) -> Vec<Chunk> {
    let body = MappedText::from_source(text, 0..text.len()).trimmed();
    if body.is_empty() {
        return tile(Vec::new(), text.len());
    }
    let body_start = body.source_start().unwrap_or(0);
    tile(split_with_overlap(&body, body_start), text.len())
}

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
/// so `body.is_empty()` uniquely identifies the placeholder. Its range
/// covers the whole document, so a chunk read still returns the original
/// text.
fn placeholder_chunk(doc_len: usize) -> Chunk {
    Chunk {
        body: String::new(),
        source_range: 0..doc_len,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::Chunk;

    /// Assert the range contract every chunker must keep: the first range
    /// starts at 0, starts never decrease, each range reaches at least the
    /// next start, the last ends at the document's end, and the pieces
    /// from each start to the next start concatenate back to `doc`.
    pub(crate) fn assert_ranges_tile(doc: &str, chunks: &[Chunk]) {
        assert!(!chunks.is_empty(), "every document yields >=1 chunk");
        assert_eq!(chunks[0].source_range.start, 0, "first range starts at 0");
        let last = chunks.last().expect("non-empty");
        assert_eq!(last.source_range.end, doc.len(), "last range ends at EOF");
        let mut rebuilt = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let next_start = chunks
                .get(i + 1)
                .map_or(doc.len(), |next| next.source_range.start);
            assert!(
                chunk.source_range.start <= next_start,
                "starts must not decrease: {chunks:?}"
            );
            assert!(
                chunk.source_range.end >= next_start,
                "range {i} leaves a gap before the next start: {chunks:?}"
            );
            rebuilt.push_str(&doc[chunk.source_range.start..next_start]);
        }
        assert_eq!(rebuilt, doc, "ranges must tile the document");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str, source_start: usize, body_end: usize) -> DraftChunk {
        DraftChunk {
            body: body.to_string(),
            source_start,
            body_end,
        }
    }

    #[test]
    fn tile_runs_each_range_to_the_next_start_and_the_last_to_eof() {
        let chunks = tile(vec![draft("a", 3, 5), draft("b", 8, 10)], 14);
        assert_eq!(chunks[0].source_range, 0..8);
        assert_eq!(chunks[1].source_range, 8..14);
    }

    #[test]
    fn tile_extends_an_overlapping_body_past_the_next_start() {
        // A split sub-chunk whose body ends after the next sub-chunk starts.
        let chunks = tile(vec![draft("a", 0, 9), draft("b", 6, 12)], 12);
        assert_eq!(chunks[0].source_range, 0..9);
        assert_eq!(chunks[1].source_range, 6..12);
    }

    #[test]
    fn tile_without_drafts_yields_a_placeholder_over_the_whole_document() {
        let chunks = tile(Vec::new(), 7);
        assert_eq!(chunks, vec![placeholder_chunk(7)]);
        assert!(chunks[0].body.is_empty());
    }

    #[test]
    fn split_with_overlap_ranges_contain_each_sub_chunk_body() {
        let doc = format!("{}{}", "x".repeat(5), "あ".repeat(1500));
        let text = MappedText::from_source(&doc, 5..doc.len());
        let drafts = split_with_overlap(&text, 5);
        assert_eq!(drafts.len(), 3);
        for d in &drafts {
            assert_eq!(&doc[d.source_start..d.body_end], d.body);
            assert!(d.body.chars().count() <= WINDOW_CHARS);
        }
    }
}
