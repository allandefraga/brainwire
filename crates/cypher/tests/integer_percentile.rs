use brainwire_core::value::Value;
use brainwire_cypher::{
    execute, ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn discrete_percentile_preserves_original_numbers_and_order() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    for (values, fraction, expected) in [
        (
            "[9007199254740993, 9007199254740992]",
            "1",
            Value::Int(9007199254740993),
        ),
        (
            "[9223372036854775807, -9223372036854775807]",
            "1",
            Value::Int(i64::MAX),
        ),
        (
            "[9223372036854775807, -9223372036854775807]",
            "0",
            Value::Int(-9223372036854775807),
        ),
        (
            "[9007199254740993, 9007199254740992.0]",
            "1",
            Value::Int(9007199254740993),
        ),
        (
            "[9007199254740993, 9007199254740992.0]",
            "0",
            Value::Float(9007199254740992.0),
        ),
        ("[3, null, 1.5, 2]", "0.5", Value::Int(2)),
        ("[3, null, 1.5, 2]", "0", Value::Float(1.5)),
        ("[2, 2, 1]", "0.5", Value::Int(2)),
        ("[null]", "0.5", Value::Null),
        ("[]", "0.5", Value::Null),
    ] {
        let query = format!("UNWIND {values} AS n RETURN percentileDisc(n, {fraction})");
        assert_eq!(
            execute(&db, &query, &BTreeMap::new()).unwrap().rows,
            vec![vec![Out::Value(expected)]],
            "{query}"
        );
    }
    let params = BTreeMap::from([(
        "values".into(),
        Value::List(vec![Value::Int(i64::MAX), Value::Int(i64::MIN)]),
    )]);
    assert_eq!(
        execute(
            &db,
            "UNWIND $values AS n RETURN percentileDisc(n, 0), percentileDisc(n, 1)",
            &params,
        )
        .unwrap()
        .rows,
        vec![vec![
            Out::Value(Value::Int(i64::MIN)),
            Out::Value(Value::Int(i64::MAX)),
        ]]
    );
}

#[test]
fn percentile_rejects_non_numeric_inputs_and_invalid_fractions() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    for query in [
        "UNWIND ['bad'] AS n RETURN percentileDisc(n, 0.5)",
        "UNWIND [1] AS n RETURN percentileDisc(n, -0.1)",
        "UNWIND [1] AS n RETURN percentileDisc(n, 1.1)",
        "UNWIND [1] AS n RETURN percentileDisc(n, null)",
    ] {
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{query}"
        );
    }
}

#[test]
fn equal_numeric_values_have_deterministic_types_and_signed_zero() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    for values in ["[2, 2.0]", "[2.0, 2]"] {
        assert_eq!(
            execute(
                &db,
                &format!("UNWIND {values} AS n RETURN percentileDisc(n, 0), percentileDisc(n, 1)"),
                &BTreeMap::new()
            )
            .unwrap()
            .rows,
            vec![vec![
                Out::Value(Value::Int(2)),
                Out::Value(Value::Float(2.0))
            ]]
        );
    }
    // DISTINCT deliberately keeps its existing first-representative rule.
    for (values, expected) in [("[2, 2.0]", Value::Int(2)), ("[2.0, 2]", Value::Float(2.0))] {
        assert_eq!(
            execute(
                &db,
                &format!("UNWIND {values} AS n RETURN percentileDisc(DISTINCT n, 0)"),
                &BTreeMap::new(),
            )
            .unwrap()
            .rows,
            vec![vec![Out::Value(expected)]]
        );
    }
    for values in [vec![0.0, -0.0], vec![-0.0, 0.0]] {
        let params = BTreeMap::from([(
            "values".into(),
            Value::List(values.into_iter().map(Value::Float).collect()),
        )]);
        let result = execute(
            &db,
            "UNWIND $values AS n RETURN percentileDisc(n, 0), percentileDisc(n, 1)",
            &params,
        )
        .unwrap();
        let bits = result.rows[0]
            .iter()
            .map(|out| match out {
                Out::Value(Value::Float(value)) => value.to_bits(),
                other => panic!("expected float, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(bits, vec![(-0.0_f64).to_bits(), 0.0_f64.to_bits()]);
    }
}

#[test]
fn percentile_keeps_continuous_interpolation_and_releases_rejected_reservations() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(
        execute(
            &db,
            "UNWIND [1, 2, null, 3] AS n RETURN percentileCont(n, 0.25), percentileDisc(n, 0.25)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![
            Out::Value(Value::Float(1.5)),
            Out::Value(Value::Int(1))
        ]]
    );
    let limit = 32_000;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let mut transaction = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: limit,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert!(matches!(
        transaction.run(
            "UNWIND range(1, 200) AS n RETURN percentileDisc(n, 0.5)",
            &BTreeMap::new()
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
}
