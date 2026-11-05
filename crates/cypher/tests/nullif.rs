use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, parse, ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

fn database() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

#[test]
fn selects_null_only_for_equal_values_and_preserves_original_types() {
    let (_dir, db) = database();
    let output = execute(
        &db,
        "RETURN nullIf(4, 4), nullIf('é🦀', 'other'), NuLlIf(1, 1.0), nullIf(1, '1'), nullIf(null, 1), nullIf(1, null), nullIf(null, null), coalesce(nullIf('', ''), 'fallback')",
        &BTreeMap::new(),
    ).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Null),
            Out::Value(Value::Str("é🦀".into())),
            Out::Value(Value::Null),
            Out::Value(Value::Int(1)),
            Out::Value(Value::Null),
            Out::Value(Value::Int(1)),
            Out::Value(Value::Null),
            Out::Value(Value::Str("fallback".into())),
        ]]
    );
    let params = BTreeMap::from([("large".into(), Value::Int(9007199254740993))]);
    let output = execute(&db, "RETURN nullIf($large, 9007199254740992.0)", &params).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![Out::Value(Value::Int(9007199254740993))]]
    );
}

#[test]
fn uses_cypher_equality_for_nested_lists_maps_and_unknown_comparisons() {
    let (_dir, db) = database();
    let output = execute(&db,
        "RETURN nullIf([1, {x: 'a'}], [1.0, {x: 'a'}]), nullIf({x: 2}, {x: 3}), nullIf([null], [null]), nullIf({x: null}, {x: null})",
        &BTreeMap::new()).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Null),
            Out::Value(Value::Map(BTreeMap::from([("x".into(), Value::Int(2))]))),
            Out::Value(Value::List(vec![Value::Null])),
            Out::Value(Value::Map(BTreeMap::from([("x".into(), Value::Null)]))),
        ]]
    );
}

#[test]
fn validates_arity_and_evaluates_errors_without_committing_prior_writes() {
    let (_dir, db) = database();
    for query in [
        "RETURN nullIf()",
        "RETURN nullIf(1)",
        "RETURN nullIf(1, 2, 3)",
    ] {
        assert!(parse(query).is_err(), "{query}");
    }
    assert!(matches!(
        execute(
            &db,
            "CREATE (:Customer {entity_id: 'pending'}) RETURN nullIf(null, $missing)",
            &BTreeMap::new()
        ),
        Err(ExecError::ParameterMissing(_))
    ));
    assert_eq!(
        execute(&db, "MATCH (n:Customer) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
    execute(
        &db,
        "CREATE (:Customer {entity_id: 'one', name: ''})",
        &BTreeMap::new(),
    )
    .unwrap();
    let output = execute(
        &db,
        "MATCH (n:Customer) SET n.name = nullIf(n.name, '') RETURN n.name",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Null)]]);
}

#[test]
fn selected_payload_obeys_query_budget_and_releases_pool() {
    let (_dir, db) = database();
    let params = BTreeMap::from([("text".into(), Value::Str("x".repeat(8192)))]);
    let pool = Arc::new(QueryMemoryPool::new(256_000));
    let mut transaction = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 256_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let output = transaction
        .run("RETURN nullIf($text, 'other')", &params)
        .unwrap();
    assert_eq!(
        output.rows,
        vec![vec![Out::Value(Value::Str("x".repeat(8192)))]]
    );
    drop(output);
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);

    let mut transaction = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 32_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let large = BTreeMap::from([("text".into(), Value::Str("x".repeat(64_000)))]);
    assert!(matches!(
        transaction.run("RETURN nullIf($text, 'other')", &large),
        Err(ExecError::MemoryLimit { .. }) | Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
}
