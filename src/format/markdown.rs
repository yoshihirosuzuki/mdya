//! Cuts the body of a Markdown document into chunks: at every heading, and
//! within a heading's section at the starts of its blocks.

use pulldown_cmark::{Event, Options, Parser, Tag};

use super::window::{self, MAX_CHARS};
use crate::types::Chunk;

/// Front matter is a block of its own, not a thematic break and a heading.
const PARSER_OPTIONS: Options = Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;

/// Where a block begins. Blocks are the places a chunk may start.
struct Block {
    start: usize,
    heading: bool,
}

/// The chunks of `body`, packing blocks until the next would take a chunk over
/// [`MAX_CHARS`]. A chunk runs up to where the next one starts, so that
/// together they cover all of `body`.
pub(super) fn chunks(body: &str) -> Vec<Chunk> {
    if body.is_empty() {
        return Vec::new();
    }
    let blocks = block_starts(body);
    let mut packer = Packer::new(body);
    for (i, block) in blocks.iter().enumerate() {
        let end = blocks.get(i + 1).map_or(body.len(), |next| next.start);
        packer.add(block, end);
    }
    packer.finish()
}

/// The starts of the blocks of `body`, in order. A block starts where its line
/// does, so that the markers of the containers around it stay with it. Blocks
/// that begin on the same line, such as a list and its first item, are one.
fn block_starts(body: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for (event, range) in Parser::new_ext(body, PARSER_OPTIONS).into_offset_iter() {
        if !starts_a_block(&event) {
            continue;
        }
        let start = line_start(body, range.start);
        let heading = matches!(event, Event::Start(Tag::Heading { .. }));
        match blocks.last_mut() {
            Some(last) if last.start == start => last.heading |= heading,
            _ => blocks.push(Block { start, heading }),
        }
    }
    with_first_at_zero(blocks)
}

fn starts_a_block(event: &Event<'_>) -> bool {
    match event {
        Event::Start(tag) => !is_inline(tag),
        Event::Rule => true,
        _ => false,
    }
}

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

fn line_start(body: &str, offset: usize) -> usize {
    body[..offset]
        .rfind(['\n', '\r'])
        .map_or(0, |line_end| line_end + 1)
}

/// What comes before the first block, such as blank lines or link reference
/// definitions, goes with that block. A body with no blocks is one block.
fn with_first_at_zero(mut blocks: Vec<Block>) -> Vec<Block> {
    match blocks.first_mut() {
        Some(first) => first.start = 0,
        None => blocks.push(Block {
            start: 0,
            heading: false,
        }),
    }
    blocks
}

/// The chunk being filled: where it starts, how many characters it has, and
/// whether a heading is all it holds so far.
struct OpenChunk {
    start: usize,
    chars: usize,
    only_heading: bool,
}

struct Packer<'a> {
    body: &'a str,
    chunks: Vec<Chunk>,
    open: Option<OpenChunk>,
}

impl<'a> Packer<'a> {
    fn new(body: &'a str) -> Self {
        Self {
            body,
            chunks: Vec::new(),
            open: None,
        }
    }

    fn add(&mut self, block: &Block, end: usize) {
        let chars = self.body[block.start..end].chars().count();
        if chars > MAX_CHARS {
            self.cut(block, end);
            return;
        }
        if block.heading || self.would_overflow(chars) {
            self.close(block.start);
        }
        match &mut self.open {
            Some(open) => {
                open.chars += chars;
                open.only_heading = false;
            }
            None => {
                self.open = Some(OpenChunk {
                    start: block.start,
                    chars,
                    only_heading: block.heading,
                })
            }
        }
    }

