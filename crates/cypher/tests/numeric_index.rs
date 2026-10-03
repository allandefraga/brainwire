use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, find_by_indexed_property, merge_node},
    txn::Cf,
};
use std::collections::BTreeMap;

fn mapping(indexed: bool) -> GraphMapping {
    serde_json::from_str(&format!(r#"{{"version":"1","nodes":[{{"label":"Item","properties":["value"],"indexed_fields":{}}}],"edges":[]}}"#,if indexed {r#"["value"]"#} else {"[]"})).unwrap()
}
#[test]
fn numeric_index_matches_scan_at_precision_boundaries_and_survives_overwrite() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let indexed = Database::open(a.path(), &resources).unwrap();
    let scan = Database::open(b.path(), &resources).unwrap();
    let catalog = indexed.apply_ontology(&mapping(true)).unwrap();
    let plain = scan.apply_ontology(&mapping(false)).unwrap();
    let values = vec![
        Value::Int(0),
        Value::Float(-0.0),
        Value::Int(1),
        Value::Float(1.0),
        Value::Float(1.25),
        Value::Float(-1.25),
        Value::Int(i64::MIN),
        Value::Float(i64::MIN as f64),
        Value::Int(i64::MAX),
        Value::Float(9223372036854775808.0),
        Value::Int(9007199254740993),
        Value::Float(9007199254740992.0),
        Value::Int(-9007199254740993),
        Value::Float(-9007199254740992.0),
        Value::Str("1".into()),
        Value::Bool(true),
        Value::Null,
    ];
    for (db, catalog) in [(&indexed, &catalog), (&scan, &plain)] {
        let mut tx = db.begin();
        for (i, value) in values.iter().enumerate() {
            merge_node(
                &mut tx,
                catalog,
                "Item",
                &format!("item-{i}"),
                &BTreeMap::from([("value".into(), value.clone())]),
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    for value in &values {
        let params = BTreeMap::from([("value".into(), value.clone())]);
        for query in [
            "MATCH (i:Item) WHERE i.value=$value RETURN i.entity_id",
            "MATCH (i:Item {value:$value}) RETURN i.entity_id",
            "MATCH (i:Item) WHERE $value=i.value RETURN i.entity_id",
        ] {
            assert_eq!(
                execute(&indexed, query, &params).unwrap().rows,
                execute(&scan, query, &params).unwrap().rows,
                "{value:?}: {query}"
            );
        }
        if matches!(value, Value::Int(_) | Value::Float(_)) {
            assert!(find_by_indexed_property(
                &indexed.begin(),
                &catalog.node_types[0],
                "value",
                value
            )
            .unwrap()
            .is_some());
        }
    }
    let query = "MATCH (i:Item) WHERE i.value=1.25 RETURN i.entity_id";
    let mut old = QueryTransaction::begin(&indexed).unwrap();
    let mut tx = QueryTransaction::begin(&indexed).unwrap();
    tx.run(
        "MATCH (i:Item) WHERE i.entity_id='item-4' SET i.value=42",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(tx.run(query, &BTreeMap::new()).unwrap().rows.is_empty());
    tx.commit().unwrap();
    assert_eq!(
        old.run(query, &BTreeMap::new()).unwrap().rows,
        vec![vec![Out::Value(Value::Str("item-4".into()))]]
    );
    assert!(execute(&indexed, query, &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
    execute(
        &indexed,
        "MATCH (i:Item) WHERE i.value=42 SET i.value=null",
        &BTreeMap::new(),
    )
    .unwrap();
    check_invariants(&indexed).unwrap();
    drop(tx);
    drop(old);
    drop(indexed);
    check_invariants(&Database::open(a.path(), &resources).unwrap()).unwrap();
}

#[test]
fn v1_ready_marker_cannot_enable_numeric_index_before_atomic_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping = mapping(true);
    let catalog = db.apply_ontology(&mapping).unwrap();
    let mut tx = db.begin();
    merge_node(
        &mut tx,
        &catalog,
        "Item",
        "legacy",
        &BTreeMap::from([("value".into(), Value::Int(7))]),
    )
    .unwrap();
    tx.commit().unwrap();
    let mut tx = db.begin();
    for (key, _) in tx.scan_prefix(Cf::Idx, &[]).unwrap() {
        tx.delete(Cf::Idx, key);
    }
    tx.delete(Cf::Meta, b"scalar-index-v2-ready".to_vec());
    tx.put(Cf::Meta, b"scalar-index-v1-ready".to_vec(), vec![1]);
    tx.commit().unwrap();
    assert!(!db.scalar_indexes_ready().unwrap());
    let query = "MATCH (i:Item) WHERE i.value=7 RETURN i.entity_id";
    assert_eq!(execute(&db, query, &BTreeMap::new()).unwrap().rows.len(), 1);
    let mut tx = db.begin();
    tx.put(Cf::Meta, b"scalar-index-v2-ready".to_vec(), vec![1]);
    tx.commit().unwrap();
    assert!(!db.scalar_indexes_ready().unwrap());
    assert_eq!(execute(&db, query, &BTreeMap::new()).unwrap().rows.len(), 1);
    db.apply_ontology(&mapping).unwrap();
    assert!(db.scalar_indexes_ready().unwrap());
    assert_eq!(execute(&db, query, &BTreeMap::new()).unwrap().rows.len(), 1);
    check_invariants(&db).unwrap();
}

#[test]
fn oversized_v1_migration_keeps_catalog_marker_and_existing_entries_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let original = mapping(true);
    let catalog = db.apply_ontology(&original).unwrap();
    // v1 string keys have the same encoding. 50,001 removals + insertions
    // exceed the atomic cap even though the graph itself has fewer than 100K nodes.
    for start in (0..50_001).step_by(1000) {
        let mut tx = db.begin();
        for i in start..(start + 1000).min(50_001) {
            merge_node(
                &mut tx,
                &catalog,
                "Item",
                &format!("legacy-{i}"),
                &BTreeMap::from([("value".into(), Value::Str("old".into()))]),
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let mut tx = db.begin();
    tx.delete(Cf::Meta, b"scalar-index-v2-ready".to_vec());
    tx.put(Cf::Meta, b"scalar-index-v1-ready".to_vec(), vec![1]);
    tx.commit().unwrap();
    let original_entries = db.begin().scan_prefix(Cf::Idx, &[]).unwrap();
    let mut changed = original.clone();
    changed.version = "2".into();
    changed.nodes[0].properties.push("new_column".into());
    assert!(matches!(
        db.apply_ontology(&changed),
        Err(brainwire_store::graph::GraphError::IndexBackfillTooLarge)
    ));
    assert_eq!(db.catalog().unwrap().unwrap().version, catalog.version);
    assert_eq!(
        db.begin().get(Cf::Meta, b"scalar-index-v1-ready").unwrap(),
        Some(vec![1])
    );
    assert!(!db.scalar_indexes_ready().unwrap());
    assert_eq!(
        db.begin().scan_prefix(Cf::Idx, &[]).unwrap(),
        original_entries
    );
    assert_eq!(
        execute(
            &db,
            "MATCH (i:Item) WHERE i.value='old' RETURN count(*)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![Out::Value(Value::Int(50_001))]]
    );
}
