use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryTransaction,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    txn::Cf,
};
use std::{collections::BTreeMap, path::Path, time::Duration};

const QUERY: &str =
    "CALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope";

fn open(path: &Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}

fn row(sequence: Option<u64>) -> Vec<Vec<Out>> {
    vec![vec![
        Out::Value(Value::Str(sequence.unwrap_or(0).to_string())),
        Out::Value(Value::Bool(sequence.is_some())),
        Out::Value(Value::Str("store_snapshot".into())),
    ]]
}

fn write(db: &Database) -> u64 {
    let mut tx = db.begin();
    tx.put(Cf::Meta, b"synthetic-commit-state".to_vec(), vec![1]);
    tx.commit().unwrap()
}

#[test]
fn one_row_supports_shared_projection_aliases_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let output = execute(&db, QUERY, &BTreeMap::new()).unwrap();
    assert_eq!(output.fields, ["sequence", "tracked", "scope"]);
    assert_eq!(output.rows, row(None));
    let output = execute(
        &db,
        "CALL db.commitState() YIELD sequence AS s, tracked AS t, scope AS kind WHERE t = false RETURN kind, s",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.fields, ["kind", "s"]);
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Str("store_snapshot".into())),
            Out::Value(Value::Str("0".into())),
        ]]
    );
    assert!(execute(
        &db,
        "CALL db.commitState() YIELD tracked WHERE tracked = true RETURN tracked",
        &BTreeMap::new(),
    )
    .unwrap()
    .rows
    .is_empty());
}

#[test]
fn rejects_arguments_options_unknown_fields_and_duplicate_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    for query in [
        "CALL db.commitState({}) YIELD sequence RETURN sequence",
        "CALL db.commitState({unknown: 1}) YIELD sequence RETURN sequence",
        "CALL db.commitState(1) YIELD sequence RETURN sequence",
        "CALL db.commitState(1, 2) YIELD sequence RETURN sequence",
        "CALL db.commitState() YIELD missing RETURN missing",
        "CALL db.commitState() YIELD sequence AS x, tracked AS x RETURN x",
    ] {
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{query}"
        );
    }
}

#[test]
fn explicit_snapshot_stays_stable_across_commits_and_internal_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let initial = write(&db);
    let mut old = QueryTransaction::begin(&db).unwrap();
    assert_eq!(
        old.run(QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(initial))
    );
    let newer = write(&db);
    assert!(newer > initial);
    assert_eq!(
        execute(&db, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(newer))
    );
    db.next_edge_id().unwrap();
    let after_allocation = execute(&db, QUERY, &BTreeMap::new()).unwrap().rows;
    assert_ne!(after_allocation, row(Some(newer)));
    assert_eq!(
        old.run(QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(initial))
    );
    assert_eq!(old.commit().unwrap(), None);
}

#[test]
fn pending_writes_rollback_reopen_and_next_commit_report_snapshot_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    assert_eq!(
        execute(&db, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(1))
    );
    let initial = write(&db);
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run(
        "CREATE (c:Customer {entity_id: 'synthetic'})",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        tx.run(QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(initial))
    );
    tx.rollback();
    drop(tx);
    // CREATE can durably reserve an ID block even though its graph writes roll back.
    let durable = execute(&db, QUERY, &BTreeMap::new()).unwrap().rows;
    drop(db);
    let db = open(dir.path());
    assert_eq!(execute(&db, QUERY, &BTreeMap::new()).unwrap().rows, durable);
    let next = write(&db);
    assert_eq!(
        execute(&db, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(next))
    );
    assert_ne!(row(Some(next)), durable);
    assert_eq!(
        execute(&db, "MATCH (c:Customer) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn independent_stores_report_their_own_decimal_sequences() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = open(first_dir.path());
    let second = open(second_dir.path());
    let mut sequence = 0;
    for _ in 0..128 {
        sequence = write(&first);
    }
    assert_eq!(
        execute(&first, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(sequence))
    );
    assert_eq!(
        execute(&second, QUERY, &BTreeMap::new()).unwrap().rows,
        row(None)
    );
    let mut untracked_snapshot = QueryTransaction::begin(&second).unwrap();
    let second_sequence = write(&second);
    assert_eq!(
        execute(&second, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(second_sequence))
    );
    assert_eq!(
        untracked_snapshot
            .run(QUERY, &BTreeMap::new())
            .unwrap()
            .rows,
        row(None)
    );
    assert_eq!(untracked_snapshot.commit().unwrap(), None);
    assert_eq!(
        execute(&first, QUERY, &BTreeMap::new()).unwrap().rows,
        row(Some(sequence))
    );
}

#[test]
fn obeys_shared_memory_and_deadline_limits() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    assert!(matches!(
        execute_with_limits(
            &db,
            QUERY,
            &BTreeMap::new(),
            ExecutionLimits {
                memory_bytes: QUERY.len(),
                timeout: None
            }
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(matches!(
        execute_with_limits(
            &db,
            QUERY,
            &BTreeMap::new(),
            ExecutionLimits {
                memory_bytes: 1 << 20,
                timeout: Some(Duration::ZERO)
            }
        ),
        Err(ExecError::TimedOut)
    ));
}
