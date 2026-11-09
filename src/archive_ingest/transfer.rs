//! What the part that drives an ingest and the parts that read zip and tar
//! files hand to each other.

use crate::types::BoxError;

/// The size of the list inside an archive: a running total and its limit.
/// Whoever creates it gives the limit, so that a test can make it small.
#[derive(Debug)]
pub(super) struct ListSize {
    total: u64,
    limit: u64,
}

impl ListSize {
    /// Creates one with a total of 0.
    pub(super) fn new(limit: u64) -> Self {
        Self { total: 0, limit }
    }

    /// Adds to the total, unless that would take it over the limit.
    pub(super) fn add(&mut self, bytes: u64) -> Result<(), ListSizeExceeded> {
        self.total = self
            .total
            .checked_add(bytes)
            .filter(|total| *total <= self.limit)
            .ok_or(ListSizeExceeded)?;
        Ok(())
    }

    /// Takes away from the total, stopping at 0.
    pub(super) fn sub(&mut self, bytes: u64) {
        self.total = self.total.saturating_sub(bytes);
    }
}

/// Adding would have taken the total over the limit.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ListSizeExceeded;

/// One item of the list inside an archive.
#[derive(Debug)]
pub(super) struct Entry {
    /// The name as bytes, which need not be UTF-8.
    pub(super) name: Vec<u8>,
    /// Whether the item is a regular file.
    pub(super) regular: bool,
    /// Where to read the item from on the second pass.
    pub(super) position: Position,
}

/// Where an item is, for reading it on the second pass. Only the reader that
/// made it knows what the number means. Positions grow in the order of the
/// items.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Position(pub(super) u64);

/// Why the first pass stopped.
#[derive(Debug)]
pub(super) enum ListError {
    /// The source cannot be read.
    SourceUnreadable(BoxError),
    /// The list is larger than its limit.
    ListSizeExceeded,
}

/// The first pass: reads the list inside an archive, one item at a time.
pub(super) trait Listing: Send {
    /// The next item, or `None` once the list has been read to its end.
    fn next(&mut self, size: &mut ListSize) -> Result<Option<Entry>, ListError>;
}

/// What reading one position on the second pass gives.
#[derive(Debug)]
pub(super) enum ReadResult {
    /// The item at the position, with its content or why that cannot be read.
    Entry {
        name: Vec<u8>,
        regular: bool,
        content: Result<Vec<u8>, BoxError>,
    },
    /// Neither the position nor anything after it can be read.
    RestUnreadable(BoxError),
}

/// The second pass: reads the items at the positions it is given, which must
/// come in the order the first pass returned them.
pub(super) trait Contents: Send {
    fn read(&mut self, position: Position) -> ReadResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_up_to_the_limit_exactly_succeeds() {
        let mut size = ListSize::new(10);

        assert_eq!(size.add(4), Ok(()));
        assert_eq!(size.add(6), Ok(()));
    }

    #[test]
    fn adding_past_the_limit_fails() {
        let mut size = ListSize::new(10);
        size.add(10).unwrap();

        assert_eq!(size.add(1), Err(ListSizeExceeded));
    }

    #[test]
    fn adding_more_than_a_u64_can_hold_fails() {
        let mut size = ListSize::new(u64::MAX);
        size.add(u64::MAX).unwrap();

        assert_eq!(size.add(1), Err(ListSizeExceeded));
    }

    #[test]
    fn subtracting_makes_room_for_that_much_again() {
        let mut size = ListSize::new(10);
        size.add(10).unwrap();

        size.sub(3);

        assert_eq!(size.add(3), Ok(()));
        assert_eq!(size.add(1), Err(ListSizeExceeded));
    }

    #[test]
    fn subtracting_more_than_the_total_leaves_it_at_zero() {
        let mut size = ListSize::new(10);
        size.add(2).unwrap();

        size.sub(5);

        assert_eq!(size.add(10), Ok(()));
        assert_eq!(size.add(1), Err(ListSizeExceeded));
    }
}
