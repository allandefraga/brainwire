use brainwire_core::value::Value;
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    (dir, db)
}

fn floats(values: &[f64]) -> Vec<Vec<Out>> {
    vec![values
        .iter()
        .map(|v| Out::Value(Value::Float(*v)))
        .collect()]
}

#[test]
fn zero_precision_ties_round_towards_positive_infinity() {
    let (_dir, db) = setup();
    let result = execute(
        &db,
        "RETURN round(-2.5), round(2.5), round(-1.5, 0), round(1.5, 0.0), round(-2.6), round(-2.4)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(result.rows, floats(&[-2.0, 3.0, -1.0, 2.0, -3.0, -2.0]));
}

#[test]
fn precision_rounding_keeps_half_away_from_zero_and_propagates_null() {
    let (_dir, db) = setup();
    let result = execute(
        &db,
        "RETURN round(-1.25, 1), round(1.25, 1), round(8.765432, 3), round(null), round(1.5, null)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::Float(-1.3)),
            Out::Value(Value::Float(1.3)),
            Out::Value(Value::Float(8.765)),
            Out::Value(Value::Null),
            Out::Value(Value::Null)
        ]]
    );
}

#[test]
fn rounding_rejects_invalid_precision_instead_of_wrapping() {
    let (_dir, db) = setup();
    for query in [
        "RETURN round(1.0, -1)",
        "RETURN round(1.0, 2147483648)",
        "RETURN round(1.0, 4294967296)",
        "RETURN round(1.0, 1.5)",
        "RETURN round(1.0, 'one')",
        "RETURN round(true)",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
    }
}

#[test]
fn rounding_preserves_large_finite_values_and_nonfinite_parameters() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([
        ("large".into(), Value::Float(f64::MAX)),
        (
            "below".into(),
            Value::Float(f64::from_bits(0.5_f64.to_bits() - 1)),
        ),
        ("nan".into(), Value::Float(f64::NAN)),
        ("inf".into(), Value::Float(f64::INFINITY)),
        ("tiny".into(), Value::Float(f64::from_bits(1))),
    ]);
    let result = execute(&db, "RETURN round($large), round($large, 308), round($below), round($nan), round($inf), round(1.25, 309), round(1.25, 2147483647), round($tiny, 323), round($tiny, 324)", &params).unwrap();
    assert_eq!(result.rows[0][0], Out::Value(Value::Float(f64::MAX)));
    assert_eq!(result.rows[0][1], Out::Value(Value::Float(f64::MAX)));
    assert_eq!(result.rows[0][2], Out::Value(Value::Float(0.0)));
    assert!(matches!(result.rows[0][3], Out::Value(Value::Float(x)) if x.is_nan()));
    assert_eq!(result.rows[0][4], Out::Value(Value::Float(f64::INFINITY)));
    assert_eq!(result.rows[0][5], Out::Value(Value::Float(1.25)));
    assert_eq!(result.rows[0][6], Out::Value(Value::Float(1.25)));
    assert_eq!(result.rows[0][7], Out::Value(Value::Float(0.0)));
    assert_eq!(
        result.rows[0][8],
        Out::Value(Value::Float(f64::from_bits(1)))
    );
}

#[test]
fn precision_already_represented_by_the_value_does_not_change_its_bits() {
    let (_dir, db) = setup();
    for value in [123456789.0_f64, 123456789.1, 0.1, -0.0000001234, -0.0] {
        for precision in [16_i64, 17, 100, 308, 309, 324, i64::from(i32::MAX)] {
            let params = BTreeMap::from([
                ("value".into(), Value::Float(value)),
                ("precision".into(), Value::Int(precision)),
            ]);
            let result = execute(&db, "RETURN round($value, $precision)", &params).unwrap();
            let Out::Value(Value::Float(actual)) = result.rows[0][0] else {
                panic!("round did not return a float");
            };
            assert_eq!(
                actual.to_bits(),
                value.to_bits(),
                "value={value} precision={precision}"
            );
        }
    }
}
