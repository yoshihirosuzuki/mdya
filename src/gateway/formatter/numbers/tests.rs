use std::collections::BTreeMap;

use serde::Serialize;

use super::*;

const MAX: u64 = 9_007_199_254_740_991;

#[derive(Serialize)]
struct Hit {
    score: f64,
    chunk_index: u64,
}

#[derive(Serialize)]
enum Shape {
    Unit,
    Newtype(i64),
    Tuple(u64, f32),
    Struct { value: f64 },
}

#[test]
fn values_without_numbers_pass() {
    assert!(check_json_numbers(&("text", true, None::<u64>, ())).is_ok());
}

#[test]
fn integers_from_minus_2_to_the_53_plus_1_to_2_to_the_53_minus_1_pass() {
    let max = i64::try_from(MAX).unwrap();

    assert!(check_json_numbers(&(MAX, -max, 0_u8, -1_i32, u128::from(MAX))).is_ok());
}

#[test]
fn finite_fractions_pass() {
    assert!(check_json_numbers(&(0.5_f64, -1.0e300_f64, 1.5_f32)).is_ok());
}

#[test]
fn an_integer_larger_than_2_to_the_53_minus_1_is_found() {
    assert_eq!(
        check_json_numbers(&(MAX + 1)),
        Err(UnsafeNumber::OutOfRange("9007199254740992".to_owned()))
    );
}

#[test]
fn an_integer_smaller_than_minus_2_to_the_53_plus_1_is_found() {
    assert_eq!(
        check_json_numbers(&(-9_007_199_254_740_992_i64)),
        Err(UnsafeNumber::OutOfRange("-9007199254740992".to_owned()))
    );
}

#[test]
fn a_128_bit_integer_out_of_the_range_is_found() {
    assert_eq!(
        check_json_numbers(&u128::MAX),
        Err(UnsafeNumber::OutOfRange(u128::MAX.to_string()))
    );
    assert_eq!(
        check_json_numbers(&i128::MIN),
        Err(UnsafeNumber::OutOfRange(i128::MIN.to_string()))
    );
}

#[test]
fn nan_and_the_infinities_are_found() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            check_json_numbers(&value),
            Err(UnsafeNumber::NotFinite(value.to_string())),
            "{value}"
        );
    }
    assert_eq!(
        check_json_numbers(&f32::NAN),
        Err(UnsafeNumber::NotFinite("NaN".to_owned()))
    );
}

#[test]
fn a_number_in_a_struct_in_a_sequence_is_found() {
    let hits = vec![
        Hit {
            score: 0.5,
            chunk_index: 1,
        },
        Hit {
            score: f64::NAN,
            chunk_index: 2,
        },
    ];

    assert_eq!(
        check_json_numbers(&hits),
        Err(UnsafeNumber::NotFinite("NaN".to_owned()))
    );
}

#[test]
fn a_number_in_a_map_value_is_found() {
    let map = BTreeMap::from([("limit", MAX + 1)]);

    assert!(check_json_numbers(&map).is_err());
}

#[test]
fn a_number_in_an_option_is_found() {
    assert!(check_json_numbers(&Some(f64::INFINITY)).is_err());
}

#[test]
fn numbers_in_enum_variants_are_found() {
    assert!(check_json_numbers(&Shape::Unit).is_ok());
    assert!(check_json_numbers(&Shape::Newtype(i64::MIN)).is_err());
    assert!(check_json_numbers(&Shape::Tuple(1, f32::NAN)).is_err());
    assert!(
        check_json_numbers(&Shape::Struct {
            value: f64::NEG_INFINITY
        })
        .is_err()
    );
}

#[test]
fn a_number_in_a_json_value_is_found() {
    let value = serde_json::json!({ "outer": [1, { "inner": MAX + 1 }] });

    assert!(check_json_numbers(&value).is_err());
}

#[test]
fn a_map_key_is_not_checked_as_a_number_as_json_writes_it_as_a_string() {
    let map = BTreeMap::from([(u64::MAX, "x")]);

    assert!(check_json_numbers(&map).is_ok());
}
