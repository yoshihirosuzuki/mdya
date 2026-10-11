//! Rules for the path a document is indexed under that hold for directory
//! and archive collections alike, and how a name that breaks them is shown.
//!
//! A path containing a control character (Unicode general category Cc:
//! U+0000–U+001F, U+007F and U+0080–U+009F) is not indexed. Paths are
//! printed as they are in log lines, progress and search results, where a
//! control character can move the cursor or recolor the terminal, and XML
//! 1.0 cannot carry most of those below U+0020 at all. Rejecting them on
//! the way in keeps them out of every output (an entry an earlier version
//! indexed is dropped as an orphan on the next run), and an indexed path
//! stays one that can be typed back into `mdya get`.

use std::path::PathBuf;

/// Why a document's name cannot be used as a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameProblem {
    NotUtf8,
    Absolute,
    ParentDir,
    ControlCharacter,
}

impl std::fmt::Display for NameProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotUtf8 => "name is not valid UTF-8",
            Self::Absolute => "name is an absolute path",
            Self::ParentDir => "name contains a `..` segment",
            Self::ControlCharacter => "name contains a control character",
        })
    }
}

/// A document left out because of its name, counted as failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RejectedDocument {
    /// The name as [`shown_name`] makes it safe to log.
    pub(crate) name: String,
    pub(crate) problem: NameProblem,
}

/// Whether `path` holds a control character, which keeps its document out
/// of the index (see the module docs).
pub(crate) fn has_control_character(path: &str) -> bool {
    path.chars().any(char::is_control)
}

/// Split the paths of a directory walk into those that can be indexed and
/// those rejected for a control character, each in the order given.
pub(crate) fn reject_control_characters(
    paths: Vec<PathBuf>,
) -> (Vec<PathBuf>, Vec<RejectedDocument>) {
    let (rejected, kept): (Vec<PathBuf>, Vec<PathBuf>) = paths
        .into_iter()
        .partition(|path| has_control_character(&path.to_string_lossy()));
    let rejected = rejected
        .iter()
        .map(|path| RejectedDocument {
            name: shown_name(path.to_string_lossy().as_bytes()),
            problem: NameProblem::ControlCharacter,
        })
        .collect();
    (kept, rejected)
}

/// `text` with each control character written as its Rust escape (`\n`,
/// `\u{1b}`), for an error message about to be logged: a field passed with
/// `%` is written to the terminal as it is, and the message may come from
/// code whose wording mdya does not control (the PDF library, the
/// decompressor, the file system), so nothing guarantees that it quotes no
/// text read from the input.
pub(crate) fn escape_control_characters(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        push_escaped(&mut escaped, c);
    }
    escaped
}

fn push_escaped(out: &mut String, c: char) {
    if c.is_control() {
        out.extend(c.escape_default());
    } else {
        out.push(c);
    }
}

/// How many characters of a name [`shown_name`] keeps.
const MAX_SHOWN_NAME_CHARS: usize = 256;

/// A name as it can go into a log line: invalid UTF-8 replaced, control
/// characters escaped as [`escape_control_characters`] does, and cut after
/// [`MAX_SHOWN_NAME_CHARS`] characters. Only the start of the name is
/// decoded: a character takes at most 4 bytes, so these hold more than the
/// characters kept, however long the name.
pub(crate) fn shown_name(raw: &[u8]) -> String {
    let start = &raw[..raw.len().min((MAX_SHOWN_NAME_CHARS + 1) * 4)];
    let mut shown = String::new();
    for (count, c) in String::from_utf8_lossy(start).chars().enumerate() {
        if count == MAX_SHOWN_NAME_CHARS {
            shown.push('…');
            break;
        }
        push_escaped(&mut shown, c);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_the_unicode_cc_category() {
        for path in [
            "a\0.md",
            "a\t.md",
            "a\n.md",
            "a\x1b[31m.md",
            "a\x7f.md",
            "a\u{9b}.md",
        ] {
            assert!(has_control_character(path), "{path:?}");
        }
        // Format characters such as a right-to-left override are not Cc.
        for path in ["docs/a.md", "メモ.md", "a\u{202e}.md", "a\u{a0}.md"] {
            assert!(!has_control_character(path), "{path:?}");
        }
    }

    #[test]
    fn walked_paths_with_control_characters_are_rejected_in_order() {
        let (kept, rejected) = reject_control_characters(
            ["a.md", "b\x1b.md", "c.md", "d\n.md"]
                .map(PathBuf::from)
                .to_vec(),
        );
        assert_eq!(kept, [PathBuf::from("a.md"), PathBuf::from("c.md")]);
        let names: Vec<&str> = rejected.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["b\\u{1b}.md", "d\\n.md"]);
        assert!(
            rejected
                .iter()
                .all(|r| r.problem == NameProblem::ControlCharacter)
        );
    }

    #[test]
    fn error_text_has_control_characters_escaped_and_is_not_cut() {
        let long = "あ".repeat(MAX_SHOWN_NAME_CHARS + 1);
        assert_eq!(
            escape_control_characters(&format!("bad size for a\x1b[2J.md {long}")),
            format!("bad size for a\\u{{1b}}[2J.md {long}")
        );
    }

    #[test]
    fn names_have_control_characters_escaped_and_are_cut_short() {
        assert_eq!(shown_name(b"a\n\x1b[31m.md"), "a\\n\\u{1b}[31m.md");
        assert_eq!(shown_name(b"\x82\xa0.md"), "\u{fffd}\u{fffd}.md");
        let long = shown_name("あ".repeat(MAX_SHOWN_NAME_CHARS + 1).as_bytes());
        assert_eq!(long, format!("{}…", "あ".repeat(MAX_SHOWN_NAME_CHARS)));
    }
}
