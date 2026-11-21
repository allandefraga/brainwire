use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn public_transaction_returns_small_scan_projection_under_tight_budget() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Doc","properties":["title","unused"]}],"edges":[]}"#,
    )
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    execute(
        &db,
        "CREATE (:Doc {entity_id:'synthetic',title:'small',unused:$unused})",
        &BTreeMap::from([("unused".into(), Value::Str("u".repeat(40 << 10)))]),
    )
    .unwrap();
    let params = BTreeMap::from([("payload".into(), Value::Str("p".repeat(8 << 10)))]);
    let pool = Arc::new(QueryMemoryPool::new(1 << 20));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 64 << 10,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let output = tx
        .run(
            "MATCH (d:Doc) RETURN d.title AS title, $payload AS payload",
            &params,
        )
        .unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Str("small".into())),
            Out::Value(Value::Str("p".repeat(8 << 10)))
        ]]
    );
    eprintln!(
        "public executor peak={} reserved={}",
        pool.usage().peak_bytes,
        pool.usage().reserved_bytes
    );
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
}
