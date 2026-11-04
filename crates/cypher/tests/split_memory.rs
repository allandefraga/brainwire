use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn split_refuses_while_counting_output_and_releases_its_pool() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let limit = 100_000;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: limit,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let params = BTreeMap::from([("text".into(), Value::Str("a,".repeat(10_000)))]);
    match transaction.run("RETURN split($text, ',')", &params) {
        Err(ExecError::MemoryLimit { attempted, .. }) => assert!(
            attempted <= limit + 256,
            "split counted the whole expanded output before refusing: {attempted}"
        ),
        other => panic!("expected bounded split refusal, got {other:?}"),
    }
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn split_keeps_unicode_empty_separator_and_empty_field_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mut transaction = QueryTransaction::begin(&database).unwrap();
    let result = transaction.run("RETURN split('é🦀', ''), split('', ''), split('a,,b,', ','), split('', ','), split(null, ',')", &BTreeMap::new()).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::List(vec![
                Value::Str("é".into()),
                Value::Str("🦀".into())
            ])),
            Out::Value(Value::List(vec![])),
            Out::Value(Value::List(vec![
                Value::Str("a".into()),
                Value::Str("".into()),
                Value::Str("b".into()),
                Value::Str("".into())
            ])),
            Out::Value(Value::List(vec![Value::Str("".into())])),
            Out::Value(Value::Null),
        ]]
    );
}

#[test]
fn bounded_split_succeeds_under_a_small_budget() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mut transaction = QueryTransaction::begin_with_limits(
        &database,
        ExecutionLimits {
            memory_bytes: 100_000,
            timeout: None,
        },
    )
    .unwrap();
    let params = BTreeMap::from([("text".into(), Value::Str("a,".repeat(1000)))]);
    let result = transaction
        .run("RETURN size(split($text, ','))", &params)
        .unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(1001))]]);
}
