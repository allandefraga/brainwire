use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryTransaction, QueryTransactionState};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    replicated_apply::{self, ApplyError, ApplyLimits, Batch},
    txn::Cf,
};
use std::collections::BTreeMap;
fn setup() -> (tempfile::TempDir, Resources, Database) {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    let mut mapping: serde_json::Value =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    mapping["nodes"].as_array_mut().unwrap().push(
        serde_json::json!({"label":"ImportCheckpoint","properties":["next_offset","sha256"]}),
    );
    let mapping: GraphMapping = serde_json::from_value(mapping).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, res, db)
}
#[test]
fn replicated_multi_run_prepares_all_bytes_without_local_mutation_then_applies_once() {
    let (_dir, _res, db) = setup();
    let before = db.commit_sequence();
    let mut tx = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    tx.run(
        "CREATE (c:Customer {entity_id:'one',email:'old'})",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.run(
        "MATCH (c:Customer {entity_id:'one'}) SET c.email='new'",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.run("CREATE (c:Company {entity_id:'company'}) MATCH (a:Customer {entity_id:'one'}) MERGE (a)-[:WORKS_AT]->(c)",&BTreeMap::new()).unwrap();
    tx.run("CALL db.setNodeAcl({label:'Customer',entityId:'one',allow:['user']}) YIELD updated RETURN updated",&BTreeMap::new()).unwrap();
    tx.run(
        "CREATE (c:ImportCheckpoint {entity_id:'cursor',next_offset:128,sha256:'synthetic'})",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(db.commit_sequence(), before);
    assert_eq!(db.stats().unwrap().nodes, 0);
    assert!(db
        .begin()
        .scan_prefix(Cf::Meta, b"seq/")
        .unwrap()
        .is_empty());
    let prepared = tx.prepare_replication().unwrap().unwrap();
    assert_eq!(prepared.expected_revision, 0);
    assert_eq!(tx.state(), QueryTransactionState::Prepared);
    assert_eq!(db.commit_sequence(), before);
    let batch = Batch::from_canonical_bytes(&prepared.batch, ApplyLimits::default()).unwrap();
    assert_eq!(batch.index, 1);
    assert!(batch
        .mutations
        .iter()
        .any(|m| m.cf == "meta" && m.key.starts_with(b"seq/node/")));
    assert!(batch.mutations.iter().any(|m| m.cf == "acl"));
    let receipt = replicated_apply::apply(&db, &batch, ApplyLimits::default()).unwrap();
    assert!(!receipt.already_applied);
    assert_eq!(db.stats().unwrap().nodes, 3);
    assert_eq!(db.stats().unwrap().edges, 1);
    assert_eq!(
        execute(
            &db,
            "MATCH (c:Customer {email:'new'}) RETURN c.entity_id",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![Out::Value(Value::Str("one".into()))]]
    );
    assert!(
        replicated_apply::apply(&db, &batch, ApplyLimits::default())
            .unwrap()
            .already_applied
    );
}
#[test]
fn readonly_drops_and_local_commit_or_stale_prepare_fail_closed() {
    let (_dir, _res, db) = setup();
    let before = db.commit_sequence();
    let mut read = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    read.run("RETURN 1", &BTreeMap::new()).unwrap();
    assert!(read.prepare_replication().unwrap().is_none());
    assert_eq!(db.commit_sequence(), before);
    let mut bad = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    bad.run("CREATE (c:Customer {entity_id:'bad'})", &BTreeMap::new())
        .unwrap();
    assert!(bad.commit().is_err());
    assert_eq!(db.stats().unwrap().nodes, 0);
    assert_eq!(db.commit_sequence(), before);
    let mut stale = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    stale
        .run("CREATE (c:Customer {entity_id:'stale'})", &BTreeMap::new())
        .unwrap();
    let mut winner = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    winner
        .run("CREATE (c:Customer {entity_id:'winner'})", &BTreeMap::new())
        .unwrap();
    let p = winner.prepare_replication().unwrap().unwrap();
    let batch = Batch::from_canonical_bytes(&p.batch, ApplyLimits::default()).unwrap();
    replicated_apply::apply(&db, &batch, ApplyLimits::default()).unwrap();
    assert!(matches!(
        stale.prepare_replication(),
        Err(ExecError::Conflict)
    ));
    assert_eq!(db.stats().unwrap().nodes, 1);
}
#[test]
fn competing_prepared_entries_reject_divergence_without_mutation() {
    let (_dir, _res, db) = setup();
    let mut a = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    let mut b = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    a.run("CREATE (c:Customer {entity_id:'a'})", &BTreeMap::new())
        .unwrap();
    b.run("CREATE (c:Customer {entity_id:'b'})", &BTreeMap::new())
        .unwrap();
    let pa = a.prepare_replication().unwrap().unwrap();
    let pb = b.prepare_replication().unwrap().unwrap();
    let ba = Batch::from_canonical_bytes(&pa.batch, ApplyLimits::default()).unwrap();
    let bb = Batch::from_canonical_bytes(&pb.batch, ApplyLimits::default()).unwrap();
    replicated_apply::apply(&db, &ba, ApplyLimits::default()).unwrap();
    assert!(matches!(
        replicated_apply::apply(&db, &bb, ApplyLimits::default()),
        Err(ApplyError::Divergent(1))
    ));
    assert_eq!(db.stats().unwrap().nodes, 1);
}
#[test]
fn preparation_buffer_guard_rejects_before_export_and_releases_shared_reservation() {
    use brainwire_cypher::{ExecutionLimits, QueryMemoryPool};
    let (_dir, _res, db) = setup();
    let before = db.commit_sequence();
    let pool = std::sync::Arc::new(QueryMemoryPool::new(128 * 1024));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 128 * 1024,
            timeout: None,
        },
        pool.clone(),
    )
    .unwrap()
    .with_replicated_ids();
    let ids = (0..100).map(|i| Value::Str(format!("guard-{i}"))).collect();
    tx.run(
        "UNWIND $ids AS id CREATE (c:Customer {entity_id:id})",
        &BTreeMap::from([("ids".into(), Value::List(ids))]),
    )
    .unwrap();
    assert!(pool.usage().reserved_bytes > 0);
    assert!(matches!(
        tx.prepare_replication(),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert_eq!(db.commit_sequence(), before);
    assert_eq!(db.stats().unwrap().nodes, 0);
}
