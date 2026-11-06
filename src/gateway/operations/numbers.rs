//! The numbers in the operations' results. The command line and the MCP
//! server write the results as JSON. JSON has no NaN or infinity, and JSON
//! implementations agree exactly on the value of an integer from −(2^53 − 1)
//! to 2^53 − 1 (RFC 8259, section 6). A result holds only finite numbers and
//! integers in that range, checked when the number is made: a number outside
//! them is a bug in mdya, so making one is an unexpected failure (a panic).

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde_json::{Map, Number, Value};

#[cfg(test)]
mod tests;

/// 2^53 − 1. JSON implementations agree exactly on the value of an integer
/// from −(2^53 − 1) to 2^53 − 1 (RFC 8259, section 6).
pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// A whole number in an operation's result, from 0 to [`MAX_SAFE_INTEGER`]:
/// a count, an index, or a dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct SafeInteger(u64);

impl SafeInteger {
    /// Panics when `value` is larger than [`MAX_SAFE_INTEGER`]. The counts,
    /// indexes, and dimensions that mdya keeps are of documents, chunks, and
    /// vectors, which never get that many, so a larger value is a bug.
    #[expect(clippy::panic)]
    pub(crate) fn new(value: u64) -> Self {
        if value > MAX_SAFE_INTEGER {
            panic!(
                "a number in an operation's result is out of range: {value} is larger than \
                 2^53 - 1 (RFC 8259, section 6)"
            );
        }
        Self(value)
    }

    /// The same as [`SafeInteger::new`], for a `usize`.
    pub(crate) fn from_usize(value: usize) -> Self {
        Self::new(u64::try_from(value).unwrap_or(u64::MAX))
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// A whole number from 0 to [`MAX_SAFE_INTEGER`].
impl JsonSchema for SafeInteger {
    fn schema_name() -> Cow<'static, str> {
        "SafeInteger".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({ "type": "integer", "minimum": 0, "maximum": MAX_SAFE_INTEGER })
    }
}

/// A search score in an operation's result: a finite number.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct Score(f64);

impl Score {
    /// Panics when `value` is NaN or infinite, which JSON cannot hold. A
    /// search must give finite scores, so that the result can be written as
    /// JSON: such a score is a bug in the search.
    #[expect(clippy::panic)]
    pub(crate) fn new(value: f64) -> Self {
        if !value.is_finite() {
            panic!("the score {value} is not finite: JSON has no NaN or infinity");
        }
        Self(value)
    }

    pub(crate) fn get(self) -> f64 {
        self.0
    }
}

/// A number.
impl JsonSchema for Score {
    fn schema_name() -> Cow<'static, str> {
        "Score".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({ "type": "number" })
    }
}

/// A JSON value in an operation's result (a setting's value) whose integers
/// are all from
/// −[`MAX_SAFE_INTEGER`] to [`MAX_SAFE_INTEGER`]. A JSON value's numbers are
/// never NaN or infinite, and a number with a fraction or an exponent is not
/// checked: the spec limits only the integers.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub(crate) struct SafeJsonValue(Value);

impl SafeJsonValue {
    /// Panics when `value` holds an integer outside the range. A setting's
    /// value must be within it, so that the settings can be written as JSON:
    /// a value outside it is a bug in the component that kept the value.
    pub(crate) fn new(value: Value) -> Self {
        check_integers(&value);
        Self(value)
    }

    pub(crate) fn get(&self) -> &Value {
        &self.0
    }
}

/// A JSON object in an operation's result (a collection's type-specific
/// attributes) whose integers are all from −[`MAX_SAFE_INTEGER`] to
/// [`MAX_SAFE_INTEGER`], as in [`SafeJsonValue`]. The members keep their order.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub(crate) struct SafeJsonObject(Map<String, Value>);

impl SafeJsonObject {
    /// Panics when `members` hold an integer outside the range. The attributes
    /// must be within it, so that they can be written as JSON: an integer
    /// outside it is a bug in the component that kept the attributes.
    pub(crate) fn new(members: Map<String, Value>) -> Self {
        members.values().for_each(check_integers);
        Self(members)
    }

    pub(crate) fn get(&self) -> &Map<String, Value> {
        &self.0
    }
}

fn check_integers(value: &Value) {
    match value {
        Value::Number(number) => check_integer(number),
        Value::Array(items) => items.iter().for_each(check_integers),
        Value::Object(members) => members.values().for_each(check_integers),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

#[expect(clippy::panic)]
fn check_integer(number: &Number) {
    if let Some(value) = number.as_u64() {
        SafeInteger::new(value);
    } else if let Some(value) = number.as_i64()
        && value.unsigned_abs() > MAX_SAFE_INTEGER
    {
        panic!(
            "a number in an operation's result is out of range: {value} is smaller than \
             -(2^53 - 1) (RFC 8259, section 6)"
        );
    }
}
