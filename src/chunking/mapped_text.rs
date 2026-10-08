//! Plain text that remembers where each piece came from in the original
//! document.
//!
//! The chunkers build a chunk's `body` by concatenating plain-text pieces
//! (pulldown-cmark text events for Markdown, the trimmed extract for PDF).
//! To record each chunk's byte range in the original document, every piece
//! keeps the source range it was read from. Separators the chunker inserts
//! itself (the blank line joining two blocks) have no source and are not
//! mapped.
//!
//! A piece is *verbatim* when its text equals the source bytes it came
//! from. Offsets inside a verbatim piece map 1:1 onto the source. A
//! non-verbatim piece (an escape such as `\*`, an entity such as `&amp;`,
//! inline code with its backticks, a CRLF line break) maps only as a whole:
//! a start inside it rounds down to the piece's source start and an end
//! inside it rounds up to the piece's source end, so a mapped range always
//! contains the text it was mapped from.

use std::ops::Range;

/// One mapped piece: `plain` is its byte range in [`MappedText::as_str`],
/// `source` its byte range in the original document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    plain: Range<usize>,
    source: Range<usize>,
    verbatim: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct MappedText {
    text: String,
    /// Ordered by `plain.start`, non-overlapping.
    pieces: Vec<Piece>,
}

impl MappedText {
    /// A single piece of text read from `source` in `document`.
    pub(super) fn from_source(document: &str, source: Range<usize>) -> Self {
        let mut mapped = Self::default();
        let text = document[source.clone()].to_string();
        mapped.push(&text, source, document);
        mapped
    }

    /// Append `text`, read from `source` in `document`. The piece is
    /// verbatim when `text` equals the source bytes.
    pub(super) fn push(&mut self, text: &str, source: Range<usize>, document: &str) {
        if text.is_empty() {
            return;
        }
        let verbatim = document.get(source.clone()) == Some(text);
        let start = self.text.len();
        self.text.push_str(text);
        self.pieces.push(Piece {
            plain: start..self.text.len(),
            source,
            verbatim,
        });
    }

