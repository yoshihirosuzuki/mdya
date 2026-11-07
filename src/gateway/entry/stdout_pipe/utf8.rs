//! Splits bytes written in pieces into whole characters, for a writer that
//! has to write whole characters at a time (a Windows console, in UTF-16).

use std::io;

/// Splits `bytes` into the whole UTF-8 characters at their start and the
/// start of a character at their end that is not whole yet, which the next
/// bytes may complete. An error if the bytes are not UTF-8 otherwise.
pub(super) fn split_whole(bytes: &[u8]) -> io::Result<(&str, &[u8])> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok((text, &[])),
        Err(error) if error.error_len().is_none() => {
            let (whole, rest) = bytes.split_at(error.valid_up_to());
            // `whole` is the part that `from_utf8` checked.
            let whole = std::str::from_utf8(whole)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok((whole, rest))
        }
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_characters_are_all_split_off() {
        let (whole, rest) = split_whole("日本語".as_bytes()).unwrap();

        assert_eq!((whole, rest), ("日本語", &[][..]));
    }

    #[test]
    fn the_start_of_a_character_at_the_end_is_kept_for_the_next_bytes() {
        let bytes = "日本".as_bytes();

        let (whole, rest) = split_whole(&bytes[..4]).unwrap();

        assert_eq!((whole, rest), ("日", &bytes[3..4]));
    }

    #[test]
    fn bytes_that_are_not_utf8_are_an_error() {
        let error = split_whole(b"a\xffb").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
