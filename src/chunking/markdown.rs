//! Heading-aware Markdown chunker.
//!
//! Walks pulldown-cmark events with their source offsets, splitting the
//! document at every heading. Each heading starts a new chunk whose body is
//! the heading text followed by the section text beneath it — so heading
//! words are searchable via both FTS and vector embedding without a
//! separate `title` column.
//!
//! Within a section, the body is collected as a list of *segments*. Every
//! block start and block end closes the current segment, and only inline
//! content (text, emphasis, link text, inline code) and code-block contents
//! are collected as text — so list items, nested lists, and block quotes are
//! separated no matter how the source lays them out. When a section exceeds
//! [`crate::chunking::WINDOW_CHARS`] the segments are packed greedily into
//! chunks at segment boundaries, so a chunk never cuts a paragraph or a code
//! fence in half. A single segment that is itself larger than the window is
//! the only case that falls back to a fixed-width character split (with
//! [`crate::chunking::OVERLAP_CHARS`] overlap).
//!
//! Each segment remembers where its first block starts in the source and
//! where each of its text pieces came from, so every chunk's source range
//! can be recorded (see [`super::Chunk::source_range`]).
//!
//! A document pulldown-cmark cannot report offsets for is chunked as plain
//! text with the same fixed-width window, Markdown syntax included, so it
//! is still indexed in full.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use super::{
    Chunk, ChunkingError, DraftChunk, MappedText, WINDOW_CHARS, chunk_plain_text,
    split_with_overlap, tile,
};

/// Chunk a Markdown document. See module-level docs for the rules; the
/// behaviour is fully covered by `#[cfg(test)]` cases below.
///
/// YAML front matter is removed by pulldown-cmark itself
/// (`ENABLE_YAML_STYLE_METADATA_BLOCKS`): a leading `---` … `---`/`...`
/// block is delivered as a `MetadataBlock` whose text the walker drops. A
/// block without a closing delimiter is not metadata — the parser keeps it
/// as ordinary content, so it stays in the body.
///
/// A document the offset parser cannot walk (see
/// `offset_parser_would_panic`) is chunked as plain text instead, so it is
/// still indexed in full.
pub fn chunk_markdown(content: &str) -> Result<Vec<Chunk>, ChunkingError> {
    if offset_parser_would_panic(content) {
        return Ok(chunk_plain_text(content));
    }
    let mut walker = Walker::new(content);
    for (event, range) in Parser::new_ext(content, PARSER_OPTIONS).into_offset_iter() {
        walker.handle(event, range);
    }
    Ok(tile(walker.finalize(), content.len()))
}

/// Shared by the offset walk and [`offset_parser_would_panic`]: the check is
/// exact only when both parse with the same options.
const PARSER_OPTIONS: Options = Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;

/// Whether pulldown-cmark's offset iterator panics on `content`
/// (<https://github.com/pulldown-cmark/pulldown-cmark/issues/1129>). Some
/// inputs — e.g. a list item holding only a link reference definition,
/// directly followed by a whitespace-only line indented at least four
/// columns past the item's text — leave an empty tight paragraph that the
/// offset iterator unwraps. The plain iterator walks the same tree with the
/// same code except at that point, where it returns `None` early instead of
/// panicking, and yields events again when polled once more. So a plain
/// iterator that resumes after its first `None` marks exactly the inputs the
/// offset iterator panics on.
///
/// `Parser` is declared a `FusedIterator`, so the resume breaks that
/// contract; the check depends on it, so do not `.fuse()` the parser here.
/// A pulldown-cmark release that makes the plain iterator unwrap at that
/// point too would make this check panic as well, and one that fixes the
/// upstream issue would leave the check out of step with the offset
/// iterator. The test `offset_parser_check_matches_the_upstream_panic`
/// fails on either upgrade; remove this check once the upstream issue is
/// fixed.
fn offset_parser_would_panic(content: &str) -> bool {
    let mut parser = Parser::new_ext(content, PARSER_OPTIONS);
    parser.by_ref().for_each(drop);
    parser.next().is_some()
}

/// A block-level run of text and where its first block starts in the
/// source.
struct Segment {
    text: MappedText,
    start: usize,
}

/// A segment start not yet claimed by any text, and the depth of the block
/// that set it.
#[derive(Debug, Clone, Copy)]
struct PendingStart {
    offset: usize,
    depth: usize,
}

