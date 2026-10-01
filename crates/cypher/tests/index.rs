use std::collections::BTreeMap;

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, find_by_indexed_property, merge_node},
    txn::{Cf, TxnError},
};

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}

fn count(db: &Database, predicate: &str) -> Vec<Vec<Out>> {
    run(
        db,
        &format!("MATCH (c:Customer {{{predicate}}}) RETURN count(*)"),
    )
}

#[test]
fn indexed_strings_follow_create_set_null_delete_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    assert!(db.scalar_indexes_ready().unwrap());
    run(&db, "CREATE (a:Customer {entity_id:'a', email:'same'}), (b:Customer {entity_id:'b', email:'same'}), (c:Customer {entity_id:'c', email:'other'})");
    let ids = find_by_indexed_property(
        &db.begin(),
        catalog.node_type("Customer").unwrap(),
        "email",
        &Value::Str("same".into()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(
        count(&db, "email:'same'"),
        vec![vec![Out::Value(Value::Int(2))]]
    );
    run(&db, "MATCH (c:Customer {entity_id:'a'}) SET c.email='new'");
    assert_eq!(
        count(&db, "email:'same'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    assert_eq!(
        count(&db, "email:'new'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    run(&db, "MATCH (c:Customer {entity_id:'a'}) SET c.email=null");
    assert_eq!(
        count(&db, "email:'new'"),
        vec![vec![Out::Value(Value::Int(0))]]
    );
    run(&db, "MATCH (c:Customer {entity_id:'b'}) DETACH DELETE c");
    assert_eq!(
        count(&db, "email:'same'"),
        vec![vec![Out::Value(Value::Int(0))]]
    );
    check_invariants(&db).unwrap();
    drop(db);
    let reopened = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert!(reopened.scalar_indexes_ready().unwrap());
    assert_eq!(
        count(&reopened, "email:'other'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
}

#[test]
fn applying_new_index_backfills_and_old_unmarked_catalog_still_scans() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let base: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"]}]}"#,
    )
    .unwrap();
    db.apply_ontology(&base).unwrap();
    run(&db, "CREATE (c:Customer {entity_id:'a', email:'before'})");
    let indexed: GraphMapping = serde_json::from_str(
        r#"{"version":"2","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    )
    .unwrap();
    let catalog = db.apply_ontology(&indexed).unwrap();
    assert_eq!(
        count(&db, "email:'before'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    assert_eq!(
        find_by_indexed_property(
            &db.begin(),
            catalog.node_type("Customer").unwrap(),
            "email",
            &Value::Str("before".into()),
        )
        .unwrap()
        .unwrap()
        .len(),
        1
    );
    // Simulate an older store with a declaration but no ready index marker.
    let mut tx = db.begin();
    tx.delete(Cf::Meta, b"scalar-index-v1-ready".to_vec());
    for (key, _) in tx.scan_prefix(Cf::Idx, &[]).unwrap() {
        tx.delete(Cf::Idx, key);
    }
    tx.commit().unwrap();
    assert!(!db.scalar_indexes_ready().unwrap());
    assert_eq!(
        count(&db, "email:'before'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    db.apply_ontology(&indexed).unwrap();
    assert!(db.scalar_indexes_ready().unwrap());
    assert_eq!(
        count(&db, "email:'before'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
}

#[test]
fn removing_and_readding_a_declaration_discards_stale_keys() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let indexed: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    )
    .unwrap();
    let plain: GraphMapping = serde_json::from_str(
        r#"{"version":"2","nodes":[{"label":"Customer","properties":["email"]}]}"#,
    )
    .unwrap();
    db.apply_ontology(&indexed).unwrap();
    run(&db, "CREATE (c:Customer {entity_id:'a', email:'old'})");
    db.apply_ontology(&plain).unwrap();
    run(&db, "MATCH (c:Customer {entity_id:'a'}) SET c.email='new'");
    assert_eq!(
        count(&db, "email:'old'"),
        vec![vec![Out::Value(Value::Int(0))]]
    );
    db.apply_ontology(&indexed).unwrap();
    let ty = db
        .catalog()
        .unwrap()
        .unwrap()
        .node_type("Customer")
        .unwrap()
        .clone();
    assert!(
        find_by_indexed_property(&db.begin(), &ty, "email", &Value::Str("old".into()))
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        count(&db, "email:'new'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
}

#[test]
fn boolean_index_is_type_tagged_and_numeric_equality_falls_back_to_scan() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["active","score"],"indexed_fields":["active","score"]}]}"#,
    )
    .unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (a:Customer {entity_id:'a', active:true, score:1}), (b:Customer {entity_id:'b', active:'true', score:2})");
    let ty = catalog.node_type("Customer").unwrap();
    assert_eq!(
        find_by_indexed_property(&db.begin(), ty, "active", &Value::Bool(true))
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        find_by_indexed_property(&db.begin(), ty, "active", &Value::Str("true".into()))
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert!(
        find_by_indexed_property(&db.begin(), ty, "score", &Value::Int(1))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(&db, "active:true"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    assert_eq!(
        count(&db, "active:'true'"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    assert_eq!(
        count(&db, "score:1.0"),
        vec![vec![Out::Value(Value::Int(1))]]
    );
}

#[test]
fn rollback_and_conflicting_writers_never_publish_uncommitted_index_keys() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    )
    .unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (c:Customer {entity_id:'a', email:'old'})");
    let mut first = db.begin();
    let mut second = db.begin();
    merge_node(
        &mut first,
        &catalog,
        "Customer",
        "a",
        &BTreeMap::from([("email".into(), Value::Str("winner".into()))]),
    )
    .unwrap();
    merge_node(
        &mut second,
        &catalog,
        "Customer",
        "a",
        &BTreeMap::from([("email".into(), Value::Str("loser".into()))]),
    )
    .unwrap();
    first.commit().unwrap();
    assert!(matches!(second.commit(), Err(TxnError::Conflict)));
    let mut aborted = db.begin();
    merge_node(
        &mut aborted,
        &catalog,
        "Customer",
        "a",
        &BTreeMap::from([("email".into(), Value::Str("aborted".into()))]),
    )
    .unwrap();
    drop(aborted);
    for (email, expected) in [("winner", 1), ("old", 0), ("loser", 0), ("aborted", 0)] {
        assert_eq!(
            count(&db, &format!("email:'{email}'")),
            vec![vec![Out::Value(Value::Int(expected))]]
        );
    }
}

#[test]
fn malformed_index_key_fails_closed_instead_of_returning_a_wrong_node() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    )
    .unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (c:Customer {entity_id:'a', email:'value'})");
    let mut tx = db.begin();
    let mut bad_key = tx.scan_prefix(Cf::Idx, &[]).unwrap()[0].0.clone();
    bad_key.pop();
    tx.put(Cf::Idx, bad_key, vec![]);
    tx.commit().unwrap();
    assert!(find_by_indexed_property(
        &db.begin(),
        catalog.node_type("Customer").unwrap(),
        "email",
        &Value::Str("value".into()),
    )
    .is_err());
}

#[test]
fn invariant_audit_detects_missing_and_stale_active_index_entries() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    ).unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (c:Customer {entity_id:'a', email:'old'})");
    let key = db.begin().scan_prefix(Cf::Idx, &[]).unwrap()[0].0.clone();
    let mut tx = db.begin();
    tx.delete(Cf::Idx, key.clone());
    tx.commit().unwrap();
    assert!(check_invariants(&db).unwrap_err().contains("missing index"));
    let mut tx = db.begin();
    tx.put(Cf::Idx, key.clone(), vec![]);
    tx.commit().unwrap();
    check_invariants(&db).unwrap();
    run(&db, "MATCH (c:Customer {entity_id:'a'}) SET c.email='new'");
    let mut tx = db.begin();
    tx.put(Cf::Idx, key, vec![]);
    tx.commit().unwrap();
    assert!(check_invariants(&db).unwrap_err().contains("stale index"));
}

#[test]
fn indexed_and_scan_paths_agree_across_mixed_type_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email"],"indexed_fields":["email"]}]}"#,
    ).unwrap();
    db.apply_ontology(&mapping).unwrap();
    let values = [
        Value::Str("prefix".into()),
        Value::Str("prefix\0suffix".into()),
        Value::Str("ação/東京".into()),
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(1),
        Value::Float(1.0),
        Value::Null,
        Value::List(vec![Value::Int(1)]),
    ];
    for phase in 0..4 {
        for i in 0..64 {
            let params = BTreeMap::from([
                ("id".into(), Value::Str(format!("customer-{i}"))),
                (
                    "value".into(),
                    values[(i + phase * 3) % values.len()].clone(),
                ),
            ]);
            execute(
                &db,
                "MERGE (c:Customer {entity_id:$id}) SET c.email=$value",
                &params,
            )
            .unwrap();
        }
        for value in &values {
            let params = BTreeMap::from([("value".into(), value.clone())]);
            let indexed = execute(
                &db,
                "MATCH (c:Customer {email:$value}) RETURN count(*)",
                &params,
            )
            .unwrap();
            let scanned = execute(
                &db,
                "MATCH (c:Customer) WHERE c.email=$value RETURN count(*)",
                &params,
            )
            .unwrap();
            assert_eq!(indexed.rows, scanned.rows, "phase {phase}, {value:?}");
        }
        check_invariants(&db).unwrap();
    }
}
