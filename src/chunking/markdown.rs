//! Heading-aware Markdown chunker.
//!
//! Walks pulldown-cmark events, splitting the document at every heading.
//! Each heading starts a new chunk whose body is the heading text followed
//! by the section text beneath it — so heading words are searchable via
//! both FTS and vector embedding without a separate `title` column.
//!
//! Within a section, the body is collected as a list of block-level
//! *segments* (paragraphs, thematic breaks, and fenced code blocks). When a
//! section exceeds [`crate::chunking::WINDOW_CHARS`] the segments are packed
//! greedily into chunks at segment boundaries, so a chunk never cuts a
//! paragraph or a code fence in half. A single segment that is itself larger
//! than the window is the only case that falls back to a fixed-width
//! character split (with [`crate::chunking::OVERLAP_CHARS`] overlap).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use super::{Chunk, ChunkingError, OVERLAP_CHARS, WINDOW_CHARS};

/// Chunk a Markdown document. See module-level docs for the rules; the
/// behaviour is fully covered by `#[cfg(test)]` cases below.
///
/// YAML front matter is removed by pulldown-cmark itself
/// (`ENABLE_YAML_STYLE_METADATA_BLOCKS`): a leading `---` … `---`/`...`
/// block is delivered as a `MetadataBlock` whose text the walker drops. A
/// block without a closing delimiter is not metadata — the parser keeps it
/// as ordinary content, so it stays in the body.
pub fn chunk_markdown(content: &str) -> Result<Vec<Chunk>, ChunkingError> {
    let mut walker = Walker::new();
    for event in Parser::new_ext(content, Options::ENABLE_YAML_STYLE_METADATA_BLOCKS) {
        walker.handle(event);
    }
    let mut chunks = walker.finalize();
    if chunks.is_empty() {
        chunks.push(super::placeholder_chunk());
    }
    Ok(chunks)
}

/// Mutable state for the event walk.
struct Walker {
    /// Heading text of the currently open section, accumulated between
    /// `Start(Heading)` and `End(Heading)`. Empty before the first heading
    /// (the leading section) and for documents with no heading at all.
    current_heading: String,
    /// The block segment currently being accumulated (one paragraph, or the
    /// contents of one fenced code block). Flushed into `segments` at the
    /// block's end.
    current_segment: String,
    /// Completed body segments beneath the current heading, in order.
    segments: Vec<String>,
    /// Set between `Start(Tag::Heading)` and `End(TagEnd::Heading)`.
    in_heading: bool,
    /// Set between `Start(Tag::MetadataBlock)` and its end. Text inside a
    /// metadata block (YAML front matter) is dropped rather than chunked.
    in_metadata: bool,
    chunks: Vec<Chunk>,
}

impl Walker {
    fn new() -> Self {
        Self {
            current_heading: String::new(),
            current_segment: String::new(),
            segments: Vec::new(),
            in_heading: false,
            in_metadata: false,
            chunks: Vec::new(),
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { .. }) => self.open_heading(),
            Event::End(TagEnd::Heading(_)) => self.in_heading = false,
            Event::Start(Tag::MetadataBlock(_)) => self.in_metadata = true,
            Event::End(TagEnd::MetadataBlock(_)) => self.in_metadata = false,
            // A fenced/indented code block is one atomic segment: its inner
            // blank lines must not become segment boundaries, so flush the
            // pending paragraph at the block edges and let the block's text
            // accumulate into a single segment.
            Event::Start(Tag::CodeBlock(_)) | Event::End(TagEnd::CodeBlock) => self.end_segment(),
            Event::Text(text) | Event::Code(text) => self.append_text(&text),
            Event::SoftBreak | Event::HardBreak => self.append_text("\n"),
            Event::End(TagEnd::Paragraph) => self.end_segment(),
            // A thematic break (`---`/`***`) is a block boundary with no text
            // of its own; close the current segment so packing can split here.
            Event::Rule => self.end_segment(),
            _ => {}
        }
    }

    /// Flush the section that just ended, then begin collecting the new
    /// heading. Heading level is irrelevant: every heading starts its own
    /// section, so there is no level stack to maintain.
    fn open_heading(&mut self) {
        self.flush_section();
        self.in_heading = true;
    }

    fn append_text(&mut self, text: &str) {
        if self.in_metadata {
            return;
        }
        if self.in_heading {
            self.current_heading.push_str(text);
            return;
        }
        self.current_segment.push_str(text);
    }

    /// Push the in-progress segment (if any) into `segments`. Empty/whitespace
    /// segments are dropped so they never occupy a chunk. Inside a metadata
    /// block `current_segment` is always empty (`append_text` drops metadata
    /// text), so a stray block-end event there is a harmless no-op.
    fn end_segment(&mut self) {
        let segment = self.current_segment.trim();
        if !segment.is_empty() {
            self.segments.push(segment.to_string());
        }
        self.current_segment.clear();
    }

    /// Emit the current section as chunks: the heading (if any) leads the
    /// first chunk, followed by its packed body segments.
    fn flush_section(&mut self) {
        self.end_segment();
        let heading = std::mem::take(&mut self.current_heading);
        let segments = std::mem::take(&mut self.segments);
        pack_section(&mut self.chunks, heading.trim(), &segments);
    }

    fn finalize(mut self) -> Vec<Chunk> {
        self.flush_section();
        self.chunks
    }
}