/// Mutable state for the event walk.
struct Walker<'a> {
    document: &'a str,
    /// Heading of the currently open section, accumulated between
    /// `Start(Heading)` and `End(Heading)`. `None` before the first heading
    /// (the leading section) and for documents with no heading at all.
    heading: Option<Segment>,
    /// The segment currently being accumulated. Closed at the next block
    /// start or end.
    current: MappedText,
    /// Where the next segment starts: the source offset of the earliest block
    /// that opened since the last segment was pushed, with the nesting depth
    /// of that block. A block that closes without yielding text (an HTML
    /// block, an image-only paragraph) withdraws the start it set, so its
    /// source stays in the previous chunk's range. A block still open when a
    /// heading starts (the block quote around `> # Title`) keeps it, so the
    /// section starts at that block.
    pending_start: Option<PendingStart>,
    /// Number of currently open non-inline blocks (headings excluded).
    open_blocks: usize,
    /// Completed body segments beneath the current heading, in order.
    segments: Vec<Segment>,
    /// Set between `Start(Tag::Heading)` and `End(TagEnd::Heading)`.
    in_heading: bool,
    /// Set between `Start(Tag::MetadataBlock)` and its end. Text inside a
    /// metadata block (YAML front matter) is dropped rather than chunked.
    in_metadata: bool,
    drafts: Vec<DraftChunk>,
}

impl<'a> Walker<'a> {
    fn new(document: &'a str) -> Self {
        Self {
            document,
            heading: None,
            current: MappedText::default(),
            pending_start: None,
            open_blocks: 0,
            segments: Vec::new(),
            in_heading: false,
            in_metadata: false,
            drafts: Vec::new(),
        }
    }

    fn handle(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::Start(Tag::Heading { .. }) => self.open_heading(range.start),
            Event::End(TagEnd::Heading(_)) => self.in_heading = false,
            Event::Start(Tag::MetadataBlock(_)) => {
                self.open_block(range.start);
                self.in_metadata = true;
            }
            Event::End(TagEnd::MetadataBlock(_)) => {
                self.in_metadata = false;
                self.close_block();
            }
            Event::Start(tag) if is_inline(&tag) => {}
            Event::End(tag) if is_inline_end(&tag) => {}
            // Every other tag is a block (paragraph, list, item, block quote,
            // code block, HTML block, …): its start and end both close the
            // current segment. A code block's text therefore forms one atomic
            // segment, and its inner blank lines never split it.
            Event::Start(_) => self.open_block(range.start),
            Event::End(_) => self.close_block(),
            Event::Text(text) | Event::Code(text) => self.append_text(&text, range),
            Event::SoftBreak | Event::HardBreak => self.append_text("\n", range),
            // A thematic break (`---`/`***`) is a block with no text of its
            // own: it closes the segment, and its source stays with the
            // previous chunk.
            Event::Rule => self.end_segment(),
            // HTML (block or inline), footnote references, task-list markers
            // and math carry no body text; their source still falls inside
            // a chunk's range through the tiling.
            _ => {}
        }
    }

    /// Close the current segment and note where the new block starts.
    fn open_block(&mut self, start: usize) {
        self.end_segment();
        self.open_blocks += 1;
        let depth = self.open_blocks;
        self.pending_start.get_or_insert(PendingStart {
            offset: start,
            depth,
        });
    }

    /// Close the current segment. If the closing block set the pending start
    /// and yielded no text, withdraw it.
    fn close_block(&mut self) {
        self.end_segment();
        if self
            .pending_start
            .is_some_and(|p| p.depth == self.open_blocks)
        {
            self.pending_start = None;
        }
        self.open_blocks = self.open_blocks.saturating_sub(1);
    }

    /// Flush the section that just ended, then begin collecting the new
    /// heading. Heading level is irrelevant: every heading starts its own
    /// section, so there is no level stack to maintain. A block still open
    /// around the heading (e.g. the block quote around `> # Title`) moves the
    /// section start back to that block.
    fn open_heading(&mut self, start: usize) {
        self.end_segment();
        self.flush_section();
        let start = self.pending_start.take().map_or(start, |p| p.offset);
        self.heading = Some(Segment {
            text: MappedText::default(),
            start,
        });
        self.in_heading = true;
    }

    fn append_text(&mut self, text: &str, range: Range<usize>) {
        if self.in_metadata {
            return;
        }
        if self.in_heading {
            if let Some(heading) = self.heading.as_mut() {
                heading.text.push(text, range, self.document);
            }
            return;
        }
        let depth = self.open_blocks;
        self.pending_start.get_or_insert(PendingStart {
            offset: range.start,
            depth,
        });
        self.current.push(text, range, self.document);
    }

    /// Push the in-progress segment (if any) into `segments`. Empty /
    /// whitespace segments are dropped so they never occupy a chunk.
    fn end_segment(&mut self) {
        let text = std::mem::take(&mut self.current).trimmed();
        if text.is_empty() {
            return;
        }
        let start = self
            .pending_start
            .take()
            .map(|p| p.offset)
            .or_else(|| text.source_start())
            .unwrap_or(0);
        self.segments.push(Segment { text, start });
    }

    /// Emit the current section as drafts: the heading (if any) leads the
    /// first chunk, followed by its packed body segments.
    fn flush_section(&mut self) {
        let heading = self.heading.take().map(|h| Segment {
            text: h.text.trimmed(),
            start: h.start,
        });
        let segments = std::mem::take(&mut self.segments);
        let blocks = section_blocks(heading, segments);
        // An empty heading (`#` alone) with no body emits nothing; keep its
        // start pending so its source joins the next segment's range.
        if let Some(offset) = blocks.unattached_start {
            // Depth 0 is never closed, so no block withdraws this start.
            self.pending_start = Some(PendingStart { offset, depth: 0 });
        }
        pack_blocks(&mut self.drafts, blocks.blocks);
    }

    fn finalize(mut self) -> Vec<DraftChunk> {
        self.end_segment();
        self.flush_section();
        self.drafts
    }
}

