//! Which numbers JSON output does not write: NaN and the infinities, which
//! JSON has no form for, and the integers outside −(2^53 − 1) to 2^53 − 1,
//! whose values JSON implementations do not all agree on (RFC 8259, section
//! 6). The numbers are checked as the JSON is made, in the one serialization
//! that makes it, so that what is checked is what is written, whatever the
//! value's `Serialize` does: `serde_json` would write NaN and the infinities
//! as `null`, and could not tell them apart afterwards.

use std::cell::RefCell;

use serde::Serialize;
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant, Serializer,
};

use super::super::operations::MAX_SAFE_INTEGER;

#[cfg(test)]
mod tests;

/// A number that JSON output does not write, as it would be shown.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UnsafeNumber {
    #[error("the number {0} is not finite: JSON has no NaN or infinity")]
    NotFinite(String),
    #[error("the integer {0} is outside -(2^53 - 1) to 2^53 - 1 (RFC 8259, section 6)")]
    OutOfRange(String),
    /// A value that cannot be written as JSON for another reason.
    #[error("the value cannot be written as JSON: {0}")]
    Other(String),
}

/// `value` as one line of JSON, with its numbers checked as they are
/// written.
pub(super) fn to_json_string<T: Serialize + ?Sized>(value: &T) -> Result<String, UnsafeNumber> {
    let found = Found::default();
    let written = serde_json::to_string(&Checked {
        value,
        found: &found,
    });
    found.or(written)
}

/// `value` as a JSON value, with its numbers checked as it is made.
pub(super) fn to_json_value<T: Serialize + ?Sized>(
    value: &T,
) -> Result<serde_json::Value, UnsafeNumber> {
    let found = Found::default();
    let made = serde_json::to_value(Checked {
        value,
        found: &found,
    });
    found.or(made)
}

/// Checks every number in `value` as JSON output would write it. The keys of
/// a map are not checked, as JSON writes them as strings.
pub(super) fn check_json_numbers<T: Serialize + ?Sized>(value: &T) -> Result<(), UnsafeNumber> {
    to_json_value(value).map(|_| ())
}

fn check_integer(value: i128) -> Result<(), UnsafeNumber> {
    if value.unsigned_abs() > u128::from(MAX_SAFE_INTEGER) {
        return Err(UnsafeNumber::OutOfRange(value.to_string()));
    }
    Ok(())
}

fn check_unsigned(value: u128) -> Result<(), UnsafeNumber> {
    if value > u128::from(MAX_SAFE_INTEGER) {
        return Err(UnsafeNumber::OutOfRange(value.to_string()));
    }
    Ok(())
}

fn check_float(value: f64) -> Result<(), UnsafeNumber> {
    if !value.is_finite() {
        return Err(UnsafeNumber::NotFinite(value.to_string()));
    }
    Ok(())
}

/// The first unsafe number found while serializing. The serialization is
/// stopped with the serializer's own error, which says less; this keeps what
/// was found.
#[derive(Default)]
struct Found(RefCell<Option<UnsafeNumber>>);

impl Found {
    /// Stops the serialization when `checked` found an unsafe number.
    fn stop<E: ser::Error>(&self, checked: Result<(), UnsafeNumber>) -> Result<(), E> {
        match checked {
            Ok(()) => Ok(()),
            Err(unsafe_number) => {
                let message = unsafe_number.to_string();
                self.0.borrow_mut().get_or_insert(unsafe_number);
                Err(E::custom(message))
            }
        }
    }

    /// The unsafe number found, if any, or else what the serialization gave.
    fn or<T>(self, serialized: Result<T, serde_json::Error>) -> Result<T, UnsafeNumber> {
        if let Some(unsafe_number) = self.0.into_inner() {
            return Err(unsafe_number);
        }
        serialized.map_err(|error| UnsafeNumber::Other(error.to_string()))
    }
}

/// A value to serialize with its numbers checked.
struct Checked<'a, T: ?Sized> {
    value: &'a T,
    found: &'a Found,
}

impl<T: Serialize + ?Sized> Serialize for Checked<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(Checking {
            inner: serializer,
            found: self.found,
        })
    }
}

/// Passes everything on to `inner`, checking each number on the way.
struct Checking<'a, S> {
    inner: S,
    found: &'a Found,
}

impl<'a, S: Serializer> Checking<'a, S> {
    fn checked<'v, T: ?Sized>(&self, value: &'v T) -> Checked<'v, T>
    where
        'a: 'v,
    {
        Checked {
            value,
            found: self.found,
        }
    }
}

