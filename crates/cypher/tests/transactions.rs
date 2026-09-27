use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryTransaction, QueryTransactionState};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

fn mapping() -> GraphMapping {
    serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap()
}
fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.apply_ontology(&mapping()).unwrap();
    (dir, db)
}
fn count(db: &Database) -> Vec<Vec<Out>> {
    execute(db, "MATCH (c:Customer) RETURN count(*)", &BTreeMap::new())
        .unwrap()
        .rows
}
fn n(value: i64) -> Vec<Vec<Out>> {
    vec![vec![Out::Value(Value::Int(value))]]
}
fn create(tx: &mut QueryTransaction<'_>, id: &str) {
    tx.run(
        "CREATE (c:Customer {entity_id: $id})",
        &BTreeMap::from([("id".into(), Value::Str(id.into()))]),
    )
    .unwrap();
}
fn rejects(tx: &mut QueryTransaction<'_>) {
    assert!(matches!(
        tx.run("RETURN 1", &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    assert!(matches!(tx.commit(), Err(ExecError::Semantic(_))));
}

#[test]
fn shared_overlay_is_private_then_persists_on_commit_and_reopen() {
    let (dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert_eq!(tx.state(), QueryTransactionState::Active);
    create(&mut tx, "one");
    create(&mut tx, "two");
    assert_eq!(
        tx.run("MATCH (c:Customer) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        n(2)
    );
    assert_eq!(count(&db), n(0));
    assert!(tx.commit().unwrap().is_some());
    assert_eq!(tx.state(), QueryTransactionState::Committed);
    rejects(&mut tx);
    tx.rollback();
    assert_eq!(tx.state(), QueryTransactionState::Committed);
    drop(tx);
    drop(db);
    assert_eq!(count(&open(dir.path())), n(2));
}

#[test]
fn rollback_and_drop_discard_every_run() {
    let (dir, db) = setup();
    {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        create(&mut tx, "one");
        create(&mut tx, "two");
        tx.rollback();
        tx.rollback();
        assert_eq!(tx.state(), QueryTransactionState::RolledBack);
        rejects(&mut tx);
    }
    {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        create(&mut tx, "three");
        create(&mut tx, "four");
    }
    drop(db);
    assert_eq!(count(&open(dir.path())), n(0));
}

#[test]
fn every_query_error_poisons_all_prior_work() {
    let (_dir, db) = setup();
    for query in [
        "RETURN (",
        "RETURN unknown",
        "CREATE (c:Customer {entity_id: 'late'}) UNWIND 42 AS x RETURN x",
        "RETURN $missing",
        "CREATE (c:Customer {entity_id: 'one'})",
        "CREATE (c:Customer)",
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        create(&mut tx, "one");
        create(&mut tx, "two");
        assert!(tx.run(query, &BTreeMap::new()).is_err());
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        rejects(&mut tx);
        assert_eq!(count(&db), n(0));
        tx.rollback();
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        rejects(&mut tx);
    }
}

#[test]
fn bindings_and_parameters_are_query_local() {
    let (_dir, db) = setup();
    for query in ["RETURN x", "RETURN $value"] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        tx.run(
            "UNWIND [$value] AS x RETURN x",
            &BTreeMap::from([("value".into(), Value::Int(7))]),
        )
        .unwrap();
        assert!(tx.run(query, &BTreeMap::new()).is_err());
    }
    let mut tx = QueryTransaction::begin(&db).unwrap();
    for value in [1, 2] {
        assert_eq!(
            tx.run(
                "RETURN $value",
                &BTreeMap::from([("value".into(), Value::Int(value))])
            )
            .unwrap()
            .rows,
            n(value)
        );
    }
    assert_eq!(tx.commit().unwrap(), None);
}

#[test]
fn snapshot_hides_external_updates_and_read_only_completion_succeeds() {
    let (_dir, db) = setup();
    execute(
        &db,
        "CREATE (c:Customer {entity_id: 'one', number: 1})",
        &BTreeMap::new(),
    )
    .unwrap();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    let query = "MATCH (c:Customer) RETURN c.number";
    assert_eq!(tx.run(query, &BTreeMap::new()).unwrap().rows, n(1));
    execute(&db, "MATCH (c:Customer) SET c.number = 2", &BTreeMap::new()).unwrap();
    db.apply_ontology(&mapping()).unwrap();
    assert_eq!(tx.run(query, &BTreeMap::new()).unwrap().rows, n(1));
    assert_eq!(tx.commit().unwrap(), None);
    rejects(&mut tx);
}

#[test]
fn conflicting_writer_fails_permanently() {
    let (_dir, db) = setup();
    let mut first = QueryTransaction::begin(&db).unwrap();
    let mut second = QueryTransaction::begin(&db).unwrap();
    create(&mut first, "same");
    create(&mut second, "same");
    first.commit().unwrap();
    assert!(matches!(second.commit(), Err(ExecError::Conflict)));
    assert_eq!(second.state(), QueryTransactionState::Failed);
    rejects(&mut second);
    assert_eq!(count(&db), n(1));
}

#[test]
fn catalog_migration_conflicts_with_writes() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    create(&mut tx, "one");
    let mut old_catalog = QueryTransaction::begin(&db).unwrap();
    let revised: GraphMapping = serde_json::from_value(serde_json::json!({"version":"2", "nodes":[{"label":"Customer"}, {"label":"NewLabel"}], "edges":[]})).unwrap();
    db.apply_ontology(&revised).unwrap();
    create(&mut tx, "two");
    assert!(matches!(
        old_catalog.run("MATCH (n:NewLabel) RETURN n", &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    assert!(matches!(tx.commit(), Err(ExecError::Conflict)));
    rejects(&mut tx);
    assert_eq!(count(&db), n(0));
    let mut fresh = QueryTransaction::begin(&db).unwrap();
    assert!(fresh
        .run("MATCH (n:NewLabel) RETURN n", &BTreeMap::new())
        .is_ok());
}

#[test]
fn owned_database_worker_safely_contains_borrowed_transaction() {
    let (_dir, db) = setup();
    let db = Arc::new(db);
    let worker_db = Arc::clone(&db);
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let mut tx = QueryTransaction::begin(&worker_db).unwrap();
                create(&mut tx, "one");
                create(&mut tx, "two");
                assert!(tx.commit().unwrap().is_some());
                drop(tx);
                drop(worker_db);
            })
            .join()
            .unwrap();
    });
    assert_eq!(count(&db), n(2));
}
