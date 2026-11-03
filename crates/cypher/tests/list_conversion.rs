use brainwire_core::value::Value;
use brainwire_cypher::{execute, execute_with_limits, ExecError, ExecutionLimits, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    (dir, db)
}

#[test]
fn list_conversions_preserve_order_null_and_scalar_conversion_rules() {
    let (_dir, db) = setup();
    let result = execute(
        &db,
        "RETURN toBooleanList([true, 'FALSE', 0, -7, 1.0, 'bad', null, [true], {}]), toIntegerList([true, false, 2.9, '-4', 'bad', null, [1], {}]), toFloatList([1, 2.5, '3.25', true, 'bad', null, [1], {}])",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::List(vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(false),
                Value::Bool(true),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ])),
            Out::Value(Value::List(vec![
                Value::Int(1),
                Value::Int(0),
                Value::Int(2),
                Value::Int(-4),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ])),
            Out::Value(Value::List(vec![
                Value::Float(1.0),
                Value::Float(2.5),
                Value::Float(3.25),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ])),
        ]]
    );
}

#[test]
fn list_conversions_handle_null_empty_parameters_and_input_contract() {
    let (_dir, db) = setup();
    for name in ["toBooleanList", "toIntegerList", "toFloatList"] {
        let result = execute(
            &db,
            &format!("RETURN {name}(null), {name}([]), {name}($items)"),
            &BTreeMap::from([("items".into(), Value::List(vec![Value::Null]))]),
        )
        .unwrap();
        assert_eq!(
            result.rows,
            vec![vec![
                Out::Value(Value::Null),
                Out::Value(Value::List(vec![])),
                Out::Value(Value::List(vec![Value::Null]))
            ]]
        );
        for argument in ["1", "true", "'one'", "{}"] {
            assert!(execute(&db, &format!("RETURN {name}({argument})"), &BTreeMap::new()).is_err());
        }
        for arguments in ["", "[], []"] {
            assert!(execute(
                &db,
                &format!("RETURN {name}({arguments})"),
                &BTreeMap::new()
            )
            .is_err());
        }
    }
}

#[test]
fn list_conversion_still_enforces_the_query_budget() {
    let (_dir, db) = setup();
    let result = execute_with_limits(
        &db,
        "RETURN toIntegerList(range(1, 1000))",
        &BTreeMap::new(),
        ExecutionLimits {
            memory_bytes: 1024,
            ..ExecutionLimits::default()
        },
    );
    assert!(matches!(result, Err(ExecError::MemoryLimit { .. })));
    assert!(execute(&db, "RETURN toIntegerList(['1'])", &BTreeMap::new()).is_ok());
}

#[test]
fn integer_list_keeps_signed_bounds_and_invalid_numbers_consistent_with_scalars() {
    let (_dir, db) = setup();
    let result = execute(
        &db,
        "RETURN toIntegerList(['9223372036854775807', '-9223372036854775808', '9223372036854775808', 'NaN', 'inf', '-12.9', '1.2e2'])",
        &BTreeMap::new(),
    ).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Out::Value(Value::List(vec![
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Int(-12),
            Value::Int(120)
        ]))]]
    );
}
