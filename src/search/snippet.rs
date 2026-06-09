//! Snippet extraction for `SearchHit::snippet`.
//!
//! Two strategies live here, dispatched per `SearchMode` by
//! `engine.rs`:
//!
//! - `extract_snippet` cuts a window around the first raw-substring
//!   occurrence of `query` in `body`. Used by FTS, where the query
//!   token tends to appear verbatim. When the FTS index hits on
//!   token-level matches that have no contiguous raw-substring (e.g.
//!   lindera/ipadic splits `"東京駅"` into `"東京"` + `"駅"` and the
//!   tokens land far apart in `body`), `find` returns `None` and we
//!   fall back to the body head.
//! - `extract_snippet_head` returns the body head verbatim and is used
//!   by vector / hybrid, where the query is semantic and rarely
//!   appears as a contiguous substring. With the
//!   `chunk = heading + section body` chunker, the head doubles as a
//!   display label for sub-chunk 0.

/// Per-hit snippet width, in `char`s (not bytes or grapheme clusters).
/// 60 chars fits roughly one sentence of Japanese kanji-density text
/// or about ten English words — enough for an LLM consumer to decide
/// whether to call `get_document` next, and short enough that a 20-hit
/// response stays well clear of the MCP output-token budget. The MCP
/// transport is the binding constraint here; the CLI inherits the
/// same width to keep CLI / MCP symmetric.
pub const DEFAULT_SNIPPET_CHARS: usize = 60;

const ELLIPSIS: char = '…';

/// Extract a single-line snippet from `body` around the first
/// occurrence of `query`. Returns at most `max_chars` characters
/// (plus optional leading/trailing `…` markers). Newlines inside the
/// window are replaced with single spaces so the rendered CLI line
/// stays on one row.
///
/// Used by FTS; vector / hybrid use [`extract_snippet_head`] instead.
pub fn extract_snippet(body: &str, query: &str, max_chars: usize) -> String {
    let trimmed_query = query.trim();
    let center_byte = if trimmed_query.is_empty() {
        0
    } else {
        body.find(trimmed_query).unwrap_or(0)
    };
    let window = window_around(body, center_byte, max_chars);
    let snippet = collapse_newlines(&body[window.start..window.end]);
    decorate(snippet, window.start > 0, window.end < body.len())
}

/// Return the first `max_chars` characters of `body` as a single-line
/// snippet (trailing `…` if the body was truncated). Newlines inside
/// the window are replaced with single spaces so the rendered CLI
/// line stays on one row. The body head doubles as the chunk's
/// display label for sub-chunk 0 because the chunker folds the
/// heading text into `body`'s first line.
///
/// Used by vector / hybrid; FTS uses [`extract_snippet`] to keep the
/// query token visible in the rendered snippet.
pub fn extract_snippet_head(body: &str, max_chars: usize) -> String {
    if body.is_empty() || max_chars == 0 {
        return String::new();
    }
    let end = walk_chars(body, 0, max_chars, Direction::Forward);
    let snippet = collapse_newlines(&body[..end]);
    decorate(snippet, false, end < body.len())
}

struct Window {
    start: usize,
    end: usize,
}

/// Compute byte offsets for a window of up to `max_chars` characters
/// centred on `center_byte`. Both endpoints are snapped to the
/// nearest char boundary by walking `char_indices`, so the slice we
/// hand back to `&body[..]` never panics on a multibyte split.
fn window_around(body: &str, center_byte: usize, max_chars: usize) -> Window {
    if body.is_empty() || max_chars == 0 {
        return Window { start: 0, end: 0 };
    }
    let half = max_chars / 2;
    let center = snap_to_char_boundary(body, center_byte);
    let start = walk_chars(body, center, half, Direction::Backward);
    let end = walk_chars(
        body,
        center,
        max_chars - chars_between(body, start, center),
        Direction::Forward,
    );
    Window { start, end }
}

/// Snap `byte` to the start byte of the char it lands in. `byte`
/// values already on a char boundary (as `str::find` guarantees)
/// round-trip unchanged; off-boundary inputs round down to the
/// previous char start, so the resulting offset is always safe to
/// hand to `&body[offset..]`.
fn snap_to_char_boundary(body: &str, byte: usize) -> usize {
    if byte >= body.len() {
        return body.len();
    }
    let mut last = 0;
    for (i, _) in body.char_indices() {
        if i > byte {
            return last;
        }
        last = i;
    }
    body.len()
}

#[derive(Clone, Copy)]
enum Direction {
    Forward,
    Backward,
}

