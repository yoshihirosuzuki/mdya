//! Fixed-window sliding chunker for PDFs.
//!
//! PDFs have no Markdown-style heading structure, so we slide a constant-size
//! window over the extracted plain text. Window and overlap are shared with
//! [`super::WINDOW_CHARS`] / [`super::OVERLAP_CHARS`] so retrieval
//! granularity is uniform across file formats. The shared placeholder rule
//! applies when extraction yields no text.

use super::{Chunk, ChunkingError, chunk_plain_text};
#[cfg(test)]
use super::{OVERLAP_CHARS, WINDOW_CHARS};

/// Chunk PDF-extracted plain text. See module-level docs for the rules; the
/// behaviour is fully covered by `#[cfg(test)]` cases below.
pub fn chunk_pdf(text: &str) -> Result<Vec<Chunk>, ChunkingError> {
    Ok(chunk_plain_text(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_yields_one_placeholder_chunk() {
        let out = chunk_pdf("").expect("ok");
        assert_eq!(out.len(), 1);
        assert!(out[0].body.is_empty());
    }

    #[test]
    fn whitespace_only_text_yields_one_placeholder_chunk() {
        let out = chunk_pdf("   \n\n  ").expect("ok");
        assert_eq!(out.len(), 1);
        assert!(out[0].body.is_empty());
    }

    #[test]
    fn short_text_yields_single_chunk() {
        let out = chunk_pdf("Just some text.").expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "Just some text.");
    }

    #[test]
    fn text_over_window_splits_with_overlap() {
        // 750 chars > WINDOW_CHARS (700), exactly one overflow.
        let body: String = "あ".repeat(750);
        let out = chunk_pdf(&body).expect("ok");
        assert!(out.len() >= 2, "expected >=2 sub-chunks, got {out:?}");
        assert_eq!(out[0].body.chars().count(), WINDOW_CHARS);
        // Second chunk starts at `step` and runs to end: 750 - step = 750 - 630.
        assert_eq!(
            out[1].body.chars().count(),
            750 - (WINDOW_CHARS - OVERLAP_CHARS)
        );
    }

    #[test]
    fn ranges_tile_the_untrimmed_text_and_contain_each_body() {
        // Leading / trailing whitespace is trimmed from the bodies but still
        // covered by the first / last range, and every overlapping
        // sub-chunk's range contains its own body.
        let text = format!("\n\n  {}  \n", "あ".repeat(1500));
        let out = chunk_pdf(&text).expect("ok");
        assert_eq!(out.len(), 3);
        crate::chunking::test_support::assert_ranges_tile(&text, &out);
        for chunk in &out {
            assert!(text[chunk.source_range.clone()].contains(&chunk.body));
        }
        assert_eq!(out[1].source_range.start, 4 + 630 * "あ".len());
    }

    #[test]
    fn whitespace_only_placeholder_covers_the_whole_text() {
        let out = chunk_pdf("  \n ").expect("ok");
        assert_eq!(out[0].source_range, 0..4);
    }

    #[test]
    fn japanese_multibyte_chars_count_correctly_for_window() {
        let body: String = "あ".repeat(1000);
        let out = chunk_pdf(&body).expect("ok");
        assert!(out.len() >= 2);
        for chunk in &out {
            assert!(
                chunk.body.chars().count() <= WINDOW_CHARS,
                "chunk exceeds window: {} chars",
                chunk.body.chars().count()
            );
        }
    }
}
