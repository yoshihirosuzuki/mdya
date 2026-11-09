//! The numbers tar files hold: octal and base-256 header fields, and the
//! decimal digits of extended header records.

/// The largest number accepted anywhere in a tar file. Lengths this size still
/// leave room in a `u64` to round up to a whole block.
pub(super) const MAX_NUMBER: u64 = i64::MAX as u64;

/// `number` with one more digit, unless that passes [`MAX_NUMBER`].
pub(super) fn with_digit(number: u64, base: u64, digit: u64) -> Option<u64> {
    number
        .checked_mul(base)?
        .checked_add(digit)
        .filter(|number| *number <= MAX_NUMBER)
}

/// An octal field: digits, which may follow spaces and be followed by any mix
/// of spaces and NULs up to the end of the field. Writers differ.
pub(super) fn octal(field: &[u8]) -> Option<u64> {
    let start = field.iter().position(|byte| *byte != b' ')?;
    let field = &field[start..];
    let digits = field
        .iter()
        .take_while(|byte| (b'0'..=b'7').contains(*byte))
        .count();
    if digits == 0 || field[digits..].iter().any(|byte| !matches!(byte, b' ' | 0)) {
        return None;
    }
    field[..digits].iter().try_fold(0, |number, byte| {
        with_digit(number, 8, u64::from(byte - b'0'))
    })
}

/// A `size` or sparse field: [`octal`], or base-256 when the first byte is
/// 0x80 (the bytes after it, most significant first). Python writes base-256
/// for a size that does not fit in octal.
pub(super) fn octal_or_base_256(field: &[u8]) -> Option<u64> {
    match field.split_first() {
        Some((0x80, bytes)) => bytes
            .iter()
            .try_fold(0, |number, byte| with_digit(number, 256, u64::from(*byte))),
        _ => octal(field),
    }
}
