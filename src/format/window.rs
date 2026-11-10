//! Cuts a text into overlapping ranges of a fixed width.

use std::ops::Range;

/// The most characters that a chunk's text holds.
pub(super) const MAX_CHARS: usize = 700;

/// The characters that two neighbouring ranges of a cut text share.
const OVERLAP_CHARS: usize = 70;

/// Cuts `text` into ranges of [`MAX_CHARS`] characters, each sharing
/// [`OVERLAP_CHARS`] characters with the next. The ranges are byte offsets
/// into `text` and cover all of it.
pub(super) fn cut(text: &str) -> Vec<Range<usize>> {
    let step = MAX_CHARS - OVERLAP_CHARS;
    let ends = text
        .char_indices()
        .skip(MAX_CHARS)
        .step_by(step)
        .map(|(end, _)| end);
    let next_starts = text
        .char_indices()
        .step_by(step)
        .skip(1)
        .map(|(start, _)| start);
    let mut ranges = Vec::new();
    let mut start = 0;
    for (end, next_start) in ends.zip(next_starts) {
        ranges.push(start..end);
        start = next_start;
    }
    ranges.push(start..text.len());
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_within_the_width_is_one_range() {
        assert_eq!(cut("short"), vec![0..5]);
    }

    #[test]
    fn a_text_of_exactly_the_width_is_one_range() {
        let text = "a".repeat(700);

        assert_eq!(cut(&text), vec![0..700]);
    }

    #[test]
    fn one_character_over_the_width_starts_a_second_range_that_overlaps() {
        let text = "a".repeat(701);

        assert_eq!(cut(&text), vec![0..700, 630..701]);
    }

    #[test]
    fn a_last_range_of_exactly_the_width_ends_the_cut() {
        let text = "a".repeat(1330);

        assert_eq!(cut(&text), vec![0..700, 630..1330]);
    }

    #[test]
    fn ranges_are_counted_in_characters_and_given_in_bytes() {
        let text = "あ".repeat(1500);

        assert_eq!(cut(&text), vec![0..2100, 1890..3990, 3780..4500]);
    }

    #[test]
    fn whitespace_at_the_end_counts_like_any_other_character() {
        let within = format!("{}\n\n", "a".repeat(698));
        let over = format!("{}\n\n\n", "a".repeat(700));

        assert_eq!(cut(&within), vec![0..700]);
        assert_eq!(cut(&over), vec![0..700, 630..703]);
    }

    #[test]
    fn a_text_of_only_whitespace_is_one_range() {
        assert_eq!(cut(" \n\t"), vec![0..3]);
    }
}
