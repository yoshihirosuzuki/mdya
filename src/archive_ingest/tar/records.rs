//! The records of an extended header, read one at a time and never held whole.

use std::io::{BufRead, Read};

use super::Stop;
use super::numbers::with_digit;

/// Keywords longer than this are read past, whatever they say.
const MAX_KEYWORD_LEN: u64 = 4096;

fn malformed() -> Stop {
    Stop::unreadable("an extended header has a malformed record")
}

/// Moves through records of the form `<length> <keyword>=<value>\n`, where
/// the length counts the whole record. A value may hold newlines and NULs,
/// so records are split by their lengths only.
pub(super) struct Records<'a, D> {
    data: &'a mut D,
    keyword: Vec<u8>,
    keyword_too_long: bool,
    value_left: u64,
    inside_a_record: bool,
}

impl<'a, D: BufRead> Records<'a, D> {
    pub(super) fn new(data: &'a mut D) -> Self {
        Self {
            data,
            keyword: Vec::new(),
            keyword_too_long: false,
            value_left: 0,
            inside_a_record: false,
        }
    }

    /// Goes to the next record. Returns false when none is left. Whether or
    /// not the previous value was read, its record's shape has been checked.
    pub(super) fn advance(&mut self) -> Result<bool, Stop> {
        self.close_record()?;
        let Some(first) = self.data.fill_buf()?.first().copied() else {
            return Ok(false);
        };
        if first == 0 {
            self.check_only_nuls_are_left()?;
            return Ok(false);
        }
        let (length, digits) = self.read_length()?;
        // What follows the length and its space. The shortest record has a
        // keyword of one byte and an empty value; anything shorter could not
        // move forward.
        let body = length
            .checked_sub(digits + 1)
            .filter(|body| *body >= 3)
            .ok_or_else(malformed)?;
        let keyword_len = self.read_keyword(body)?;
        self.value_left = body - keyword_len - 2;
        self.inside_a_record = true;
        Ok(true)
    }

    /// The keyword, or `None` if it is too long to be one worth looking at.
    pub(super) fn keyword(&self) -> Option<&[u8]> {
        (!self.keyword_too_long).then_some(self.keyword.as_slice())
    }

    pub(super) fn value_len(&self) -> u64 {
        self.value_left
    }

    /// The value as bytes. The caller has checked its length.
    pub(super) fn read_value(&mut self) -> Result<Vec<u8>, Stop> {
        let mut value = Vec::with_capacity(usize::try_from(self.value_left).unwrap_or(0));
        let read = (&mut *self.data)
            .take(self.value_left)
            .read_to_end(&mut value)?;
        self.value_left -= read as u64;
        Ok(value)
    }

    /// The value as a decimal number, or `None` if it is not one. An empty
    /// value is not a number.
    pub(super) fn read_decimal(&mut self) -> Result<Option<u64>, Stop> {
        let mut number = None;
        let mut bad = false;
        self.each_value_chunk(|chunk| {
            for byte in chunk {
                bad = bad || !byte.is_ascii_digit();
                if !bad {
                    number = with_digit(number.unwrap_or(0), 10, u64::from(byte - b'0'));
                    bad = number.is_none();
                }
            }
        })?;
        Ok(number.filter(|_| !bad))
    }

    /// Calls `each` with every comma-separated element of the value, parsed
    /// as a decimal number (`None` for one that is not). An empty value has
    /// no elements.
    pub(super) fn read_number_list(
        &mut self,
        mut each: impl FnMut(Option<u64>),
    ) -> Result<(), Stop> {
        if self.value_left == 0 {
            return Ok(());
        }
        let mut element: Option<u64> = None;
        let mut bad = false;
        self.each_value_chunk(|chunk| {
            for byte in chunk {
                if *byte == b',' {
                    each(element.filter(|_| !bad));
                    (element, bad) = (None, false);
                } else if byte.is_ascii_digit() && !bad {
                    element = with_digit(element.unwrap_or(0), 10, u64::from(byte - b'0'));
                    bad = element.is_none();
                } else {
                    bad = true;
                }
            }
        })?;
        each(element.filter(|_| !bad));
        Ok(())
    }

    fn each_value_chunk(&mut self, mut each: impl FnMut(&[u8])) -> Result<(), Stop> {
        while self.value_left > 0 {
            let available = self.data.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            let take = available
                .len()
                .min(usize::try_from(self.value_left).unwrap_or(usize::MAX));
            each(&available[..take]);
            self.data.consume(take);
            self.value_left -= take as u64;
        }
        Ok(())
    }

    fn close_record(&mut self) -> Result<(), Stop> {
        if !self.inside_a_record {
            return Ok(());
        }
        self.inside_a_record = false;
        self.each_value_chunk(|_| {})?;
        if self.value_left != 0 || self.read_byte()? != Some(b'\n') {
            return Err(malformed());
        }
        Ok(())
    }

    fn check_only_nuls_are_left(&mut self) -> Result<(), Stop> {
        loop {
            let available = self.data.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            if available.iter().any(|byte| *byte != 0) {
                return Err(malformed());
            }
            let read = available.len();
            self.data.consume(read);
        }
    }

    fn read_length(&mut self) -> Result<(u64, u64), Stop> {
        let (mut length, mut digits) = (0u64, 0u64);
        loop {
            match self.read_byte()?.ok_or_else(malformed)? {
                byte @ b'0'..=b'9' => {
                    length =
                        with_digit(length, 10, u64::from(byte - b'0')).ok_or_else(malformed)?;
                    digits += 1;
                }
                b' ' if digits > 0 => return Ok((length, digits)),
                _ => return Err(malformed()),
            }
        }
    }

    /// Reads up to and including the `=`, keeping at most
    /// [`MAX_KEYWORD_LEN`] bytes. Returns the keyword's whole length.
    fn read_keyword(&mut self, body: u64) -> Result<u64, Stop> {
        self.keyword.clear();
        let mut len = 0u64;
        loop {
            // The `=` and the newline must still fit in the record.
            if len + 2 > body {
                return Err(malformed());
            }
            match self.read_byte()?.ok_or_else(malformed)? {
                b'=' if len == 0 => return Err(malformed()),
                b'=' => {
                    self.keyword_too_long = len > MAX_KEYWORD_LEN;
                    return Ok(len);
                }
                0 => return Err(malformed()),
                byte => {
                    len += 1;
                    if len <= MAX_KEYWORD_LEN {
                        self.keyword.push(byte);
                    }
                }
            }
        }
    }

    fn read_byte(&mut self) -> Result<Option<u8>, Stop> {
        let Some(byte) = self.data.fill_buf()?.first().copied() else {
            return Ok(None);
        };
        self.data.consume(1);
        Ok(Some(byte))
    }
}
