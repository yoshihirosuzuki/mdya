//! Render `SearchResponse` to stdout. The search engines only have
//! to assemble hits — they never re-implement formatting.
//!
//! Doc-level is the default `SearchHit` granularity, so each renderer
//! dispatches on the `SearchHit::{Doc, Chunk}` variant for the
//! granularity-specific field (`matched_chunks` vs `chunk_sequence`)
//! while reusing the shared accessors ([`SearchHit::collection`] /
//! `path` / `score` / `snippet`) for the fields both variants carry.

use std::io::{self, Write};

use super::response::{SearchHit, SearchResponse};

/// ANSI escape codes for the colored header. Kept dependency-free —
/// the surface is small enough that pulling `owo-colors` or `nu-ansi-term`
/// is not worth the audit.
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_YELLOW: &str = "\x1b[33m";

/// Render the response in the human-readable format.
///
/// `use_color` is the resolved colorise decision — the caller applies
/// the precedence (`--no-color` > `NO_COLOR` env >
/// `stderr.is_terminal()`) and passes the final boolean. The writer is
/// generic so unit tests can capture output into a `Vec<u8>` without
/// touching process stdout. The summary line ends in `"{level} hits"`
/// (e.g. `"17 doc hits (showing 20 max)"`) so a reader can tell the
/// granularity from the line itself without consulting the envelope.
pub fn print_human<W: Write>(
    writer: &mut W,
    resp: &SearchResponse,
    use_color: bool,
) -> io::Result<()> {
    for hit in &resp.hits {
        write_hit_header(writer, hit.collection(), hit.path(), hit.score(), use_color)?;
        for line in hit.snippet().lines() {
            writeln!(writer, "  > {line}")?;
        }
        writeln!(writer, "---")?;
    }
    writeln!(
        writer,
        "{} {} hits (showing {} max)",
        resp.total,
        resp.level.as_str(),
        resp.limit
    )?;
    Ok(())
}

fn write_hit_header<W: Write>(
    writer: &mut W,
    collection: &str,
    path: &str,
    score: f32,
    use_color: bool,
) -> io::Result<()> {
    if use_color {
        writeln!(
            writer,
            "{ANSI_CYAN}{collection}/{path}{ANSI_RESET}  {ANSI_YELLOW}score={score:.3}{ANSI_RESET}"
        )
    } else {
        writeln!(writer, "{collection}/{path}  score={score:.3}")
    }
}

/// Render the response as the JSON envelope. Compact (no pretty-
/// print) so a single line can be piped to `jq` without extra parsing
/// rules. `serde` handles the untagged-enum dispatch for hits — `Doc`
/// and `Chunk` variants serialise as distinct field sets so a consumer
/// can tell them apart by field presence (anyOf schema, see
/// `schemars_derive-1.2.1/src/schema_exprs.rs:389-391`).
pub fn print_json<W: Write>(writer: &mut W, resp: &SearchResponse) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, resp)?;
    writeln!(writer)?;
    Ok(())
}

/// Render the response as Markdown. A human/agent-facing
/// *presentation* view: an envelope preamble followed
/// by one `## collection/path` section per hit, with the snippet as a
/// blockquote so multi-line snippets stay readable (a table would have to
/// collapse newlines into `<br>`). `score` is rounded to 3 decimals like
/// the human format — Markdown is for reading, not lossless machine
/// parsing (use `json` / `xml` for that), so field values are emitted
/// without Markdown escaping. The granularity-specific field is
/// `matched_chunks` for `SearchHit::Doc` (breadth signal) or
/// `chunk_sequence` for `SearchHit::Chunk` (passage locator).
pub fn print_md<W: Write>(writer: &mut W, resp: &SearchResponse) -> io::Result<()> {
    writeln!(writer, "# Search results")?;
    writeln!(writer)?;
    writeln!(writer, "- query: `{}`", resp.query)?;
    writeln!(writer, "- mode: {}", resp.mode.as_str())?;
    writeln!(writer, "- level: {}", resp.level.as_str())?;
    writeln!(
        writer,
        "- collections: {}",
        format_collections(&resp.collections)
    )?;
    writeln!(
        writer,
        "- {} {} hits (showing {} max)",
        resp.total,
        resp.level.as_str(),
        resp.limit
    )?;
    for hit in &resp.hits {
        writeln!(writer)?;
        writeln!(writer, "## {}/{}", hit.collection(), hit.path())?;
        writeln!(writer)?;
        write_md_granularity_fields(writer, hit)?;
        writeln!(writer, "- score: {:.3}", hit.score())?;
        writeln!(writer)?;
        for line in hit.snippet().lines() {
            writeln!(writer, "> {line}")?;
        }
    }
    Ok(())
}

