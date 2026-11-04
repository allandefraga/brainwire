use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, ExecutionLimits, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn quantifier_temporary_bindings_obey_pool_and_release() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(32768)))]);
    let pool = Arc::new(QueryMemoryPool::new(150_000));
    let mut input_only = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    input_only
        .run("WITH $payload AS p RETURN true", &params)
        .unwrap();
    drop(input_only);
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let result = tx.run(
        "WITH $payload AS p RETURN any(x IN [1] WHERE true)",
        &params,
    );
    assert!(
        matches!(result, Err(ExecError::MemoryPoolExhausted { .. })),
        "{result:?}"
    );
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn quantifier_releases_each_binding_and_preserves_null_semantics() {
    use brainwire_cypher::{execute, Out};
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(512_000));
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(16384)))]);
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let output = tx
        .run(
            "WITH $payload AS p RETURN all(x IN range(1,1000) WHERE x > 0)",
            &params,
        )
        .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Bool(true))]]);
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
    let out=execute(&db,"RETURN any(x IN [false,null] WHERE x), all(x IN [true,null] WHERE x), none(x IN [false,null] WHERE x), single(x IN [true,false] WHERE x)", &BTreeMap::new()).unwrap();
    assert_eq!(
        out.rows,
        vec![vec![
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Bool(true))
        ]]
    );
}
