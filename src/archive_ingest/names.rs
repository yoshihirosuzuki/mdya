//! Which entries of an archive become documents, and under which key. Names
//! are worked on as bytes and strings, never as a `Path`, so that the result
//! is the same on every operating system.

use std::str::{Utf8Error, from_utf8};

use crate::format::{FileType, Format};
use crate::types::{BoxError, Key};

/// What an entry of the archive is to the update.
pub(super) enum EntryRole {
    NotADocument,
    /// A document that cannot be read, as its name is not UTF-8.
    WithoutKey {
        /// The name to show in the warning.
        name: String,
        cause: BoxError,
    },
    Document {
        key: Key,
        file_type: FileType,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("the name of the entry is not valid UTF-8")]
struct NameNotUtf8(#[source] Utf8Error);

/// Decides what an entry is. Whether it is a document at all comes before
/// reading its name as UTF-8, so that an image with a name in Shift_JIS, say,
/// is not reported as a document that cannot be read.
pub(super) fn role_of(name: &[u8], regular: bool, format: &dyn Format) -> EntryRole {
    if !regular {
        return EntryRole::NotADocument;
    }
    let Some(file_type) = file_type_of(name, format) else {
        return EntryRole::NotADocument;
    };
    if is_macos_metadata(name) {
        return EntryRole::NotADocument;
    }
    match from_utf8(name) {
        Ok(name) => EntryRole::Document {
            key: key_of(name),
            file_type,
        },
        Err(error) => EntryRole::WithoutKey {
            name: String::from_utf8_lossy(name).into_owned(),
            cause: Box::new(NameNotUtf8(error)),
        },
    }
}

/// The part of the name after its last separator. A backslash separates only
/// in a name that is UTF-8: in Shift_JIS, the byte 0x5C can be the second half
/// of a character.
fn last_segment(name: &[u8]) -> &[u8] {
    let backslash_separates = from_utf8(name).is_ok();
    let is_separator = |byte: &u8| *byte == b'/' || (backslash_separates && *byte == b'\\');
    name.rsplit(is_separator).next().unwrap_or(name)
}

/// The file type that the extension of the name stands for. An extension that
/// is not UTF-8 stands for none.
fn file_type_of(name: &[u8], format: &dyn Format) -> Option<FileType> {
    let segment = last_segment(name);
    let dot = segment.iter().rposition(|byte| *byte == b'.')?;
    let extension = from_utf8(segment.get(dot + 1..)?).ok()?;
    format.file_type_from_extension(extension)
}

/// Whether the entry is one of those macOS adds. Looked at in bytes, so that
/// the entries with a name that is not UTF-8 are caught as well. In the top
/// level a backslash always separates: after the ASCII `__MACOSX` it cannot be
/// the second half of a Shift_JIS character.
fn is_macos_metadata(name: &[u8]) -> bool {
    let top_segment = name
        .split(|byte| matches!(byte, b'/' | b'\\'))
        .find(|segment| !is_empty_or_dot(segment));
    last_segment(name).starts_with(b"._") || top_segment == Some(b"__MACOSX".as_slice())
}

fn is_empty_or_dot(segment: &[u8]) -> bool {
    segment.is_empty() || segment == b"."
}

fn key_of(name: &str) -> Key {
    let segments: Vec<&str> = name
        .split(['/', '\\'])
        .filter(|segment| !is_empty_or_dot(segment.as_bytes()))
        .collect();
    Key(segments.join("/"))
}
