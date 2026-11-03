use brainwire_core::value::Value;
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

#[test]
fn integer_conversion_accepts_representable_signed_bounds_without_saturating() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let min = i64::MIN as f64;
    let upper = -(i64::MIN as f64);
    let largest = f64::from_bits(upper.to_bits() - 1);
    let below_min = f64::from_bits(min.to_bits() + 1);
    let params = BTreeMap::from([
        ("min".into(), Value::Float(min)),
        ("largest".into(), Value::Float(largest)),
        ("upper".into(), Value::Float(upper)),
        ("below_min".into(), Value::Float(below_min)),
    ]);
    let result = execute(
        &db,
        "RETURN toInteger($min), toInteger($largest), toIntegerOrNull($upper), toIntegerOrNull($below_min), toIntegerOrNull($min)",
        &params,
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::Int(i64::MIN)),
            Out::Value(Value::Int(9_223_372_036_854_774_784)),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Int(i64::MIN)),
        ]]
    );
}

#[test]
fn strings_keep_exact_integer_bounds_without_expanding_decimal_coercion() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let result = execute(
        &db,
        "RETURN toInteger('9223372036854775807'), toInteger('-9223372036854775808'), toInteger('-9223372036854775809'), toInteger('9223372036854775808'), toInteger('-9223372036854775809.0'), toInteger('-9.223372036854775809e18'), toInteger('9223372036854775808.0'), toInteger('NaN'), toInteger('inf'), toInteger(' -12.9 '), toInteger('1.2e2'), toInteger('.9')",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::Int(i64::MAX)),
            Out::Value(Value::Int(i64::MIN)),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Int(-12)),
            Out::Value(Value::Int(120)),
            Out::Value(Value::Int(0)),
        ]]
    );
}
