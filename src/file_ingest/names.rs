//! What the path of a file says about it: its extension and its key.

use std::ffi::OsStr;
use std::path::Path;

use crate::types::Key;

/// The part of a file name after its last `.`, worked out from the name's
/// bytes so that a name that is not valid UTF-8 can still have an extension.
/// `None` when the name has no `.`, or when that part is not valid UTF-8.
pub(super) fn extension_of(name: &OsStr) -> Option<&str> {
    let bytes = name.as_encoded_bytes();
    let last_dot = bytes.iter().rposition(|byte| *byte == b'.')?;
    std::str::from_utf8(&bytes[last_dot + 1..]).ok()
}

/// The key of a file, from its path relative to the directory: the components
/// joined with `/`. `None` when a component is not valid UTF-8.
pub(super) fn key_of(relative: &Path) -> Option<Key> {
    let components: Option<Vec<&str>> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    Some(Key(components?.join("/")))
}

/// A relative path as text for a warning. What is not valid UTF-8 becomes
/// U+FFFD.
pub(super) fn shown_name(relative: &Path) -> String {
    relative.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension(name: &str) -> Option<&str> {
        extension_of(OsStr::new(name))
    }

    #[test]
    fn the_extension_is_what_follows_the_last_dot() {
        assert_eq!(extension("a.md"), Some("md"));
        assert_eq!(extension("a.tar.pdf"), Some("pdf"));
        assert_eq!(extension("A.MD"), Some("MD"));
    }

    #[test]
    fn a_name_that_only_starts_with_a_dot_has_that_extension() {
        assert_eq!(extension(".md"), Some("md"));
    }

    #[test]
    fn a_name_ending_in_a_dot_has_an_empty_extension() {
        assert_eq!(extension("a."), Some(""));
    }

    #[test]
    fn a_name_without_a_dot_has_no_extension() {
        assert_eq!(extension("README"), None);
    }

    #[test]
    fn the_key_joins_the_components_with_a_slash() {
        let relative = Path::new("sub").join("deep").join("a.md");

        assert_eq!(key_of(&relative), Some(Key("sub/deep/a.md".to_owned())));
    }

    #[test]
    fn a_file_directly_in_the_directory_has_its_name_as_its_key() {
        assert_eq!(key_of(Path::new("a.md")), Some(Key("a.md".to_owned())));
    }

    #[cfg(unix)]
    mod not_utf8 {
        use std::os::unix::ffi::OsStrExt;

        use super::*;

        fn bytes(name: &[u8]) -> &OsStr {
            OsStr::from_bytes(name)
        }

        #[test]
        fn a_name_that_is_not_utf8_still_has_its_extension() {
            assert_eq!(extension_of(bytes(b"\xFF.md")), Some("md"));
        }

        #[test]
        fn an_extension_that_is_not_utf8_is_none() {
            assert_eq!(extension_of(bytes(b"a.\xFF")), None);
        }

        #[test]
        fn a_path_that_is_not_utf8_has_no_key() {
            let relative = Path::new("sub").join(bytes(b"\xFF.md"));

            assert_eq!(key_of(&relative), None);
        }

        #[test]
        fn a_directory_that_is_not_utf8_leaves_the_path_without_a_key() {
            let relative = Path::new(bytes(b"\xFF")).join("a.md");

            assert_eq!(key_of(&relative), None);
        }

        #[test]
        fn the_shown_name_replaces_what_is_not_utf8() {
            let relative = Path::new("sub").join(bytes(b"a\xFFb.md"));

            assert_eq!(shown_name(&relative), "sub/a\u{FFFD}b.md");
        }
    }
}