fn walk_chars(body: &str, from: usize, count: usize, dir: Direction) -> usize {
    match dir {
        Direction::Forward => body[from..]
            .char_indices()
            .nth(count)
            .map(|(off, _)| from + off)
            .unwrap_or(body.len()),
        Direction::Backward => {
            let prefix = &body[..from];
            let total = prefix.chars().count();
            if count >= total {
                return 0;
            }
            let skip = total - count;
            prefix
                .char_indices()
                .nth(skip)
                .map(|(off, _)| off)
                .unwrap_or(0)
        }
    }
}

fn chars_between(body: &str, start: usize, end: usize) -> usize {
    body[start..end].chars().count()
}

fn collapse_newlines(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

fn decorate(mut snippet: String, lead: bool, trail: bool) -> String {
    if lead {
        snippet.insert(0, ELLIPSIS);
    }
    if trail {
        snippet.push(ELLIPSIS);
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_centres_window_on_first_query_occurrence() {
        // Body is long enough on both sides of the query so the window
        // truncates and both ellipsis markers fire.
        let body = "padding padding padding padding padding Release checklist \
                    padding padding padding padding padding extra extra extra.";
        let snippet = extract_snippet(body, "Release", 30);
        assert!(snippet.contains("Release"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn fallback_to_body_head_when_query_not_found_as_substring() {
        let body = "完全に別の文章でクエリは出てこない";
        let snippet = extract_snippet(body, "queryX", 20);
        // Falls back to the body head; the prefix ellipsis is absent.
        assert!(!snippet.starts_with('…'));
        assert!(snippet.starts_with("完全"));
    }

    #[test]
    fn snippet_replaces_newlines_with_spaces() {
        let body = "line1\nline2\r\nline3";
        let snippet = extract_snippet(body, "line2", 30);
        assert!(!snippet.contains('\n'));
        assert!(!snippet.contains('\r'));
        assert!(snippet.contains("line2"));
    }

    #[test]
    fn snippet_respects_multibyte_char_boundary() {
        // "日本語" is 9 bytes (3 chars × 3 bytes UTF-8). Force a window
        // that would naively cut mid-codepoint and assert no panic +
        // valid UTF-8.
        let body = "日本語のテキスト日本語のテキスト日本語のテキスト";
        let snippet = extract_snippet(body, "テキスト", 8);
        assert!(snippet.is_char_boundary(snippet.len()));
        assert!(snippet.contains("テキスト"));
    }

    #[test]
    fn empty_body_yields_empty_snippet() {
        let snippet = extract_snippet("", "query", 50);
        assert_eq!(snippet, "");
    }

    #[test]
    fn body_shorter_than_window_returns_body_without_ellipsis() {
        let snippet = extract_snippet("short body", "short", 200);
        assert_eq!(snippet, "short body");
    }

    #[test]
    fn head_returns_first_chars_with_trailing_ellipsis_when_truncated() {
        // 80-char body, ask for the head 30: trailing ellipsis fires,
        // leading ellipsis must NOT (the head is always at offset 0).
        let body = "padding padding padding padding padding padding \
                    padding padding padding padding";
        let snippet = extract_snippet_head(body, 30);
        assert!(!snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        // The trailing ellipsis adds one char; the head itself is 30.
        assert_eq!(snippet.chars().count(), 31);
    }

    #[test]
    fn head_returns_body_verbatim_when_shorter_than_window() {
        let snippet = extract_snippet_head("short body", 200);
        assert_eq!(snippet, "short body");
    }

    #[test]
    fn head_replaces_newlines_with_spaces() {
        let body = "# Heading\nFirst line\r\nSecond line";
        let snippet = extract_snippet_head(body, 200);
        assert!(!snippet.contains('\n'));
        assert!(!snippet.contains('\r'));
        assert!(snippet.starts_with("# Heading"));
    }

    #[test]
    fn head_respects_multibyte_char_boundary() {
        // Force a window that would naively cut mid-codepoint and
        // assert no panic + valid UTF-8.
        let body = "日本語のテキスト日本語のテキスト日本語のテキスト";
        let snippet = extract_snippet_head(body, 5);
        assert!(snippet.is_char_boundary(snippet.len()));
        // 5 chars taken from the head are "日本語のテ"; the rest is
        // truncated, so the trailing ellipsis must be present.
        assert!(snippet.starts_with("日本語のテ"));
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn head_yields_empty_string_for_empty_body() {
        assert_eq!(extract_snippet_head("", 50), "");
    }

    #[test]
    fn head_yields_empty_string_for_zero_max_chars() {
        // Symmetry with `extract_snippet`: a zero-width window can
        // only ever be empty, no ellipsis decoration applied.
        assert_eq!(extract_snippet_head("non-empty body", 0), "");
    }
}
