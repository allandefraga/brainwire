use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn list_scopes_admit_parent_copies_and_release_pool_after_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(32768)))]);
    let pool = Arc::new(QueryMemoryPool::new(150_000));
    for query in [
        "WITH $payload AS p RETURN [x IN [1] WHERE false | x]",
        "WITH $payload AS p RETURN reduce(s = 0, x IN [1] | s + x)",
    ] {
        let mut tx = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: 1 << 20,
                timeout: None,
            },
            pool.clone(),
        )
        .unwrap();
        let result = tx.run(query, &params);
        assert!(
            matches!(result, Err(ExecError::MemoryPoolExhausted { .. })),
            "{query}: {result:?}"
        );
        assert_eq!(pool.usage().reserved_bytes, 0);
        drop(tx);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    tx.run("WITH $payload AS p RETURN true", &params).unwrap();
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn list_scopes_reuse_temporary_budget_without_changing_results() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(16384)))]);
    let pool = Arc::new(QueryMemoryPool::new(512_000));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 512_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let result = tx.run(
        "WITH $payload AS p RETURN [x IN range(1,1000) WHERE x = 1000 | x + 1], reduce(s = 0, x IN range(1,1000) | s + x)",
        &params,
    ).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::List(vec![Value::Int(1001)])),
            Out::Value(Value::Int(500500))
        ]]
    );
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn comprehension_keeps_accumulated_output_in_its_budget() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(8192)))]);
    let pool = Arc::new(QueryMemoryPool::new(200_000));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 200_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let result = tx.run("RETURN [x IN range(1,100) | $payload]", &params);
    assert!(
        matches!(
            result,
            Err(ExecError::MemoryLimit { .. }) | Err(ExecError::MemoryPoolExhausted { .. })
        ),
        "{result:?}"
    );
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn comprehension_does_not_double_charge_its_completed_list() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(8192)))]);
    let pool = Arc::new(QueryMemoryPool::new(150_000));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 150_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let result = tx
        .run("RETURN size([x IN range(1,10) | $payload])", &params)
        .unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(10))]]);
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}
