use std::{collections::BTreeMap, sync::Arc};

use brainwire_core::{acl::NodeAcl, schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{
        check_invariants, get_node_acl, merge_edge, merge_keyed_edge, merge_node, set_node_acl,
    },
};

fn setup() -> (tempfile::TempDir, Database, brainwire_core::ids::NodeId) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Item","properties":["email"],"indexed_fields":["email"]}],"edges":[{"type":"LINK","from":{"label":"Item"},"to":{"label":"Item"}},{"type":"EVENT","from":{"label":"Item"},"to":{"label":"Item"},"properties":["kind"],"merge_key_fields":["kind"]}]}"#).unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    let mut tx = db.begin();
    let hub = merge_node(
        &mut tx,
        &catalog,
        "Item",
        "hub",
        &BTreeMap::from([("email".into(), Value::Str("hub@example.test".into()))]),
    )
    .unwrap();
    set_node_acl(
        &mut tx,
        hub,
        &NodeAcl {
            allow: vec!["reader".into()],
            ..Default::default()
        },
    )
    .unwrap();
    for i in 0..2048 {
        let neighbor = merge_node(
            &mut tx,
            &catalog,
            "Item",
            &format!("item-{i}"),
            &BTreeMap::new(),
        )
        .unwrap();
        merge_edge(&mut tx, &catalog, hub, "LINK", neighbor, &BTreeMap::new()).unwrap();
        if i < 32 {
            merge_edge(&mut tx, &catalog, neighbor, "LINK", hub, &BTreeMap::new()).unwrap();
        }
        if i < 2 {
            merge_keyed_edge::<brainwire_store::graph::GraphError>(
                &mut tx,
                &catalog,
                hub,
                "EVENT",
                neighbor,
                false,
                &BTreeMap::from([("kind".into(), Value::Str(format!("phase-{i}")))]),
                |_| Ok(()),
            )
            .unwrap();
        }
    }
    merge_edge(&mut tx, &catalog, hub, "LINK", hub, &BTreeMap::new()).unwrap();
    merge_keyed_edge::<brainwire_store::graph::GraphError>(
        &mut tx,
        &catalog,
        hub,
        "EVENT",
        hub,
        false,
        &BTreeMap::from([("kind".into(), Value::Str("self".into()))]),
        |_| Ok(()),
    )
    .unwrap();
    tx.commit().unwrap();
    (dir, db, hub)
}

fn count(db: &Database, query: &str) -> i64 {
    match &execute(db, query, &BTreeMap::new()).unwrap().rows[0][0] {
        Out::Value(Value::Int(value)) => *value,
        other => panic!("expected count, got {other:?}"),
    }
}

#[test]
fn detach_memory_refusal_rolls_back_prior_writes_and_releases_the_pool() {
    let (_dir, db, hub) = setup();
    let pool = Arc::new(QueryMemoryPool::new(128 * 1024));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 128 * 1024,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    tx.run("CREATE (:Item {entity_id:'pending'})", &BTreeMap::new())
        .unwrap();
    assert!(matches!(
        tx.run(
            "MATCH (n:Item {entity_id:'hub'}) DETACH DELETE n",
            &BTreeMap::new()
        ),
        Err(ExecError::MemoryLimit { .. }) | Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert!(tx.commit().is_err());
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(pool.usage().peak_bytes <= 128 * 1024);
    assert_eq!(
        count(&db, "MATCH (n:Item {entity_id:'pending'}) RETURN count(n)"),
        0
    );
    assert_eq!(
        count(
            &db,
            "MATCH (n:Item {email:'hub@example.test'}) RETURN count(n)"
        ),
        1
    );
    assert!(get_node_acl(&db.begin(), hub).unwrap().is_some());
    assert_eq!(
        count(&db, "MATCH (:Item)-[r]->(:Item) RETURN count(r)"),
        2084
    );
    check_invariants(&db).unwrap();
}

#[test]
fn admitted_detach_removes_all_directions_self_loops_keys_indexes_and_acl() {
    let (_dir, db, hub) = setup();
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 8 * 1024 * 1024,
            timeout: None,
        },
    )
    .unwrap();
    tx.run(
        "MATCH (n:Item {entity_id:'hub'}) DETACH DELETE n",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(count(&db, "MATCH (n:Item) RETURN count(n)"), 2048);
    assert_eq!(
        count(
            &db,
            "MATCH (n:Item {email:'hub@example.test'}) RETURN count(n)"
        ),
        0
    );
    assert_eq!(count(&db, "MATCH (:Item)-[r]->(:Item) RETURN count(r)"), 0);
    assert!(get_node_acl(&db.begin(), hub).unwrap().is_none());
    check_invariants(&db).unwrap();
}

#[test]
fn ordinary_delete_refuses_a_connected_hub_without_collecting_its_adjacency() {
    let (_dir, db, _) = setup();
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 32 * 1024,
            timeout: None,
        },
    )
    .unwrap();
    assert!(matches!(
        tx.run(
            "MATCH (n:Item {entity_id:'hub'}) DELETE n",
            &BTreeMap::new()
        ),
        Err(ExecError::ConstraintViolation(_))
    ));
    assert_eq!(count(&db, "MATCH (n:Item) RETURN count(n)"), 2049);
    check_invariants(&db).unwrap();
}
