use std::{collections::BTreeMap, sync::Arc};

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryMemoryPool,
    QueryTransaction,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::merge_node,
};

fn setup(indexed: bool, values: &[Value]) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let fields = if indexed { "[\"x\"]" } else { "[]" };
    let mapping: GraphMapping = serde_json::from_str(&format!(r#"{{"version":"1","nodes":[{{"label":"Item","properties":["x"],"indexed_fields":{fields}}}],"edges":[]}}"#)).unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    let mut tx = db.begin();
    for (i, value) in values.iter().enumerate() {
        merge_node(
            &mut tx,
            &catalog,
            "Item",
            &format!("item-{i:04}"),
            &BTreeMap::from([("x".into(), value.clone())]),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    (dir, db)
}
fn limits(bytes: usize) -> ExecutionLimits {
    ExecutionLimits {
        memory_bytes: bytes,
        timeout: None,
    }
}
fn params(values: Vec<Value>) -> BTreeMap<String, Value> {
    BTreeMap::from([("values".into(), Value::List(values))])
}
const COUNT: &str = "MATCH (n:Item) WHERE n.x IN $values RETURN count(n)";
const IDS: &str = "MATCH (n:Item) WHERE n.x IN $values RETURN n.entity_id ORDER BY n.entity_id";

#[test]
fn candidate_set_is_admitted_before_growth_and_failure_rolls_back_prior_writes() {
    let (_dir, db) = setup(true, &vec![Value::Int(1); 2048]);
    let mut tx = QueryTransaction::begin_with_limits(&db, limits(32 * 1024)).unwrap();
    tx.run("CREATE (:Item {entity_id:'pending',x:2})", &BTreeMap::new())
        .unwrap();
    assert!(matches!(
        tx.run(COUNT, &params(vec![Value::Int(1)])),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(tx.commit().is_err());
    assert_eq!(
        execute(
            &db,
            "MATCH (n:Item {entity_id:'pending'}) RETURN count(n)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn candidate_pressure_obeys_shared_pool_and_releases_on_failure() {
    let (_dir, db) = setup(true, &vec![Value::Int(1); 2048]);
    let pool = Arc::new(QueryMemoryPool::new(32 * 1024));
    let mut tx =
        QueryTransaction::begin_with_pool(&db, limits(1024 * 1024), Arc::clone(&pool)).unwrap();
    assert!(matches!(
        tx.run(COUNT, &params(vec![Value::Int(1)])),
        Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(pool.usage().peak_bytes <= 32 * 1024);
    let mut small =
        QueryTransaction::begin_with_pool(&db, limits(1024 * 1024), Arc::clone(&pool)).unwrap();
    assert_eq!(
        small.run("RETURN 1", &BTreeMap::new()).unwrap().rows,
        vec![vec![Out::Value(Value::Int(1))]]
    );
    small.rollback();
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn mixed_in_members_keep_exact_scan_results_and_do_not_emit_before_fallback() {
    let values = vec![
        Value::Int(i64::MIN),
        Value::Float(i64::MIN as f64),
        Value::Int(0),
        Value::Float(-0.0),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Int(i64::MAX),
        Value::List(vec![Value::Int(7)]),
        Value::Str("match".into()),
    ];
    let (_a, indexed) = setup(true, &values);
    let (_b, scan) = setup(false, &values);
    for members in [
        values.clone(),
        vec![Value::Int(0), Value::Int(0), Value::Float(-0.0)],
        vec![Value::Int(i64::MIN), Value::Float(i64::MIN as f64)],
        vec![Value::Str("match".into()), Value::Float(f64::INFINITY)],
        vec![Value::Int(0), Value::List(vec![Value::Int(7)])],
    ] {
        let input = params(members);
        assert_eq!(
            execute(&indexed, IDS, &input).unwrap().rows,
            execute(&scan, IDS, &input).unwrap().rows
        );
    }
}

#[test]
fn duplicate_members_and_ids_do_not_retain_duplicate_set_charges() {
    let (_dir, db) = setup(true, &vec![Value::Int(1); 100]);
    let result = execute_with_limits(
        &db,
        COUNT,
        &params(vec![Value::Int(1); 500]),
        limits(128 * 1024),
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(100))]]);
}

#[test]
fn unsupported_late_member_discards_partial_candidates_before_scan() {
    let (_dir, db) = setup(true, &vec![Value::Int(1); 300]);
    let result = execute_with_limits(
        &db,
        COUNT,
        &params(vec![Value::Int(1), Value::Float(f64::INFINITY)]),
        limits(48 * 1024),
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(300))]]);
}

#[test]
fn in_keeps_acl_snapshot_overlay_and_unready_index_fallback() {
    use brainwire_core::acl::{NodeAcl, ReadVisibility};
    use brainwire_store::{
        graph::{find_by_entity_id, set_node_acl},
        txn::Cf,
    };
    let (_dir, db) = setup(true, &vec![Value::Str("shared".into()); 3]);
    let ty = db.catalog().unwrap().unwrap().node_type("Item").unwrap().id;
    let mut stored = db.begin();
    for (identity, principal) in [
        ("item-0000", "bob"),
        ("item-0001", "alice"),
        ("item-0002", "alice"),
    ] {
        let id = find_by_entity_id(&stored, identity, Some(ty)).unwrap()[0];
        set_node_acl(
            &mut stored,
            id,
            &NodeAcl {
                allow: vec![principal.into()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    stored.commit().unwrap();
    let input = params(vec![
        Value::Str("shared".into()),
        Value::Str("shared".into()),
    ]);
    let mut reader = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .run(
                "MATCH (n:Item) WHERE n.x IN $values RETURN n.entity_id LIMIT 1",
                &input
            )
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Str("item-0001".into()))]]
    );
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer
        .run(
            "MATCH (n:Item {entity_id:'item-0001'}) SET n.x='changed'",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        writer.run(COUNT, &input).unwrap().rows,
        vec![vec![Out::Value(Value::Int(2))]]
    );
    writer.commit().unwrap();
    // The old read snapshot still has both allowed matches.
    assert_eq!(
        reader.run(COUNT, &input).unwrap().rows,
        vec![vec![Out::Value(Value::Int(2))]]
    );
    reader.rollback();
    let expected = execute(&db, IDS, &input).unwrap().rows;
    let mut stored = db.begin();
    stored.delete(Cf::Meta, b"scalar-index-v2-ready".to_vec());
    stored.commit().unwrap();
    assert_eq!(execute(&db, IDS, &input).unwrap().rows, expected);
}
