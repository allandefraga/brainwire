use brainwire_core::{acl::ReadVisibility, schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryAccess, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

const ASSERT: &str = "CALL db.assertNodeState('Customer', 'checkpoint', $expected)";

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    execute(
        &db,
        "CREATE (:Customer {entity_id:'checkpoint', lease:'first'})",
        &BTreeMap::new(),
    )
    .unwrap();
    (dir, db)
}

fn expectation(token: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([(
        "expected".into(),
        Value::Map(BTreeMap::from([
            ("lease".into(), Value::Str(token.into())),
            ("snapshot".into(), Value::Null),
        ])),
    )])
}

fn assert_state(tx: &mut QueryTransaction<'_>, token: &str) {
    let output = tx.run(ASSERT, &expectation(token)).unwrap();
    assert_eq!(output.fields, ["matched"]);
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Bool(true))]]);
}

fn takeover(db: &Database) {
    execute(
        db,
        "MATCH (n:Customer {entity_id:'checkpoint'}) SET n.lease='next'",
        &BTreeMap::new(),
    )
    .unwrap();
}

#[test]
fn lease_takeover_blocks_a_stale_batch_and_an_assertion_only_commit() {
    let (_dir, db) = setup();
    let mut batch = QueryTransaction::begin(&db).unwrap();
    let mut guard_only = QueryTransaction::begin(&db).unwrap();
    assert_state(&mut batch, "first");
    assert_state(&mut guard_only, "first");
    batch
        .run("CREATE (:Customer {entity_id:'batch'})", &BTreeMap::new())
        .unwrap();
    takeover(&db);
    assert!(matches!(batch.commit(), Err(ExecError::Conflict)));
    assert!(matches!(guard_only.commit(), Err(ExecError::Conflict)));
    assert_eq!(
        execute(
            &db,
            "MATCH (n:Customer {entity_id:'batch'}) RETURN n",
            &BTreeMap::new()
        )
        .unwrap()
        .rows
        .len(),
        0
    );
}

#[test]
fn same_lease_disjoint_writes_commit_and_guard_only_does_not_allocate_sequence() {
    let (_dir, db) = setup();
    let mut first = QueryTransaction::begin(&db).unwrap();
    let mut second = QueryTransaction::begin(&db).unwrap();
    let mut guard_only = QueryTransaction::begin(&db).unwrap();
    for tx in [&mut first, &mut second, &mut guard_only] {
        assert_state(tx, "first");
    }
    first
        .run("CREATE (:Customer {entity_id:'a'})", &BTreeMap::new())
        .unwrap();
    second
        .run("CREATE (:Customer {entity_id:'b'})", &BTreeMap::new())
        .unwrap();
    let s1 = first.commit().unwrap().unwrap();
    assert_eq!(guard_only.commit().unwrap(), None);
    assert_eq!(second.commit().unwrap(), Some(s1 + 1));
}

#[test]
fn mismatch_rolls_back_prior_writes_and_reads_see_the_overlay() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run(
        "MATCH (n:Customer {entity_id:'checkpoint'}) SET n.lease='pending'",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_state(&mut tx, "pending");
    let mismatch = tx
        .run(ASSERT, &expectation("secret"))
        .unwrap_err()
        .to_string();
    assert_eq!(mismatch, "node state precondition failed");
    assert!(tx.commit().is_err());

    let missing = execute(
        &db,
        "CALL db.assertNodeState('Customer', 'missing', $expected)",
        &expectation("secret"),
    )
    .unwrap_err();
    assert_eq!(missing.to_string(), mismatch);
    let mut fresh = QueryTransaction::begin(&db).unwrap();
    assert_state(&mut fresh, "first");
}

#[test]
fn denied_modes_precede_argument_lookup() {
    let (_dir, db) = setup();
    let mut read = QueryTransaction::begin(&db).unwrap();
    assert!(matches!(
        read.run_with_access(
            ASSERT,
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: true
            }
        ),
        Err(ExecError::Forbidden(_))
    ));
    let mut restricted = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility::default())
        .unwrap();
    assert!(matches!(
        restricted.run(ASSERT, &BTreeMap::new()),
        Err(ExecError::Forbidden(_))
    ));
    let mut replicated = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
    assert!(matches!(
        replicated.run(ASSERT, &BTreeMap::new()),
        Err(ExecError::Forbidden(_))
    ));
    let mut identified = QueryTransaction::begin(&db)
        .unwrap()
        .with_identified_write()
        .unwrap();
    assert!(matches!(
        identified.run(ASSERT, &BTreeMap::new()),
        Err(ExecError::Forbidden(_))
    ));
}

#[test]
fn deleting_and_recreating_the_guarded_identity_conflicts_even_with_the_same_state() {
    let (_dir, db) = setup();
    let mut old = QueryTransaction::begin(&db).unwrap();
    assert_state(&mut old, "first");
    let mut replacement = QueryTransaction::begin(&db).unwrap();
    replacement
        .run(
            "MATCH (n:Customer {entity_id:'checkpoint'}) DELETE n",
            &BTreeMap::new(),
        )
        .unwrap();
    replacement
        .run(
            "CREATE (:Customer {entity_id:'checkpoint', lease:'first'})",
            &BTreeMap::new(),
        )
        .unwrap();
    replacement.commit().unwrap();
    assert!(matches!(old.commit(), Err(ExecError::Conflict)));
}

#[test]
fn changing_the_catalog_invalidates_an_assertion_only_transaction() {
    let (_dir, db) = setup();
    let mut guarded = QueryTransaction::begin(&db).unwrap();
    assert_state(&mut guarded, "first");
    execute(&db, "CALL db.createLabel('AnotherLabel')", &BTreeMap::new()).unwrap();
    assert!(matches!(guarded.commit(), Err(ExecError::Conflict)));
}

#[test]
fn invalid_expected_state_is_rejected_before_node_lookup() {
    let (_dir, db) = setup();
    for expected in [
        BTreeMap::new(),
        BTreeMap::from([("ACL_allow".into(), Value::Null)]),
        BTreeMap::from([("lease".into(), Value::List(vec![]))]),
        BTreeMap::from([("lease".into(), Value::Float(1.0))]),
        BTreeMap::from([("lease".into(), Value::Str("x".repeat(16 * 1024)))]),
        (0..17).map(|i| (format!("f{i}"), Value::Null)).collect(),
    ] {
        let parameters = BTreeMap::from([("expected".into(), Value::Map(expected))]);
        assert!(matches!(
            execute(
                &db,
                "CALL db.assertNodeState('MissingLabel', 'missing', $expected)",
                &parameters
            ),
            Err(ExecError::Semantic(_))
        ));
    }
    assert!(matches!(execute(&db, "CALL db.assertNodeState('Customer', 'checkpoint', $expected) YIELD matched RETURN matched", &expectation("first")), Err(ExecError::Semantic(_))));
}