/// Build a section's chunks from its heading and body segments. The heading
/// is merged into the first segment so it leads the first chunk only —
/// repeating it on every sub-chunk would inflate the body and double-count
/// the heading words in BM25. A section with neither heading nor body emits
/// nothing.
fn pack_section(out: &mut Vec<Chunk>, heading: &str, segments: &[String]) {
    if heading.is_empty() && segments.is_empty() {
        return;
    }

    let mut blocks: Vec<String> = Vec::with_capacity(segments.len() + 1);
    match (heading.is_empty(), segments.is_empty()) {
        (true, _) => blocks.extend(segments.iter().cloned()),
        (false, true) => blocks.push(heading.to_string()),
        (false, false) => {
            blocks.push(format!("{heading}\n\n{}", segments[0]));
            blocks.extend(segments[1..].iter().cloned());
        }
    }

    pack_blocks(out, &blocks);
}

/// Greedily pack block segments into chunks no larger than [`WINDOW_CHARS`],
/// joining adjacent blocks with a blank line. A single block that exceeds the
/// window is the only case split mid-block, via [`char_split_with_overlap`].
fn pack_blocks(out: &mut Vec<Chunk>, blocks: &[String]) {
    let mut current = String::new();
    for block in blocks {
        if block.chars().count() > WINDOW_CHARS {
            flush_current(out, &mut current);
            char_split_with_overlap(out, block);
            continue;
        }
        if current.is_empty() {
            current.push_str(block);
        } else if joined_len(&current, block) <= WINDOW_CHARS {
            current.push_str("\n\n");
            current.push_str(block);
        } else {
            flush_current(out, &mut current);
            current.push_str(block);
        }
    }
    flush_current(out, &mut current);
}

/// Char length of `current` and `block` joined by a blank line.
fn joined_len(current: &str, block: &str) -> usize {
    current.chars().count() + "\n\n".chars().count() + block.chars().count()
}

fn flush_current(out: &mut Vec<Chunk>, current: &mut String) {
    if !current.is_empty() {
        out.push(Chunk {
            body: std::mem::take(current),
        });
    }
}

/// Split one over-window block into successive sub-chunks with
/// [`OVERLAP_CHARS`] chars of overlap. The walk over `char_indices` is done
/// once (O(n)) so very large blocks stay linear instead of degrading
/// quadratically with repeated `skip(start).take(...)`.
fn char_split_with_overlap(out: &mut Vec<Chunk>, block: &str) {
    let char_count = block.chars().count();
    // Defensive: callers only reach here for over-window blocks, but keep the
    // single-chunk path so this helper is correct in isolation.
    if char_count <= WINDOW_CHARS {
        out.push(Chunk {
            body: block.to_string(),
        });
        return;
    }
    let boundaries: Vec<usize> = block
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(block.len()))
        .collect();
    let step = WINDOW_CHARS - OVERLAP_CHARS;
    let mut start = 0;
    while start < char_count {
        let end = (start + WINDOW_CHARS).min(char_count);
        out.push(Chunk {
            body: block[boundaries[start]..boundaries[end]].to_string(),
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
}
