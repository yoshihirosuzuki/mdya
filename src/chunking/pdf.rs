//! Fixed-window sliding chunker for PDFs.
//!
//! PDFs have no Markdown-style heading structure, so we slide a constant-size
//! window over the extracted plain text. Window and overlap are shared with
//! [`super::WINDOW_CHARS`] / [`super::OVERLAP_CHARS`] so retrieval
//! granularity is uniform across file formats. The placeholder rule from
//! `markdown::placeholder_chunk` applies when extraction yields no text.

use super::{Chunk, ChunkingError, OVERLAP_CHARS, WINDOW_CHARS};

/// Chunk PDF-extracted plain text. See module-level docs for the rules; the
/// behaviour is fully covered by `#[cfg(test)]` cases below.
pub fn chunk_pdf(text: &str) -> Result<Vec<Chunk>, ChunkingError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(vec![super::placeholder_chunk()]);
    }
    let mut chunks = Vec::new();
    emit_with_overflow_split(&mut chunks, trimmed);
    Ok(chunks)
}

/// Sliding-window emission identical in shape to the Markdown chunker's
/// overflow path (`chunking::markdown::emit_with_overflow_split`). Walks
/// `char_indices` once so very large bodies stay linear instead of degrading
/// quadratically with repeated `skip(start).take(...)`.
fn emit_with_overflow_split(out: &mut Vec<Chunk>, body: &str) {
    let char_count = body.chars().count();
    if char_count <= WINDOW_CHARS {
        out.push(Chunk {
            body: body.to_string(),
        });
        return;
    }
    let boundaries: Vec<usize> = body
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .collect();
    let step = WINDOW_CHARS - OVERLAP_CHARS;
    let mut start = 0;
    while start < char_count {
        let end = (start + WINDOW_CHARS).min(char_count);
        out.push(Chunk {
            body: body[boundaries[start]..boundaries[end]].to_string(),
        });
        if end == char_count {
            break;
        }
        start += step;
    }
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
