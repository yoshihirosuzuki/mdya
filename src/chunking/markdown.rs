//! Heading-aware Markdown chunker.
//!
//! Walks pulldown-cmark events, splitting the document at every heading.
//! Each heading starts a new chunk whose body is the heading text followed
//! by the section text beneath it — so heading words are searchable via
//! both FTS and vector embedding without a separate `title` column.
//! Bodies that exceed [`crate::chunking::WINDOW_CHARS`] are split into
//! overlapping sub-chunks.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use super::{Chunk, ChunkingError, OVERLAP_CHARS, WINDOW_CHARS};

/// Chunk a Markdown document. See module-level docs for the rules; the
/// behaviour is fully covered by `#[cfg(test)]` cases below.
pub fn chunk_markdown(content: &str) -> Result<Vec<Chunk>, ChunkingError> {
    let stripped = strip_front_matter(content);
    let mut walker = Walker::new();
    for event in Parser::new(stripped) {
        walker.handle(event);
    }
    let mut chunks = walker.finalize();
    if chunks.is_empty() {
        chunks.push(super::placeholder_chunk());
    }
    Ok(chunks)
}

/// Drop a YAML-style front matter block (`---\n...\n---\n`) from the very
/// top of the document. Other `---` (horizontal rules elsewhere) are not
/// touched.
///
/// pulldown-cmark exposes `Options::ENABLE_YAML_STYLE_METADATA_BLOCKS` for
/// the same purpose; this hand-written strip is intentional. The library
/// option would (a) require an additional `in_metadata` flag inside Walker
/// because metadata bodies arrive as `Event::Text`, and (b) delegate the
/// "no closing `---`" recovery to the library, which is exactly the edge
/// case `front_matter_without_close_is_treated_as_body` keeps under our
/// explicit control.
fn strip_front_matter(content: &str) -> &str {
    let trimmed = content.trim_start_matches('\n');
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return content;
    }
    let after_open = &trimmed[trimmed.find('\n').expect("just matched ---<newline>") + 1..];
    let Some(close_offset) = find_front_matter_close(after_open) else {
        return content;
    };
    let after_close = &after_open[close_offset..];
    after_close
        .strip_prefix("---\n")
        .or_else(|| after_close.strip_prefix("---\r\n"))
        .or_else(|| after_close.strip_prefix("---"))
        .unwrap_or(after_close)
}

/// Find the byte offset of a closing `---` line (preceded by newline)
/// within an already-trimmed front-matter body.
fn find_front_matter_close(body: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find("\n---") {
        let abs = search_from + rel + 1;
        let after = &body[abs + 3..];
        if after.starts_with('\n') || after.starts_with("\r\n") || after.is_empty() {
            return Some(abs);
        }
        search_from = abs + 3;
    }
    None
}

/// Mutable state for the event walk.
struct Walker {
    /// Heading text of the currently open section, accumulated between
    /// `Start(Heading)` and `End(Heading)`. Empty before the first heading
    /// (the leading section) and for documents with no heading at all.
    current_heading: String,
    /// Section text accumulated beneath the current heading. While
    /// `in_heading == true` text fills `current_heading` instead.
    pending_body: String,
    /// Set between `Event::Start(Tag::Heading)` and the matching
    /// `Event::End(TagEnd::Heading)`.
    in_heading: bool,
    chunks: Vec<Chunk>,
}

