use brainwire_core::value::Value;
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

#[test]
fn boolean_or_null_converts_supported_inputs_and_nulls_other_types() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let result = execute(
        &db,
        "RETURN toBooleanOrNull(true), toBooleanOrNull(false), toBooleanOrNull(0), toBooleanOrNull(-7), toBooleanOrNull('true'), toBooleanOrNull('FALSE'), toBooleanOrNull('unknown'), toBooleanOrNull(null), toBooleanOrNull(1.0), toBooleanOrNull([true]), toBooleanOrNull({value:true})",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(false)),
            Out::Value(Value::Bool(false)),
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(false)),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
        ]]
    );
}

#[test]
fn boolean_or_null_keeps_strict_conversion_and_one_argument_contract() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    for query in [
        "RETURN toBoolean([true])",
        "RETURN toBoolean(1.0)",
        "RETURN toBooleanOrNull()",
        "RETURN toBooleanOrNull(true, false)",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
    }
}