    /// Append text the chunker inserts itself (it has no source).
    pub(super) fn push_unmapped(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Append `other`, shifting its pieces past the current text.
    pub(super) fn append(&mut self, other: MappedText) {
        let shift = self.text.len();
        self.text.push_str(&other.text);
        self.pieces.extend(other.pieces.into_iter().map(|p| Piece {
            plain: p.plain.start + shift..p.plain.end + shift,
            ..p
        }));
    }

    /// Copy with leading and trailing whitespace removed. Pieces cut by the
    /// trim keep a source range that still covers their remaining text.
    pub(super) fn trimmed(&self) -> MappedText {
        let start = self.text.len() - self.text.trim_start().len();
        let end = start + self.text.trim().len();
        self.slice(start..end)
    }

    /// Copy of the text in `range` (byte offsets on char boundaries), with
    /// the pieces clipped to it and rebased to start at 0. Only the pieces
    /// overlapping `range` are visited (found by binary search), so slicing
    /// a large block into many windows stays proportional to the window size.
    pub(super) fn slice(&self, range: Range<usize>) -> MappedText {
        let first = self.pieces.partition_point(|p| p.plain.end <= range.start);
        let pieces = self.pieces[first..]
            .iter()
            .take_while(|p| p.plain.start < range.end)
            .filter_map(|p| clip(p, &range))
            .collect();
        MappedText {
            text: self.text[range].to_string(),
            pieces,
        }
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn into_string(self) -> String {
        self.text
    }

    pub(super) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub(super) fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Source offset where the text from plain offset `plain` begins. An
    /// offset inside a non-verbatim piece rounds down to the piece's source
    /// start; an offset in an unmapped separator moves to the next piece.
    /// `None` when no piece lies at or after `plain`.
    pub(super) fn source_start_at(&self, plain: usize) -> Option<usize> {
        // Pieces are ordered and non-overlapping, so both `plain.start` and
        // `plain.end` are sorted and a binary search finds the piece.
        let idx = self.pieces.partition_point(|p| p.plain.end <= plain);
        let piece = self.pieces.get(idx)?;
        if plain < piece.plain.start || !piece.verbatim {
            return Some(piece.source.start);
        }
        Some(piece.source.start + (plain - piece.plain.start))
    }

    /// Source offset where the text ending at plain offset `plain_end`
    /// (exclusive) ends. An end inside a non-verbatim piece rounds up to the
    /// piece's source end; an end in an unmapped separator moves back to
    /// the previous piece. `None` when no piece lies before `plain_end`.
    pub(super) fn source_end_at(&self, plain_end: usize) -> Option<usize> {
        let idx = self.pieces.partition_point(|p| p.plain.start < plain_end);
        let piece = self.pieces.get(idx.checked_sub(1)?)?;
        if plain_end > piece.plain.end || !piece.verbatim {
            return Some(piece.source.end);
        }
        Some(piece.source.start + (plain_end - piece.plain.start))
    }

    /// Source offset where the whole text begins, if any piece is mapped.
    pub(super) fn source_start(&self) -> Option<usize> {
        self.source_start_at(0)
    }

    /// Source offset where the whole text ends, if any piece is mapped.
    pub(super) fn source_end(&self) -> Option<usize> {
        self.source_end_at(self.text.len())
    }
}

/// Clip `piece` to the plain `range` and rebase it to `range.start`. A
/// verbatim piece's source shrinks by the same amount; a non-verbatim
/// piece keeps its whole source range (it only maps as a whole).
fn clip(piece: &Piece, range: &Range<usize>) -> Option<Piece> {
    let start = piece.plain.start.max(range.start);
    let end = piece.plain.end.min(range.end);
    if start >= end {
        return None;
    }
    let source = if piece.verbatim {
        let front = start - piece.plain.start;
        let back = piece.plain.end - end;
        piece.source.start + front..piece.source.end - back
    } else {
        piece.source.clone()
    };
    Some(Piece {
        plain: start - range.start..end - range.start,
        source,
        verbatim: piece.verbatim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_piece_maps_inner_offsets_one_to_one() {
        let doc = "xx hello world";
        let text = MappedText::from_source(doc, 3..14);
        assert_eq!(text.source_start_at(6), Some(9));
        assert_eq!(text.source_end_at(5), Some(8));
        assert_eq!(&doc[9..14], "world");
    }

    #[test]
    fn non_verbatim_piece_rounds_start_down_and_end_up() {
        // `&amp;` decodes to `&`: the plain text no longer matches the
        // source, so offsets map only to the piece's edges.
        let doc = "a &amp; b";
        let mut text = MappedText::default();
        text.push("a ", 0..2, doc);
        text.push("&", 2..7, doc);
        text.push(" b", 7..9, doc);
        assert_eq!(text.as_str(), "a & b");
        assert_eq!(text.source_start_at(2), Some(2));
        assert_eq!(text.source_end_at(3), Some(7));
    }

    #[test]
    fn unmapped_separator_moves_start_forward_and_end_back() {
        let doc = "ab..cd";
        let mut text = MappedText::from_source(doc, 0..2);
        text.push_unmapped("\n\n");
        text.append(MappedText::from_source(doc, 4..6));
        assert_eq!(text.as_str(), "ab\n\ncd");
        // Offset 2 is inside the separator: the start moves to `cd`.
        assert_eq!(text.source_start_at(2), Some(4));
        // An end inside the separator moves back to the end of `ab`.
        assert_eq!(text.source_end_at(3), Some(2));
        assert_eq!(text.source_end(), Some(6));
    }

    #[test]
    fn trimmed_rebases_pieces_and_shrinks_verbatim_sources() {
        let doc = "  body  ";
        let text = MappedText::from_source(doc, 0..8).trimmed();
        assert_eq!(text.as_str(), "body");
        assert_eq!(text.source_start(), Some(2));
        assert_eq!(text.source_end(), Some(6));
    }

    #[test]
    fn trimmed_drops_whitespace_only_pieces() {
        let doc = "a\r\n";
        let mut text = MappedText::default();
        text.push("a", 0..1, doc);
        text.push("\n", 1..3, doc);
        let trimmed = text.trimmed();
        assert_eq!(trimmed.as_str(), "a");
        assert_eq!(trimmed.source_end(), Some(1));
    }

    #[test]
    fn empty_text_has_no_source_edges() {
        let text = MappedText::default();
        assert!(text.is_empty());
        assert_eq!(text.source_start(), None);
        assert_eq!(text.source_end(), None);
    }
}
