use super::*;

#[test]
fn the_largest_safe_integer_is_2_to_the_53_minus_1() {
    assert_eq!(MAX_SAFE_INTEGER, (1 << 53) - 1);
}

#[test]
fn a_safe_integer_keeps_its_value() {
    assert_eq!(SafeInteger::new(12).get(), 12);
}

#[test]
fn the_largest_safe_integer_can_be_made() {
    assert_eq!(SafeInteger::new(MAX_SAFE_INTEGER).get(), MAX_SAFE_INTEGER);
}

#[test]
#[should_panic(expected = "9007199254740992 is larger than 2^53 - 1")]
fn a_safe_integer_larger_than_2_to_the_53_minus_1_is_an_unexpected_failure() {
    SafeInteger::new(MAX_SAFE_INTEGER + 1);
}

#[test]
fn a_safe_integer_is_written_as_a_json_number() {
    let json = serde_json::to_value(SafeInteger::new(42)).unwrap();

    assert_eq!(json, serde_json::json!(42));
}

#[test]
fn the_schema_of_a_safe_integer_is_a_whole_number_from_0_to_the_largest() {
    let schema = schemars::schema_for!(SafeInteger);

    assert_eq!(schema.get("type"), Some(&serde_json::json!("integer")));
    assert_eq!(schema.get("minimum"), Some(&serde_json::json!(0)));
    assert_eq!(
        schema.get("maximum"),
        Some(&serde_json::json!(MAX_SAFE_INTEGER))
    );
}

#[test]
fn a_score_keeps_its_value() {
    assert_eq!(Score::new(0.5).get(), 0.5);
}

#[test]
#[should_panic(expected = "the score NaN is not finite")]
fn a_score_that_is_not_a_number_is_an_unexpected_failure() {
    Score::new(f64::NAN);
}

#[test]
#[should_panic(expected = "the score inf is not finite")]
fn an_infinite_score_is_an_unexpected_failure() {
    Score::new(f64::INFINITY);
}

#[test]
#[should_panic(expected = "the score -inf is not finite")]
fn a_negative_infinite_score_is_an_unexpected_failure() {
    Score::new(f64::NEG_INFINITY);
}

#[test]
fn a_score_is_written_as_a_json_number() {
    let json = serde_json::to_value(Score::new(0.25)).unwrap();

    assert_eq!(json, serde_json::json!(0.25));
}

#[test]
fn the_schema_of_a_score_is_a_number() {
    let schema = schemars::schema_for!(Score);

    assert_eq!(schema.get("type"), Some(&serde_json::json!("number")));
}

#[test]
fn a_json_value_without_numbers_is_kept() {
    let value = serde_json::json!({ "path": "/home/user/notes", "kept": [true, null] });

    assert_eq!(SafeJsonValue::new(value.clone()).get(), &value);
}

#[test]
fn a_json_value_with_safe_integers_and_fractions_is_kept() {
    let value = serde_json::json!({
        "largest": MAX_SAFE_INTEGER,
        "smallest": -9_007_199_254_740_991_i64,
        "fraction": 0.5,
    });

    assert_eq!(SafeJsonValue::new(value.clone()).get(), &value);
}

#[test]
#[should_panic(expected = "9007199254740992 is larger than 2^53 - 1")]
fn a_json_value_with_an_integer_larger_than_2_to_the_53_minus_1_is_an_unexpected_failure() {
    SafeJsonValue::new(serde_json::json!({ "outer": [{ "inner": MAX_SAFE_INTEGER + 1 }] }));
}

#[test]
#[should_panic(expected = "-9007199254740992 is smaller than -(2^53 - 1)")]
fn a_json_value_with_an_integer_smaller_than_minus_2_to_the_53_plus_1_is_an_unexpected_failure() {
    SafeJsonValue::new(serde_json::json!([-9_007_199_254_740_992_i64]));
}

#[test]
fn a_json_object_with_safe_integers_is_kept() {
    let members = object(serde_json::json!({ "path": "/home/user/notes", "count": 3 }));

    assert_eq!(SafeJsonObject::new(members.clone()).get(), &members);
}

#[test]
#[should_panic(expected = "9007199254740992 is larger than 2^53 - 1")]
fn a_json_object_with_an_integer_larger_than_2_to_the_53_minus_1_is_an_unexpected_failure() {
    SafeJsonObject::new(object(serde_json::json!({ "count": MAX_SAFE_INTEGER + 1 })));
}

#[test]
fn a_safe_json_object_is_written_as_the_object() {
    let json = serde_json::to_value(SafeJsonObject::new(object(
        serde_json::json!({ "b": 1, "a": 2 }),
    )))
    .unwrap();

    assert_eq!(serde_json::to_string(&json).unwrap(), r#"{"b":1,"a":2}"#);
}

#[test]
fn the_schema_of_a_safe_json_object_is_an_object() {
    let schema = schemars::schema_for!(SafeJsonObject);

    assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
}

fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(members) => members,
        _ => panic!("not an object"),
    }
}

#[test]
fn a_safe_json_value_is_written_as_the_value() {
    let json = serde_json::to_value(SafeJsonValue::new(serde_json::json!({ "a": 1 }))).unwrap();

    assert_eq!(json, serde_json::json!({ "a": 1 }));
}
