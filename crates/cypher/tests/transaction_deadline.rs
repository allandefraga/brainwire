use brainwire_core::schema::GraphMapping;
use brainwire_cypher::{ExecError, QueryTransaction, QueryTransactionState};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[test]
fn expired_transaction_cannot_commit_staged_writes() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    let mut transaction = QueryTransaction::begin(&db)
        .unwrap()
        .with_deadline(Instant::now() + Duration::from_millis(100));
    transaction
        .run(
            "CREATE (:Customer {entity_id:'deadline-staged'})",
            &BTreeMap::new(),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(matches!(transaction.commit(), Err(ExecError::TimedOut)));
    assert_eq!(transaction.state(), QueryTransactionState::Failed);
    assert_eq!(db.stats().unwrap().nodes, 0);
}

#[test]
fn expired_check_never_changes_an_already_successful_durable_commit() {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    let mut transaction = QueryTransaction::begin(&db)
        .unwrap()
        .with_deadline(Instant::now() + Duration::from_millis(100));
    transaction
        .run(
            "CREATE (:Customer {entity_id:'deadline-committed'})",
            &BTreeMap::new(),
        )
        .unwrap();
    assert!(transaction.commit().unwrap().is_some());
    std::thread::sleep(Duration::from_millis(150));
    assert!(matches!(
        transaction.check_transaction_deadline(),
        Err(ExecError::Semantic(_))
    ));
    assert_eq!(transaction.state(), QueryTransactionState::Committed);
    assert_eq!(db.stats().unwrap().nodes, 1);
}