    fn would_overflow(&self, chars: usize) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.chars + chars > MAX_CHARS)
    }

    /// Cuts a block that does not fit in a chunk on its own. The heading right
    /// before it, if any, leads the first piece.
    fn cut(&mut self, block: &Block, end: usize) {
        let start = self.start_of_cut(block);
        let text = &self.body[start..end];
        self.chunks
            .extend(window::cut(text).into_iter().map(|range| Chunk {
                range: start + range.start..start + range.end,
            }));
    }

    fn start_of_cut(&mut self, block: &Block) -> usize {
        let lone_heading = self
            .open
            .take_if(|open| open.only_heading && !block.heading);
        if let Some(heading) = lone_heading {
            return heading.start;
        }
        self.close(block.start);
        block.start
    }

    fn close(&mut self, end: usize) {
        if let Some(open) = self.open.take() {
            self.chunks.push(Chunk {
                range: open.start..end,
            });
        }
    }

    fn finish(mut self) -> Vec<Chunk> {
        self.close(self.body.len());
        self.chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The texts of the chunks of `body`, having checked that they cover it.
    fn texts(body: &str) -> Vec<&str> {
        let chunks = chunks(body);
        assert_covers(body, &chunks);
        chunks.iter().map(|c| &body[c.range.clone()]).collect()
    }

    fn assert_covers(body: &str, chunks: &[Chunk]) {
        assert_eq!(chunks.first().unwrap().range.start, 0, "{chunks:?}");
        assert_eq!(chunks.last().unwrap().range.end, body.len(), "{chunks:?}");
        for pair in chunks.windows(2) {
            assert!(pair[0].range.start < pair[1].range.start, "{chunks:?}");
            assert!(pair[1].range.start <= pair[0].range.end, "{chunks:?}");
            assert!(pair[0].range.end <= pair[1].range.end, "{chunks:?}");
        }
        for chunk in chunks {
            assert!(body.is_char_boundary(chunk.range.start), "{chunks:?}");
            assert!(body.is_char_boundary(chunk.range.end), "{chunks:?}");
        }
    }

    fn paragraph(chars: usize) -> String {
        "あ".repeat(chars)
    }

    fn char_counts(texts: &[&str]) -> Vec<usize> {
        texts.iter().map(|t| t.chars().count()).collect()
    }

    #[test]
    fn an_empty_body_has_no_chunks() {
        assert_eq!(chunks(""), Vec::new());
    }

    #[test]
    fn a_short_document_is_one_chunk() {
        let body = "Some text.\n\nMore text.\n";

        assert_eq!(texts(body), vec![body]);
    }

    #[test]
    fn a_body_of_only_whitespace_is_one_chunk() {
        assert_eq!(texts(" \n\n\t"), vec![" \n\n\t"]);
    }

    #[test]
    fn a_body_of_only_link_reference_definitions_is_one_chunk() {
        assert_eq!(texts("[a]: /u\n"), vec!["[a]: /u\n"]);
    }

    #[test]
    fn every_heading_starts_a_chunk() {
        let body = "# A\n\nfirst\n\n## B\n\nsecond\n";

        assert_eq!(texts(body), vec!["# A\n\nfirst\n\n", "## B\n\nsecond\n"]);
    }

    #[test]
    fn what_comes_before_the_first_heading_is_a_chunk() {
        let body = "intro\n\n# A\n";

        assert_eq!(texts(body), vec!["intro\n\n", "# A\n"]);
    }

    #[test]
    fn a_heading_with_nothing_under_it_is_a_chunk() {
        let body = "# A\n## B\n";

        assert_eq!(texts(body), vec!["# A\n", "## B\n"]);
    }

    #[test]
    fn a_heading_underlined_with_equals_signs_starts_a_chunk() {
        let body = "intro\n\nTitle\n=====\n\ntext\n";

        assert_eq!(texts(body), vec!["intro\n\n", "Title\n=====\n\ntext\n"]);
    }

    #[test]
    fn line_endings_of_crlf_and_cr_alone_are_lines() {
        assert_eq!(
            texts("# A\r\n\r\nb\r\n\r\n# C\r\n"),
            vec!["# A\r\n\r\nb\r\n\r\n", "# C\r\n"]
        );
        assert_eq!(texts("# A\r\r# B\r"), vec!["# A\r\r", "# B\r"]);
    }

    #[test]
    fn blank_lines_before_the_first_block_go_with_it() {
        assert_eq!(texts("\n\n# A\n"), vec!["\n\n# A\n"]);
    }

    #[test]
    fn front_matter_stays_in_the_first_chunk() {
        let body = "---\ntitle: t\n---\n\ntext\n";

        assert_eq!(texts(body), vec![body]);
    }

    #[test]
    fn a_section_is_packed_up_to_the_width() {
        let (a, b, c) = (paragraph(300), paragraph(300), paragraph(300));
        let body = format!("{a}\n\n{b}\n\n{c}\n");

        assert_eq!(
            texts(&body),
            vec![format!("{a}\n\n{b}\n\n"), format!("{c}\n")]
        );
    }

    #[test]
    fn blocks_that_fill_the_width_exactly_stay_together() {
        let (a, b) = (paragraph(349), paragraph(348));
        let body = format!("{a}\n\n{b}\n");

        assert_eq!(texts(&body), vec![body.as_str()]);
    }

    #[test]
    fn a_block_that_would_go_over_the_width_starts_a_chunk() {
        let (a, b) = (paragraph(349), paragraph(349));
        let body = format!("{a}\n\n{b}\n");

        assert_eq!(texts(&body), vec![format!("{a}\n\n"), format!("{b}\n")]);
    }

    #[test]
    fn the_markup_counts_towards_the_width() {
        let (a, b) = (paragraph(300), paragraph(300));
        let url = "u".repeat(100);
        let body = format!("[{a}](/{url})\n\n{b}\n");

        assert_eq!(
            texts(&body),
            vec![format!("[{a}](/{url})\n\n"), format!("{b}\n")]
        );
    }

    #[test]
    fn whitespace_at_the_end_of_a_block_counts_towards_the_width() {
        let body = format!("{}\n\n\n\n", paragraph(700));

        assert_eq!(char_counts(&texts(&body)), vec![700, 74]);
    }

    #[test]
    fn list_items_are_blocks() {
        let (a, b) = (paragraph(400), paragraph(400));
        let body = format!("- {a}\n- {b}\n");

        assert_eq!(texts(&body), vec![format!("- {a}\n"), format!("- {b}\n")]);
    }

    #[test]
    fn the_blocks_in_a_block_quote_start_where_their_lines_do() {
        let (a, b) = (paragraph(400), paragraph(400));
        let body = format!("> {a}\n>\n> {b}\n");

        assert_eq!(
            texts(&body),
            vec![format!("> {a}\n>\n"), format!("> {b}\n")]
        );
    }

    #[test]
    fn a_heading_in_a_block_quote_starts_a_chunk_at_the_quote() {
        let body = "intro\n\n> # A\n>\n> text\n";

        assert_eq!(texts(body), vec!["intro\n\n", "> # A\n>\n> text\n"]);
    }

    #[test]
    fn a_code_block_is_one_block_and_what_looks_like_a_heading_in_it_is_not_one() {
        let code = format!("```\n# not a heading\n\n{}\n```", paragraph(400));
        let text = paragraph(400);
        let body = format!("{code}\n\n{text}\n");

        assert_eq!(
            texts(&body),
            vec![format!("{code}\n\n"), format!("{text}\n")]
        );
    }

    #[test]
    fn a_thematic_break_is_a_block() {
        let (a, b) = (paragraph(698), paragraph(10));
        let body = format!("{a}\n\n---\n\n{b}\n");

        assert_eq!(
            texts(&body),
            vec![format!("{a}\n\n"), format!("---\n\n{b}\n")]
        );
    }

    #[test]
    fn inline_markup_on_a_later_line_of_a_paragraph_is_not_a_block() {
        let body = format!("{}\n*{}*\n", paragraph(400), paragraph(400));

        let texts = texts(&body);

        assert_eq!(char_counts(&texts), vec![700, 174]);
    }

    #[test]
    fn a_block_over_the_width_is_cut_with_overlap() {
        let body = paragraph(1500);

        let texts = texts(&body);

        assert_eq!(char_counts(&texts), vec![700, 700, 240]);
    }

    #[test]
    fn a_heading_leads_the_first_piece_of_a_long_block_under_it() {
        let body = format!("# T\n\n{}\n", paragraph(1500));

        let texts = texts(&body);

        assert!(texts[0].starts_with("# T\n\nあ"), "{:?}", texts[0]);
        assert_eq!(texts[0].chars().count(), 700);
    }

    #[test]
    fn a_heading_and_a_block_within_the_width_that_do_not_fit_together_are_two_chunks() {
        let text = paragraph(698);
        let body = format!("# T\n\n{text}\n");

        assert_eq!(
            texts(&body),
            vec!["# T\n\n".to_owned(), format!("{text}\n")]
        );
    }

    #[test]
    fn only_the_heading_right_before_a_long_block_leads_its_first_piece() {
        let body = format!("# A\n## B\n\n{}\n", paragraph(1500));

        let texts = texts(&body);

        assert_eq!(texts[0], "# A\n");
        assert!(texts[1].starts_with("## B\n\nあ"), "{:?}", texts[1]);
    }

    #[test]
    fn a_long_block_after_other_blocks_under_a_heading_starts_its_own_chunks() {
        let body = format!("# T\n\nshort\n\n{}\n", paragraph(800));

        let texts = texts(&body);

        assert_eq!(texts[0], "# T\n\nshort\n\n");
        assert!(texts[1].starts_with('あ'), "{:?}", texts[1]);
    }

    #[test]
    fn the_blocks_around_a_long_block_are_chunks_of_their_own() {
        let body = format!("before\n\n{}\n\nafter\n", paragraph(800));

        let texts = texts(&body);

        assert_eq!(texts.first(), Some(&"before\n\n"));
        assert_eq!(texts.last(), Some(&"after\n"));
        assert_eq!(texts.len(), 4);
    }

    #[test]
    fn no_chunk_goes_over_the_width() {
        let long_lines = format!("{}\n", "line of text\n".repeat(300));
        let body = format!(
            "# A\n\n{}\n\n- item\n- {}\n\n```\n{}\n```\n\n## B\n",
            paragraph(650),
            paragraph(900),
            long_lines
        );

        for text in texts(&body) {
            assert!(text.chars().count() <= MAX_CHARS, "{text:?}");
        }
    }

    /// pulldown-cmark 0.13.4's iterator with offsets panicked on these: a list
    /// item holding only a link reference definition, followed by a line of
    /// whitespace indented at least four columns past the item's text.
    #[test]
    fn documents_that_made_the_parser_panic_are_read_in_full() {
        for body in [
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
        ] {
            texts(body);
        }
    }
}