/// Write the granularity-specific bullet (`matched_chunks` for Doc,
/// `chunk_sequence` for Chunk) so each Markdown hit section makes the
/// hit's granularity explicit without inspecting the envelope.
fn write_md_granularity_fields<W: Write>(writer: &mut W, hit: &SearchHit) -> io::Result<()> {
    match hit {
        SearchHit::Doc { matched_chunks, .. } => {
            writeln!(writer, "- matched_chunks: {matched_chunks}")
        }
        SearchHit::Chunk { chunk_sequence, .. } => {
            writeln!(writer, "- chunk_sequence: {chunk_sequence}")
        }
    }
}

/// Render the response as XML, a lossless 1:1 mirror of the JSON
/// envelope: every JSON key becomes a child
/// element (no attributes), so text content only ever needs the three
/// element-content entities (`&` / `<` / `>`) — `"` / `'` matter only
/// inside attribute values, which this layout never produces. `score` is
/// the raw `f32` (machine-faithful, like JSON), not the human-rounded
/// form. Each `<hit>` carries the granularity-specific element
/// (`<chunk_sequence>` for `SearchHit::Chunk`, `<matched_chunks>` for
/// `SearchHit::Doc`) in the same position the JSON has the field, so
/// the two encodings stay byte-aligned modulo the XML markup overhead.
pub fn print_xml<W: Write>(writer: &mut W, resp: &SearchResponse) -> io::Result<()> {
    writeln!(writer, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(writer, "<response>")?;
    writeln!(writer, "  <query>{}</query>", xml_escape(&resp.query))?;
    writeln!(writer, "  <mode>{}</mode>", resp.mode.as_str())?;
    writeln!(writer, "  <level>{}</level>", resp.level.as_str())?;
    write_xml_collections(writer, &resp.collections)?;
    writeln!(writer, "  <limit>{}</limit>", resp.limit)?;
    writeln!(writer, "  <total>{}</total>", resp.total)?;
    write_xml_hits(writer, &resp.hits)?;
    writeln!(writer, "</response>")?;
    Ok(())
}

fn write_xml_collections<W: Write>(writer: &mut W, collections: &[String]) -> io::Result<()> {
    if collections.is_empty() {
        return writeln!(writer, "  <collections/>");
    }
    writeln!(writer, "  <collections>")?;
    for collection in collections {
        writeln!(
            writer,
            "    <collection>{}</collection>",
            xml_escape(collection)
        )?;
    }
    writeln!(writer, "  </collections>")
}

fn write_xml_hits<W: Write>(writer: &mut W, hits: &[SearchHit]) -> io::Result<()> {
    if hits.is_empty() {
        return writeln!(writer, "  <hits/>");
    }
    writeln!(writer, "  <hits>")?;
    for hit in hits {
        write_xml_hit(writer, hit)?;
    }
    writeln!(writer, "  </hits>")
}

fn write_xml_hit<W: Write>(writer: &mut W, hit: &SearchHit) -> io::Result<()> {
    writeln!(writer, "    <hit>")?;
    writeln!(
        writer,
        "      <collection>{}</collection>",
        xml_escape(hit.collection())
    )?;
    writeln!(writer, "      <path>{}</path>", xml_escape(hit.path()))?;
    if let SearchHit::Chunk { chunk_sequence, .. } = hit {
        writeln!(
            writer,
            "      <chunk_sequence>{chunk_sequence}</chunk_sequence>"
        )?;
    }
    // Serialize `score` through the same path as the JSON envelope so XML
    // stays a byte-for-byte mirror: `serde_json` and `f32::Display` happen
    // to agree today, but routing both through serde makes the parity
    // structural instead of coincidental. `to_string` never errors for an
    // `f32` (non-finite serializes as `null`).
    let score = serde_json::to_string(&hit.score())
        .expect("serde_json serializes an f32 as a JSON number or null, never erroring");
    writeln!(writer, "      <score>{score}</score>")?;
    writeln!(
        writer,
        "      <snippet>{}</snippet>",
        xml_escape(hit.snippet())
    )?;
    if let SearchHit::Doc { matched_chunks, .. } = hit {
        writeln!(
            writer,
            "      <matched_chunks>{matched_chunks}</matched_chunks>"
        )?;
    }
    writeln!(writer, "    </hit>")
}

/// Join the collection filter for the Markdown preamble. An empty filter
/// means "every collection" (0 values = all), rendered as `(all)` rather
/// than a blank so the line stays meaningful.
fn format_collections(collections: &[String]) -> String {
    if collections.is_empty() {
        return "(all)".to_string();
    }
    collections.join(", ")
}

/// Escape the three XML entities that are mandatory in element content.
/// `&` MUST be replaced first, otherwise the `&` introduced by `<` → `&lt;`
/// would itself be re-escaped into `&amp;lt;`. Shared with `introspect::output`
/// so the `--format xml` escaping has a single tested implementation.
pub(crate) fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::response::{SearchLevel, SearchMode};

    fn doc_hit(collection: &str, path: &str, score: f32, snippet: &str, matched: u32) -> SearchHit {
        SearchHit::Doc {
            collection: collection.to_string(),
            path: path.to_string(),
            score,
            snippet: snippet.to_string(),
            matched_chunks: matched,
        }
    }

    fn chunk_hit(
        collection: &str,
        path: &str,
        chunk_sequence: u32,
        score: f32,
        snippet: &str,
    ) -> SearchHit {
        SearchHit::Chunk {
            collection: collection.to_string(),
            path: path.to_string(),
            chunk_sequence,
            score,
            snippet: snippet.to_string(),
        }
    }

    fn doc_response_with_two_hits() -> SearchResponse {
        SearchResponse {
            query: "release checklist".to_string(),
            mode: SearchMode::Hybrid,
            level: SearchLevel::Doc,
            collections: vec!["notes".to_string(), "work".to_string()],
            limit: 20,
            total: 17,
            hits: vec![
                doc_hit("notes", "foo.md", 0.812, "matching text\nsecond line", 3),
                doc_hit("work", "bar.md", 0.751, "another snippet", 1),
            ],
        }
    }

    fn chunk_response_with_two_hits() -> SearchResponse {
        SearchResponse {
            query: "release checklist".to_string(),
            mode: SearchMode::Hybrid,
            level: SearchLevel::Chunk,
            collections: vec!["notes".to_string(), "work".to_string()],
            limit: 20,
            total: 17,
            hits: vec![
                chunk_hit("notes", "foo.md", 3, 0.812, "matching text\nsecond line"),
                chunk_hit("work", "bar.md", 0, 0.751, "another snippet"),
            ],
        }
    }

    fn empty_response(level: SearchLevel) -> SearchResponse {
        SearchResponse {
            query: "no-results".to_string(),
            mode: SearchMode::Fts,
            level,
            collections: vec![],
            limit: 20,
            total: 0,
            hits: vec![],
        }
    }

    fn render_to_string<F>(render: F) -> String
    where
        F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
    {
        let mut buf = Vec::new();
        render(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn human_format_doc_level_summary_uses_doc_unit() {
        let mut buf = Vec::new();
        print_human(&mut buf, &doc_response_with_two_hits(), false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s,
            "notes/foo.md  score=0.812\n  \
             > matching text\n  \
             > second line\n---\n\
             work/bar.md  score=0.751\n  \
             > another snippet\n---\n\
             17 doc hits (showing 20 max)\n"
        );
    }

    #[test]
    fn human_format_chunk_level_summary_uses_chunk_unit() {
        // Same hit layout as the doc test on purpose: only the summary
        // word changes when `level` flips. If a future refactor leaks
        // chunk-only fields (e.g. `chunk_sequence`) into the human
        // header this assertion catches it because it pins the exact
        // bytes the renderer emits.
        let mut buf = Vec::new();
        print_human(&mut buf, &chunk_response_with_two_hits(), false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(
            s,
            "notes/foo.md  score=0.812\n  \
             > matching text\n  \
             > second line\n---\n\
             work/bar.md  score=0.751\n  \
             > another snippet\n---\n\
             17 chunk hits (showing 20 max)\n"
        );
    }

    #[test]
    fn human_format_with_zero_hits_only_emits_summary_line() {
        let mut buf = Vec::new();
        print_human(&mut buf, &empty_response(SearchLevel::Doc), false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s, "0 doc hits (showing 20 max)\n");
    }

    #[test]
    fn human_format_with_color_wraps_header_fields_in_ansi_sgr() {
        let mut buf = Vec::new();
        print_human(&mut buf, &doc_response_with_two_hits(), true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\x1b[36mnotes/foo.md\x1b[0m"));
        assert!(s.contains("\x1b[33mscore=0.812\x1b[0m"));
        // The summary footer must stay plain — color is for hit headers only.
        assert!(s.ends_with("17 doc hits (showing 20 max)\n"));
    }

    #[test]
    fn json_format_emits_canonical_envelope_with_trailing_newline() {
        let mut buf = Vec::new();
        print_json(&mut buf, &doc_response_with_two_hits()).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        let parsed: SearchResponse = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(parsed, doc_response_with_two_hits());
        assert!(s.contains("\"mode\":\"hybrid\""));
        assert!(s.contains("\"level\":\"doc\""));
        // Doc-level hits carry `matched_chunks` and not `chunk_sequence`.
        assert!(s.contains("\"matched_chunks\":3"));
        assert!(!s.contains("\"chunk_sequence\""));
    }

    #[test]
    fn json_format_chunk_level_hits_carry_chunk_sequence_not_matched_chunks() {
        let mut buf = Vec::new();
        print_json(&mut buf, &chunk_response_with_two_hits()).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("\"level\":\"chunk\""));
        assert!(s.contains("\"chunk_sequence\":3"));
        assert!(!s.contains("\"matched_chunks\""));
    }

    #[test]
    fn md_format_doc_level_with_hits_matches_section_layout() {
        let s = render_to_string(|w| print_md(w, &doc_response_with_two_hits()));
        let expected = r#"# Search results

- query: `release checklist`
- mode: hybrid
- level: doc
- collections: notes, work
- 17 doc hits (showing 20 max)

## notes/foo.md

- matched_chunks: 3
- score: 0.812

> matching text
> second line

## work/bar.md

- matched_chunks: 1
- score: 0.751

> another snippet
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn md_format_chunk_level_with_hits_matches_section_layout() {
        let s = render_to_string(|w| print_md(w, &chunk_response_with_two_hits()));
        let expected = r#"# Search results

- query: `release checklist`
- mode: hybrid
- level: chunk
- collections: notes, work
- 17 chunk hits (showing 20 max)

## notes/foo.md

- chunk_sequence: 3
- score: 0.812

> matching text
> second line

## work/bar.md

- chunk_sequence: 0
- score: 0.751

> another snippet
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn md_format_with_zero_hits_emits_preamble_only_with_all_collections() {
        let s = render_to_string(|w| print_md(w, &empty_response(SearchLevel::Doc)));
        let expected = r#"# Search results

- query: `no-results`
- mode: fts
- level: doc
- collections: (all)
- 0 doc hits (showing 20 max)
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn xml_format_doc_level_with_hits_mirrors_json_envelope() {
        let s = render_to_string(|w| print_xml(w, &doc_response_with_two_hits()));
        let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<response>
  <query>release checklist</query>
  <mode>hybrid</mode>
  <level>doc</level>
  <collections>
    <collection>notes</collection>
    <collection>work</collection>
  </collections>
  <limit>20</limit>
  <total>17</total>
  <hits>
    <hit>
      <collection>notes</collection>
      <path>foo.md</path>
      <score>0.812</score>
      <snippet>matching text
second line</snippet>
      <matched_chunks>3</matched_chunks>
    </hit>
    <hit>
      <collection>work</collection>
      <path>bar.md</path>
      <score>0.751</score>
      <snippet>another snippet</snippet>
      <matched_chunks>1</matched_chunks>
    </hit>
  </hits>
</response>
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn xml_format_chunk_level_with_hits_mirrors_json_envelope() {
        let s = render_to_string(|w| print_xml(w, &chunk_response_with_two_hits()));
        let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<response>
  <query>release checklist</query>
  <mode>hybrid</mode>
  <level>chunk</level>
  <collections>
    <collection>notes</collection>
    <collection>work</collection>
  </collections>
  <limit>20</limit>
  <total>17</total>
  <hits>
    <hit>
      <collection>notes</collection>
      <path>foo.md</path>
      <chunk_sequence>3</chunk_sequence>
      <score>0.812</score>
      <snippet>matching text
second line</snippet>
    </hit>
    <hit>
      <collection>work</collection>
      <path>bar.md</path>
      <chunk_sequence>0</chunk_sequence>
      <score>0.751</score>
      <snippet>another snippet</snippet>
    </hit>
  </hits>
</response>
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn xml_format_with_zero_hits_uses_self_closing_empty_elements() {
        let s = render_to_string(|w| print_xml(w, &empty_response(SearchLevel::Doc)));
        let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<response>
  <query>no-results</query>
  <mode>fts</mode>
  <level>doc</level>
  <collections/>
  <limit>20</limit>
  <total>0</total>
  <hits/>
</response>
"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn xml_escape_replaces_the_three_element_content_entities() {
        assert_eq!(xml_escape("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        // `&` is escaped first, so the `&` inside `&lt;` is not double-escaped.
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        // Order proof: a literal `&lt;` must become `&amp;lt;`. If `<` were
        // replaced before `&`, the `&` from the first pass would be re-escaped
        // and this would wrongly stay `&lt;`.
        assert_eq!(xml_escape("a&lt;b"), "a&amp;lt;b");
    }

    #[test]
    fn xml_format_escapes_special_chars_in_field_values() {
        let resp = SearchResponse {
            query: "a & b".to_string(),
            mode: SearchMode::Fts,
            level: SearchLevel::Doc,
            collections: vec![],
            limit: 20,
            total: 1,
            hits: vec![doc_hit("notes", "x.md", 0.5, "a < b > c & <tag>", 1)],
        };
        let s = render_to_string(|w| print_xml(w, &resp));
        assert!(s.contains("<query>a &amp; b</query>"));
        assert!(s.contains("<snippet>a &lt; b &gt; c &amp; &lt;tag&gt;</snippet>"));
        // The raw, unescaped form must never reach the output.
        assert!(!s.contains("<snippet>a < b"));
    }

    fn single_doc_hit_response(snippet: &str, score: f32) -> SearchResponse {
        SearchResponse {
            query: "q".to_string(),
            mode: SearchMode::Fts,
            level: SearchLevel::Doc,
            collections: vec![],
            limit: 5,
            total: 1,
            hits: vec![doc_hit("n", "a.md", score, snippet, 1)],
        }
    }

    #[test]
    fn md_format_empty_snippet_hit_emits_no_blockquote() {
        let s = render_to_string(|w| print_md(w, &single_doc_hit_response("", 0.5)));
        let expected = r#"# Search results

- query: `q`
- mode: fts
- level: doc
- collections: (all)
- 1 doc hits (showing 5 max)

## n/a.md

- matched_chunks: 1
- score: 0.500

"#;
        assert_eq!(s, expected);
    }

    #[test]
    fn xml_format_empty_snippet_uses_empty_element() {
        let s = render_to_string(|w| print_xml(w, &single_doc_hit_response("", 0.5)));
        assert!(s.contains("<snippet></snippet>"));
    }

    #[test]
    fn xml_score_is_byte_identical_to_json_serialization() {
        // 1/3 is the kind of value where a float printer's choices matter;
        // the XML `<score>` must match what the JSON envelope emits exactly.
        let score = 1.0f32 / 3.0f32;
        let resp = single_doc_hit_response("s", score);
        let xml = render_to_string(|w| print_xml(w, &resp));
        let json_score = serde_json::to_string(&score).unwrap();
        assert!(
            xml.contains(&format!("<score>{json_score}</score>")),
            "xml did not contain JSON-identical score {json_score}:\n{xml}"
        );
    }
}