/// Inline tags never close a segment: their text joins the surrounding
/// block's text.
fn is_inline(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn is_inline_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

/// A section's blocks ready for packing, plus the start of an empty heading
/// that had nothing to attach to.
struct SectionBlocks {
    blocks: Vec<Segment>,
    unattached_start: Option<usize>,
}

/// Build a section's blocks from its heading and body segments. The heading
/// is merged into the first segment so it leads the first chunk only —
/// repeating it on every sub-chunk would inflate the body and double-count
/// the heading words in BM25. The merged block starts where the heading
/// starts. An empty heading contributes only its start.
fn section_blocks(heading: Option<Segment>, mut segments: Vec<Segment>) -> SectionBlocks {
    let Some(heading) = heading else {
        return SectionBlocks {
            blocks: segments,
            unattached_start: None,
        };
    };
    if heading.text.is_empty() {
        let Some(first) = segments.first_mut() else {
            return SectionBlocks {
                blocks: segments,
                unattached_start: Some(heading.start),
            };
        };
        first.start = heading.start;
        return SectionBlocks {
            blocks: segments,
            unattached_start: None,
        };
    }
    let mut merged = heading;
    if !segments.is_empty() {
        let first = segments.remove(0);
        merged.text.push_unmapped("\n\n");
        merged.text.append(first.text);
    }
    segments.insert(0, merged);
    SectionBlocks {
        blocks: segments,
        unattached_start: None,
    }
}

/// Greedily pack blocks into drafts no larger than [`WINDOW_CHARS`],
/// joining adjacent blocks with a blank line. A single block that exceeds the
/// window is the only case split mid-block, via [`split_with_overlap`].
fn pack_blocks(out: &mut Vec<DraftChunk>, blocks: Vec<Segment>) {
    let mut current: Option<Segment> = None;
    for block in blocks {
        if block.text.char_count() > WINDOW_CHARS {
            flush_current(out, current.take());
            out.extend(split_with_overlap(&block.text, block.start));
            continue;
        }
        match current.as_mut() {
            Some(open) if joined_len(&open.text, &block.text) <= WINDOW_CHARS => {
                open.text.push_unmapped("\n\n");
                open.text.append(block.text);
            }
            _ => {
                flush_current(out, current.take());
                current = Some(block);
            }
        }
    }
    flush_current(out, current);
}

/// Char length of `current` and `block` joined by a blank line.
fn joined_len(current: &MappedText, block: &MappedText) -> usize {
    current.char_count() + "\n\n".chars().count() + block.char_count()
}

fn flush_current(out: &mut Vec<DraftChunk>, current: Option<Segment>) {
    if let Some(segment) = current {
        out.push(DraftChunk::new(segment.text, segment.start));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::OVERLAP_CHARS;

    #[test]
    fn empty_document_yields_one_placeholder_chunk() {
        // Every file owns >=1 chunk. An empty document gets a single
        // placeholder with an empty body.
        let out = chunk_markdown("").expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "");
    }

    #[test]
    fn front_matter_only_document_yields_placeholder_chunk() {
        // Front matter is stripped, leaving no body -> placeholder.
        let out = chunk_markdown("---\ntitle: foo\ndate: 2024-01-01\n---\n").expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "");
    }

    #[test]
    fn headings_only_document_emits_one_chunk_per_heading() {
        // Headings with no section body each emit a chunk carrying the
        // heading text, so section names (often the most important words,
        // e.g. the document title) stay searchable.
        let out = chunk_markdown("# A\n\n## B\n\n### C\n").expect("ok");
        let bodies: Vec<&str> = out.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, vec!["A", "B", "C"]);
    }

    #[test]
    fn placeholder_is_the_only_source_of_an_empty_body() {
        // The ingest writer relies on `body.is_empty()` to identify the
        // placeholder (and store a null embedding). Real sections must
        // never flush an empty body, so a document with real content
        // produces no empty-body chunk.
        let out = chunk_markdown("# T\n\nreal body\n").expect("ok");
        assert!(out.iter().all(|c| !c.body.is_empty()));
    }

    #[test]
    fn document_with_only_body_keeps_text_in_body() {
        let out = chunk_markdown("Just some text.").expect("ok");
        assert_eq!(out.len(), 1);
        assert!(out[0].body.contains("Just some text."));
    }

    #[test]
    fn single_heading_keeps_heading_and_body_in_body() {
        let md = "# Title\n\nBody text.\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1);
        // Heading text lives in the body, ahead of the section.
        assert!(out[0].body.contains("Title"));
        assert!(out[0].body.contains("Body text."));
    }

    #[test]
    fn each_heading_starts_a_new_chunk_carrying_its_heading() {
        let md = "# Top\n\nTop body\n\n## Sub\n\nSub body\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(out[0].body.contains("Top") && out[0].body.contains("Top body"));
        assert!(out[1].body.contains("Sub") && out[1].body.contains("Sub body"));
    }

    #[test]
    fn nested_headings_do_not_carry_ancestor_path() {
        // Each chunk carries only its own (leaf) heading; ancestor
        // headings are searchable via their own chunks.
        let md = "# A\n\nA body\n\n## B\n\nB body\n\n### C\n\nC body\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 3);
        assert!(out[0].body.starts_with("A") && out[0].body.contains("A body"));
        assert!(out[1].body.starts_with("B") && out[1].body.contains("B body"));
        assert!(out[2].body.starts_with("C") && out[2].body.contains("C body"));
        assert!(!out[2].body.contains("A / B"));
    }

    #[test]
    fn heading_with_empty_body_still_emits_its_heading() {
        // `## A` has no body before `## B`; the chunker emits both
        // (A as a heading-only chunk) rather than skipping A.
        let md = "## A\n\n## B\n\nB body\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].body, "A");
        assert!(out[1].body.contains("B") && out[1].body.contains("B body"));
    }

    #[test]
    fn intro_before_first_heading_emits_its_own_chunk() {
        let md = "Intro paragraph.\n\n# First\n\nFirst body\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 2);
        // The leading section has no heading, so its body is just the intro.
        assert!(out[0].body.contains("Intro paragraph."));
        assert!(!out[0].body.contains("First"));
        assert!(out[1].body.contains("First") && out[1].body.contains("First body"));
    }

    #[test]
    fn long_prose_subsplit_breaks_at_paragraph_boundary() {
        // Three 300-char paragraphs (900 chars total) exceed the 700 window.
        // Packing fills a chunk with whole paragraphs (300 + 300 fits, the
        // third spills over) and never cuts a paragraph in half.
        let para_a = "a".repeat(300);
        let para_b = "b".repeat(300);
        let para_c = "c".repeat(300);
        let md = format!("{para_a}\n\n{para_b}\n\n{para_c}\n");
        let out = chunk_markdown(&md).expect("ok");
        assert_eq!(
            out.len(),
            2,
            "expected paragraph-packed chunks, got {out:?}"
        );
        assert_eq!(out[0].body, format!("{para_a}\n\n{para_b}"));
        assert_eq!(out[1].body, para_c);
    }

    #[test]
    fn paragraph_packed_chunks_do_not_overlap() {
        // Packing at segment boundaries adds no inter-chunk overlap: the
        // third paragraph appears only in the second chunk.
        let para_a = "a".repeat(300);
        let para_b = "b".repeat(300);
        let para_c = "c".repeat(300);
        let md = format!("{para_a}\n\n{para_b}\n\n{para_c}\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(!out[0].body.contains('c'));
        assert!(!out[1].body.contains('a') && !out[1].body.contains('b'));
    }

    #[test]
    fn heading_leads_only_the_first_chunk_of_a_split_section() {
        // A heading plus paragraphs that overflow the window: the heading
        // rides on the first chunk, later chunks carry no heading.
        let para_a = "a".repeat(300);
        let para_b = "b".repeat(300);
        let para_c = "c".repeat(300);
        let md = format!("# H\n\n{para_a}\n\n{para_b}\n\n{para_c}\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(out[0].body.starts_with("H"));
        assert!(out.iter().skip(1).all(|c| !c.body.contains('H')));
    }

    #[test]
    fn code_fence_is_not_split_when_section_overflows() {
        // A 500-char paragraph and a 500-char code block overflow the window
        // together, but each is its own segment, so the code block lands in a
        // single chunk intact rather than being cut across chunks.
        let prose = "a".repeat(500);
        let code = "b".repeat(500);
        let md = format!("{prose}\n\n```\n{code}\n```\n");
        let out = chunk_markdown(&md).expect("ok");
        assert_eq!(
            out.len(),
            2,
            "expected prose and code in separate chunks, got {out:?}"
        );
        assert!(
            out.iter().any(|c| c.body == code),
            "code block should stay intact in one chunk; got {out:?}"
        );
    }

    #[test]
    fn oversized_code_fence_falls_back_to_char_split() {
        // A code block larger than the window is the one case a fence is
        // split: it is char-split with overlap so WINDOW_CHARS stays a hard
        // upper bound (no over-window chunk is ever emitted).
        let code = "b".repeat(900);
        let md = format!("```\n{code}\n```\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(
            out.len() >= 2,
            "expected char-split sub-chunks, got {out:?}"
        );
        assert_eq!(out[0].body.chars().count(), WINDOW_CHARS);
        for chunk in &out {
            assert!(chunk.body.chars().count() <= WINDOW_CHARS);
        }
    }

    #[test]
    fn section_over_window_splits_with_overlap() {
        // A single headingless paragraph just over 700 chars is one
        // oversized segment, so it falls back to the char split: exactly two
        // sub-chunks with OVERLAP_CHARS overlap.
        let body: String = "あ".repeat(750);
        let out = chunk_markdown(&body).expect("ok");
        assert_eq!(out.len(), 2, "expected 2 sub-chunks, got {out:?}");
        assert_eq!(out[0].body.chars().count(), WINDOW_CHARS);
        // step = WINDOW - OVERLAP. The second chunk starts at `step` and runs
        // to the end, so it holds `750 - step = 750 - 630 = 120` chars.
        assert_eq!(
            out[1].body.chars().count(),
            750 - (WINDOW_CHARS - OVERLAP_CHARS)
        );
    }

    #[test]
    fn front_matter_is_stripped_from_body() {
        let md = "---\ntitle: foo\ndate: 2024-01-01\n---\n# Heading\n\nBody\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1);
        assert!(out[0].body.contains("Heading"));
        assert!(
            !out[0].body.contains("title: foo"),
            "front matter leaked into body: {}",
            out[0].body
        );
    }

    #[test]
    fn front_matter_with_dots_close_is_stripped() {
        // `...` is a valid YAML document-end marker; pulldown-cmark treats it
        // as a metadata block close, so the front matter is stripped just like
        // a `---` close (the hand-rolled stripper this replaced missed it).
        let md = "---\ntitle: foo\ndate: 2024-01-01\n...\n# Heading\n\nBody\n";
        let out = chunk_markdown(md).expect("ok");
        assert!(out.iter().any(|c| c.body.contains("Heading")));
        assert!(
            out.iter().all(|c| !c.body.contains("title: foo")),
            "front matter with `...` close should be stripped; got {out:?}"
        );
    }

    #[test]
    fn front_matter_without_close_is_treated_as_body() {
        // No closing delimiter: pulldown-cmark does not treat the opening
        // `---` as metadata, so the file is not silently eaten — the content
        // stays in the body.
        let md = "---\ntitle: foo\n# Heading\n\nBody\n";
        let out = chunk_markdown(md).expect("ok");
        assert!(
            out.iter().any(|c| c.body.contains("Heading")),
            "expected a chunk carrying the heading; got {out:?}"
        );
        assert!(
            out.iter().any(|c| c.body.contains("title: foo")),
            "front matter text should remain in body when close marker is absent; got {out:?}"
        );
    }

    #[test]
    fn code_fence_does_not_open_new_chunk_on_hash_lines() {
        let md = "# Real\n\n```\n# Not a heading\nfoo\n```\n\nMore\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1, "expected single chunk, got {out:?}");
        assert!(out[0].body.contains("Real"));
        // Code-block contents must remain in the body so FTS / vector
        // search can match identifiers and prose inside fenced blocks.
        assert!(
            out[0].body.contains("Not a heading") && out[0].body.contains("foo"),
            "code-block contents should stay in body; got {:?}",
            out[0].body
        );
    }

    #[test]
    fn code_fence_with_inner_blank_line_stays_one_segment() {
        // A blank line inside a fence must not split it: pulldown-cmark
        // delivers the block's text as one run, and the walker treats the
        // whole fence as a single atomic segment. Both lines land together.
        let md = "```\nline one\n\nline two\n```\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(
            out.len(),
            1,
            "code fence split on its inner blank line: {out:?}"
        );
        assert!(out[0].body.contains("line one") && out[0].body.contains("line two"));
    }

    #[test]
    fn japanese_multibyte_chars_count_correctly_for_window() {
        // 1000 hiragana chars (each 3 bytes in UTF-8) — exceeds the 700-char
        // window but stays under any naive byte threshold.
        let body: String = "あ".repeat(1000);
        let md = format!("# 見出し\n\n{body}\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(out.len() >= 2);
        assert!(out[0].body.contains("見出し"));
        for chunk in &out {
            assert!(
                chunk.body.chars().count() <= WINDOW_CHARS,
                "chunk exceeds window: {} chars",
                chunk.body.chars().count()
            );
        }
    }

    #[test]
    fn heading_inline_formatting_is_flattened_in_body() {
        let md = "# **Bold** and *italic*\n\nBody\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1);
        // Markdown emphasis markers are dropped; the plain heading text
        // leads the body.
        assert!(out[0].body.starts_with("Bold and italic"));
    }

    use crate::chunking::test_support::assert_ranges_tile;

    #[test]
    fn tight_list_items_are_separated_not_concatenated() {
        // Items of a list without blank lines are not wrapped in paragraphs
        // by pulldown-cmark; every item boundary still separates the text.
        let md = "- alpha\n- beta\n- gamma\n\n1. one\n2. two\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].body, "alpha\n\nbeta\n\ngamma\n\none\n\ntwo");
    }

    #[test]
    fn nested_list_and_block_quote_items_are_separated() {
        let md = "- outer\n  - inner\n- next\n\n> - quoted\n> - also\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out[0].body, "outer\n\ninner\n\nnext\n\nquoted\n\nalso");
    }

    #[test]
    fn link_targets_and_html_stay_out_of_the_body() {
        let md =
            "See [docs](https://example.com/x) and <b>bold</b>.\n\n<div>\nblock html\n</div>\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out[0].body, "See docs and bold.");
    }

    #[test]
    fn source_range_returns_the_original_formatting() {
        // The body is plain text; the range slices the original Markdown,
        // link targets and HTML included.
        let md = "# Title\n\n- [docs](https://example.com/x)\n- <b>bold</b>\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(&md[out[0].source_range.clone()], md);
    }

    const TEXT_LESS_BLOCK_DOCS: [&str; 8] = [
        "",
        "   \n",
        "---\ntitle: foo\n---\n",
        "---\ntitle: foo\n---\n# A\n\nbody\n\n<div>x</div>\n\n## B\n\n![](img.png)\n\n## C\n",
        "<!-- leading comment -->\n\n# A\n\ntext\n\n---\n\n# B\n\n> # quoted heading\n\ntail\n",
        "#\n\nafter an empty heading\n\n#\n",
        "a\r\nb\r\n\r\n```\r\nx\r\ny\r\n```\r\n",
        "- item\n\n      indented code\n      line2\n",
    ];

    #[test]
    fn ranges_tile_documents_with_text_less_blocks() {
        for md in TEXT_LESS_BLOCK_DOCS {
            let out = chunk_markdown(md).expect("ok");
            assert_ranges_tile(md, &out);
        }
    }

    #[test]
    fn packed_chunk_ranges_start_at_their_first_block() {
        // Three 300-char paragraphs pack into two chunks; the second chunk's
        // range starts at the third paragraph and runs to the end.
        let para = |c: char| c.to_string().repeat(300);
        let md = format!("{}\n\n{}\n\n- {}\n", para('a'), para('b'), para('c'));
        let out = chunk_markdown(&md).expect("ok");
        assert_eq!(out.len(), 2);
        assert_ranges_tile(&md, &out);
        assert_eq!(
            &md[out[1].source_range.clone()],
            format!("- {}\n", para('c'))
        );
    }

    #[test]
    fn oversized_fence_sub_chunk_ranges_contain_their_bodies() {
        let code: String = (0..120).map(|i| format!("line {i:03}\n")).collect();
        // The intro paragraph takes the heading, so the fence is its own block.
        let md = format!("# H\n\nintro\n\n```\n{code}```\n\nafter\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(out.len() >= 3, "expected split sub-chunks, got {out:?}");
        assert_ranges_tile(&md, &out);
        for chunk in out.iter().filter(|c| c.body.starts_with("line")) {
            assert!(
                md[chunk.source_range.clone()].contains(&chunk.body),
                "range {:?} misses body {:?}",
                chunk.source_range,
                chunk.body
            );
        }
        // The first sub-chunk's range starts at the opening fence.
        let first_code = out.iter().position(|c| c.body.starts_with("line")).unwrap();
        assert!(md[out[first_code].source_range.clone()].starts_with("```"));
    }

    #[test]
    fn escaped_text_ranges_round_out_to_whole_pieces() {
        // `&amp;` decodes to `&`, so its piece maps only as a whole; the
        // char-split ranges must still contain the source of every body.
        let body: String = "a &amp; b ".repeat(200);
        let md = format!("{body}\n");
        let out = chunk_markdown(&md).expect("ok");
        assert!(out.len() >= 2);
        assert_ranges_tile(&md, &out);
        for chunk in &out {
            let decoded = md[chunk.source_range.clone()].replace("&amp;", "&");
            assert!(
                decoded.contains(&chunk.body),
                "range misses body: {chunk:?}"
            );
        }
    }

    /// Source text of each chunk's range.
    fn range_texts<'a>(md: &'a str, chunks: &[Chunk]) -> Vec<&'a str> {
        chunks.iter().map(|c| &md[c.source_range.clone()]).collect()
    }

    #[test]
    fn trailing_text_less_blocks_stay_with_their_own_section() {
        // An HTML block, an image-only paragraph, and a thematic break at the
        // end of a section belong to that section, not to the next heading.
        let cases = [
            ("# A\n\ntext\n\n<div>x</div>\n\n", "# B\n\nb\n"),
            ("# A\n\ntext\n\n![](x.png)\n\n", "# B\n\nb\n"),
            ("# A\n\ntext\n\n---\n\n", "# B\n\nb\n"),
            ("# A\n\n- item\n- ![](x.png)\n\n", "# B\n\nb\n"),
        ];
        for (first, second) in cases {
            let md = format!("{first}{second}");
            let out = chunk_markdown(&md).expect("ok");
            assert_eq!(range_texts(&md, &out), vec![first, second], "{md:?}");
        }
    }

    #[test]
    fn block_quote_around_a_heading_starts_the_section() {
        let md = "intro\n\n> # Quoted\n\nbody\n";
        let out = chunk_markdown(md).expect("ok");
        assert_eq!(
            range_texts(md, &out),
            vec!["intro\n\n", "> # Quoted\n\nbody\n"]
        );
    }

    #[test]
    fn html_between_paragraphs_stays_with_the_preceding_chunk() {
        let para = |c: char| c.to_string().repeat(400);
        let md = format!("{}\n\n<div>x</div>\n\n{}\n", para('a'), para('b'));
        let out = chunk_markdown(&md).expect("ok");
        assert_eq!(out.len(), 2);
        assert!(range_texts(&md, &out)[0].ends_with("<div>x</div>\n\n"));
        assert!(range_texts(&md, &out)[1].starts_with('b'));
    }

    /// A list item holding only a link reference definition, directly
    /// followed by a whitespace-only line indented at least four columns
    /// past the item's text.
    const OFFSET_PARSER_TRIGGERS: [&str; 11] = [
        "- [a]: b\n      ",
        "- [a]: /u\n\t\t",
        "    indented\n- [^1]: foot\n\t\t",
        "# T\n\n- [a]: b\n      \n\n## Next\n\nafter text\n",
        "1. [o]:u\n    \t",
        "* [o]:u\n    \t",
        "> - [o]:u\n>     \t",
        "- x\n- [o]:u\n    \t",
        "- [o]:u\r\n    \t",
        "- [o]:u\n\t\t",
        "1. [a]: b\n       ",
    ];

    #[test]
    fn documents_the_offset_parser_cannot_handle_are_indexed_in_full() {
        for md in OFFSET_PARSER_TRIGGERS {
            let out = chunk_markdown(md).expect("ok");
            assert_ranges_tile(md, &out);
        }
        let md = OFFSET_PARSER_TRIGGERS[3];
        let out = chunk_markdown(md).expect("ok");
        assert!(
            out.iter().any(|c| c.body.contains("after text")),
            "text after the trigger is missing: {out:?}"
        );
    }

    #[test]
    fn long_document_after_a_trigger_is_window_split_within_its_ranges() {
        let md = format!(
            "- [a]: b\n      \n\n{}\r\n\r\n{}\r\n",
            "あ".repeat(800),
            "い".repeat(800)
        );
        assert!(offset_parser_would_panic(&md));
        let out = chunk_markdown(&md).expect("ok");
        assert_eq!(out.len(), 3, "{out:?}");
        assert_ranges_tile(&md, &out);
        for chunk in &out {
            assert!(md[chunk.source_range.clone()].contains(&chunk.body));
        }
    }

    /// Whether pulldown-cmark's offset iterator really panics on `md`. Tests
    /// unwind, so the panic can be caught here; release builds abort.
    fn offset_iter_panics(md: &str) -> bool {
        std::panic::catch_unwind(|| {
            Parser::new_ext(md, PARSER_OPTIONS)
                .into_offset_iter()
                .count()
        })
        .is_err()
    }

    #[test]
    fn offset_parser_check_matches_the_upstream_panic() {
        // Tripwire for pulldown-cmark upgrades. If the triggers stop
        // panicking upstream, the issue is fixed and
        // `offset_parser_would_panic` can go; if the check and the real panic
        // disagree, the guard no longer holds and must be revisited.
        for md in OFFSET_PARSER_TRIGGERS {
            assert!(offset_iter_panics(md), "{md:?} no longer panics upstream");
        }
        let near_misses = [
            "- [a]: b\n  ",
            "- [a]: b\n\t",
            "- [a]: b\n\t ",
            "- [a]: b\n     ",
            "1. [a]: b\n      ",
            "- [a]: b\n\n      \n",
            "- [a]: b\r\n      \r\nrest",
            "# T\n\n- [a]: b\n\nafter text\n",
        ];
        for md in near_misses {
            assert!(!offset_iter_panics(md), "{md:?} now panics upstream");
        }
        let docs = OFFSET_PARSER_TRIGGERS
            .into_iter()
            .chain(near_misses)
            .chain(TEXT_LESS_BLOCK_DOCS);
        for md in docs {
            assert_eq!(
                offset_parser_would_panic(md),
                offset_iter_panics(md),
                "{md:?}"
            );
        }
    }
}