impl Walker {
    fn new() -> Self {
        Self {
            current_heading: String::new(),
            pending_body: String::new(),
            in_heading: false,
            chunks: Vec::new(),
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Heading { .. }) => self.open_heading(),
            Event::End(TagEnd::Heading(_)) => {
                self.in_heading = false;
            }
            Event::Text(text) | Event::Code(text) => self.append_text(&text),
            Event::SoftBreak | Event::HardBreak => self.append_text("\n"),
            Event::End(TagEnd::Paragraph) => self.append_text("\n\n"),
            _ => {}
        }
    }

    /// Flush the section that just ended, then begin collecting the new
    /// heading. Heading level is irrelevant: every heading starts its own
    /// chunk, so there is no level stack to maintain.
    fn open_heading(&mut self) {
        self.flush_pending();
        self.in_heading = true;
    }

    fn append_text(&mut self, text: &str) {
        if self.in_heading {
            self.current_heading.push_str(text);
            return;
        }
        self.pending_body.push_str(text);
    }

    /// Emit the current section as a chunk: heading text followed by its
    /// body. A heading with no body still emits (body = heading text) so
    /// section names stay searchable; a section with neither heading nor
    /// body emits nothing.
    fn flush_pending(&mut self) {
        let combined = combine_section(self.current_heading.trim(), self.pending_body.trim());
        if !combined.is_empty() {
            emit_with_overflow_split(&mut self.chunks, &combined);
        }
        self.current_heading.clear();
        self.pending_body.clear();
    }

    fn finalize(mut self) -> Vec<Chunk> {
        self.flush_pending();
        self.chunks
    }
}

/// Join a section's heading and body into chunk text. Either may be empty;
/// the result is empty only when both are.
fn combine_section(heading: &str, body: &str) -> String {
    match (heading.is_empty(), body.is_empty()) {
        (false, false) => format!("{heading}\n\n{body}"),
        (false, true) => heading.to_string(),
        (true, false) => body.to_string(),
        (true, true) => String::new(),
    }
}

/// Emit one chunk if `body` fits in [`WINDOW_CHARS`], otherwise split it
/// into overlapping sub-chunks. The sub-split walks `char_indices` once
/// (O(n)) so very large sections stay linear instead of degrading
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
    fn combine_section_is_empty_only_when_both_inputs_are_empty() {
        // Load-bearing for the placeholder invariant: a real section
        // (heading and/or body present) must never produce an empty chunk
        // body, so `body.is_empty()` uniquely marks the placeholder.
        assert_eq!(combine_section("", ""), "");
        assert_eq!(combine_section("heading", ""), "heading");
        assert_eq!(combine_section("", "body"), "body");
        assert_eq!(combine_section("heading", "body"), "heading\n\nbody");
    }

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
        // e.g. the document title) stay searchable. This is the
        // "floating heading" fix — previously this produced a single
        // empty placeholder.
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
    fn section_over_window_splits_with_overlap() {
        // Body just over 700 chars so we get exactly 2 sub-chunks. Use a
        // headingless body so the char math is exact (no heading prefix).
        let body: String = "あ".repeat(750);
        let out = chunk_markdown(&body).expect("ok");
        assert!(
            out.len() >= 2,
            "expected at least 2 sub-chunks, got {out:?}"
        );
        let first_chars = out[0].body.chars().count();
        let second_chars = out[1].body.chars().count();
        assert_eq!(first_chars, WINDOW_CHARS);
        // step = WINDOW - OVERLAP. The second chunk starts at `step` and
        // runs to end, so it has `750 - step = 750 - 630 = 120` chars.
        assert_eq!(second_chars, 750 - (WINDOW_CHARS - OVERLAP_CHARS));
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
    fn front_matter_without_close_is_treated_as_body() {
        // No closing `---`: do not eat the rest of the file silently.
        let md = "---\ntitle: foo\n# Heading\n\nBody\n";
        let out = chunk_markdown(md).expect("ok");
        // The `---` line stays as-is, "# Heading" becomes a real heading.
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
        // search can match identifiers and prose inside fenced blocks
        // (rule pinned in `mod.rs` docstring).
        assert!(
            out[0].body.contains("Not a heading") && out[0].body.contains("foo"),
            "code-block contents should stay in body; got {:?}",
            out[0].body
        );
    }

    #[test]
    fn japanese_multibyte_chars_count_correctly_for_window() {
        // 1000 hiragana chars (each 3 bytes in UTF-8) — exceeds the 700-char
        // window twice over but stays under any naive byte threshold.
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
