use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn absent_replacement_pattern_does_not_reserve_a_large_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(256_000));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 256_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let params = BTreeMap::from([
        ("text".into(), Value::Str("a".repeat(1024))),
        ("replacement".into(), Value::Str("x".repeat(8192))),
    ]);
    let result = tx
        .run("RETURN replace($text, 'missing', $replacement)", &params)
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Out::Value(Value::Str("a".repeat(1024)))]]
    );
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn replacement_preserves_unicode_empty_patterns_and_nonoverlapping_matches() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    let result = tx.run(
        "RETURN replace('é🦀', '', '-'), replace('aaa', 'aa', 'x'), replace('', '', 'x'), replace('abab', 'ab', ''), replace(null, 'x', 'y')",
        &BTreeMap::new(),
    ).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Out::Value(Value::Str("-é-🦀-".into())),
            Out::Value(Value::Str("xa".into())),
            Out::Value(Value::Str("x".into())),
            Out::Value(Value::Str("".into())),
            Out::Value(Value::Null),
        ]]
    );
}

#[test]
fn replacement_expansion_refusal_releases_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(100_000));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 100_000,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap();
    let params = BTreeMap::from([
        ("text".into(), Value::Str("a".repeat(1000))),
        ("replacement".into(), Value::Str("x".repeat(1000))),
    ]);
    assert!(matches!(
        tx.run("RETURN replace($text, '', $replacement)", &params),
        Err(ExecError::MemoryLimit { .. }) | Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
}