impl<'a, S: Serializer> Serializer for Checking<'a, S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = Compound<'a, S::SerializeSeq>;
    type SerializeTuple = Compound<'a, S::SerializeTuple>;
    type SerializeTupleStruct = Compound<'a, S::SerializeTupleStruct>;
    type SerializeTupleVariant = Compound<'a, S::SerializeTupleVariant>;
    type SerializeMap = Compound<'a, S::SerializeMap>;
    type SerializeStruct = Compound<'a, S::SerializeStruct>;
    type SerializeStructVariant = Compound<'a, S::SerializeStructVariant>;

    fn serialize_bool(self, value: bool) -> Result<S::Ok, S::Error> {
        self.inner.serialize_bool(value)
    }

    fn serialize_i8(self, value: i8) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i8(value)
    }

    fn serialize_i16(self, value: i16) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i16(value)
    }

    fn serialize_i32(self, value: i32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_i32(value)
    }

    fn serialize_i64(self, value: i64) -> Result<S::Ok, S::Error> {
        self.found.stop(check_integer(i128::from(value)))?;
        self.inner.serialize_i64(value)
    }

    fn serialize_i128(self, value: i128) -> Result<S::Ok, S::Error> {
        self.found.stop(check_integer(value))?;
        self.inner.serialize_i128(value)
    }

    fn serialize_u8(self, value: u8) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u8(value)
    }

    fn serialize_u16(self, value: u16) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u16(value)
    }

    fn serialize_u32(self, value: u32) -> Result<S::Ok, S::Error> {
        self.inner.serialize_u32(value)
    }

    fn serialize_u64(self, value: u64) -> Result<S::Ok, S::Error> {
        self.found.stop(check_unsigned(u128::from(value)))?;
        self.inner.serialize_u64(value)
    }

    fn serialize_u128(self, value: u128) -> Result<S::Ok, S::Error> {
        self.found.stop(check_unsigned(value))?;
        self.inner.serialize_u128(value)
    }

    fn serialize_f32(self, value: f32) -> Result<S::Ok, S::Error> {
        self.found.stop(check_float(f64::from(value)))?;
        self.inner.serialize_f32(value)
    }

    fn serialize_f64(self, value: f64) -> Result<S::Ok, S::Error> {
        self.found.stop(check_float(value))?;
        self.inner.serialize_f64(value)
    }

    fn serialize_char(self, value: char) -> Result<S::Ok, S::Error> {
        self.inner.serialize_char(value)
    }

    fn serialize_str(self, value: &str) -> Result<S::Ok, S::Error> {
        self.inner.serialize_str(value)
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<S::Ok, S::Error> {
        self.inner.serialize_bytes(value)
    }

    fn serialize_none(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_none()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<S::Ok, S::Error> {
        let checked = self.checked(value);
        self.inner.serialize_some(&checked)
    }

    fn serialize_unit(self) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit_struct(name)
    }

    fn serialize_unit_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
    ) -> Result<S::Ok, S::Error> {
        self.inner.serialize_unit_variant(name, index, variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error> {
        let checked = self.checked(value);
        self.inner.serialize_newtype_struct(name, &checked)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<S::Ok, S::Error> {
        let checked = self.checked(value);
        self.inner
            .serialize_newtype_variant(name, index, variant, &checked)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self.inner.serialize_seq(len)?,
            found,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self.inner.serialize_tuple(len)?,
            found,
        })
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self.inner.serialize_tuple_struct(name, len)?,
            found,
        })
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self
                .inner
                .serialize_tuple_variant(name, index, variant, len)?,
            found,
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self.inner.serialize_map(len)?,
            found,
        })
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self.inner.serialize_struct(name, len)?,
            found,
        })
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, S::Error> {
        let found = self.found;
        Ok(Compound {
            inner: self
                .inner
                .serialize_struct_variant(name, index, variant, len)?,
            found,
        })
    }

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

/// A compound value of the inner serializer, with each part checked.
struct Compound<'a, C> {
    inner: C,
    found: &'a Found,
}

impl<C: SerializeSeq> SerializeSeq for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_element(&Checked { value, found })
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTuple> SerializeTuple for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_element(&Checked { value, found })
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTupleStruct> SerializeTupleStruct for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_field(&Checked { value, found })
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeTupleVariant> SerializeTupleVariant for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_field(&Checked { value, found })
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeMap> SerializeMap for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    /// JSON writes a key as a string, so its numbers are not checked.
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), C::Error> {
        self.inner.serialize_key(key)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_value(&Checked { value, found })
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeStruct> SerializeStruct for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_field(key, &Checked { value, found })
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.inner.skip_field(key)
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}

impl<C: SerializeStructVariant> SerializeStructVariant for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), C::Error> {
        let found = self.found;
        self.inner.serialize_field(key, &Checked { value, found })
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
        self.inner.skip_field(key)
    }

    fn end(self) -> Result<C::Ok, C::Error> {
        self.inner.end()
    }
}
