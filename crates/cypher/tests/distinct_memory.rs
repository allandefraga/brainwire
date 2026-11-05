use brainwire_core::value::Value;
use brainwire_cypher::{
    execute, ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn distinct_and_group_memory_refusals_release_the_shared_pool() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let limit = 128 << 10;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(48 << 10)))]);
    for query in [
        "RETURN DISTINCT $payload",
        "UNWIND [1,2] AS n RETURN count(DISTINCT $payload)",
        "UNWIND [1,2] AS n RETURN $payload AS key,count(*)",
        "RETURN $payload AS x UNION RETURN $payload + 'y' AS x",
    ] {
        let mut tx = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: limit,
                timeout: None,
            },
            Arc::clone(&pool),
        )
        .unwrap();
        assert!(
            matches!(tx.run(query, &params), Err(ExecError::MemoryLimit { .. })),
            "{query}"
        );
        assert_eq!(pool.usage().reserved_bytes, 0, "{query}");
    }
}

#[test]
fn distinct_memory_refusal_rolls_back_an_earlier_write() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    db.apply_ontology(
        &serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
            .unwrap(),
    )
    .unwrap();
    let limit = 128 << 10;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: limit,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    tx.run("CREATE (:Customer {entity_id:'pending'})", &BTreeMap::new())
        .unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(48 << 10)))]);
    assert!(matches!(
        tx.run("RETURN DISTINCT $payload", &params),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(tx.commit().is_err());
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert_eq!(
        execute(&db, "MATCH (n:Customer) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn successful_distinct_result_releases_its_retained_pool_lease() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let limit = 256 << 10;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(8 << 10)))]);
    for query in [
        "UNWIND [1,2] AS n RETURN DISTINCT $payload",
        "RETURN $payload AS x UNION RETURN $payload AS x",
    ] {
        let mut tx = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: limit,
                timeout: None,
            },
            Arc::clone(&pool),
        )
        .unwrap();
        let result = tx.run(query, &params).unwrap();
        assert_eq!(result.rows.len(), 1);
        let retained_result = (result, tx.output_memory_lease());
        tx.commit().unwrap();
        drop(tx);
        assert!(pool.usage().reserved_bytes > 0);
        drop(retained_result);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}

#[test]
fn union_preserves_first_seen_order_nulls_and_all_duplicates() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(
        execute(
            &db,
            "UNWIND [1,1.0,null] AS n RETURN n UNION RETURN 2 AS n UNION RETURN null AS n",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![
            vec![Out::Value(Value::Int(1))],
            vec![Out::Value(Value::Null)],
            vec![Out::Value(Value::Int(2))]
        ]
    );
    assert_eq!(
        execute(
            &db,
            "RETURN 1 AS n UNION ALL RETURN 1 AS n",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![
            vec![Out::Value(Value::Int(1))],
            vec![Out::Value(Value::Int(1))]
        ]
    );
}

#[test]
fn distinct_keeps_numeric_null_and_nested_value_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(
        execute(
            &db,
            "UNWIND [1,1.0,2,null,null] AS n RETURN DISTINCT n",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![
            vec![Out::Value(Value::Int(1))],
            vec![Out::Value(Value::Int(2))],
            vec![Out::Value(Value::Null)]
        ]
    );
    assert_eq!(
        execute(
            &db,
            "UNWIND [1,1.0,2,null,null] AS n RETURN collect(DISTINCT n),count(DISTINCT n)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![
            Out::Value(Value::List(vec![Value::Int(1), Value::Int(2)])),
            Out::Value(Value::Int(2))
        ]]
    );
    let params = BTreeMap::from([(
        "values".into(),
        Value::List(vec![
            Value::Map(BTreeMap::from([(
                "key".into(),
                Value::List(vec![Value::Int(1)]),
            )])),
            Value::Map(BTreeMap::from([(
                "key".into(),
                Value::List(vec![Value::Float(1.0)]),
            )])),
            Value::Map(BTreeMap::from([(
                "key".into(),
                Value::List(vec![Value::Int(2)]),
            )])),
        ]),
    )]);
    assert_eq!(
        execute(&db, "UNWIND $values AS n RETURN count(DISTINCT n)", &params)
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(2))]]
    );
    assert_eq!(
        execute(
            &db,
            "UNWIND ['a','a','b'] AS n RETURN n,count(*) ORDER BY n",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![
            vec![
                Out::Value(Value::Str("a".into())),
                Out::Value(Value::Int(2))
            ],
            vec![
                Out::Value(Value::Str("b".into())),
                Out::Value(Value::Int(1))
            ]
        ]
    );
}
