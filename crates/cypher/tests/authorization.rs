use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, ExecError, ExecutionLimits, Out, QueryAccess, QueryMemoryPool, QueryTransaction,
    QueryTransactionState,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    txn::Cf,
};
use std::{collections::BTreeMap, sync::Arc};

const READ: QueryAccess = QueryAccess {
    write: false,
    instance_admin: false,
};
const WRITE: QueryAccess = QueryAccess {
    write: true,
    instance_admin: false,
};
const CREATE: &str = "CREATE (c:Customer {entity_id: 'one'})";

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

fn assert_empty(db: &Database) {
    assert_eq!(
        execute(db, "MATCH (c:Customer) RETURN count(*)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn reader_can_return_match_unwind_and_graph_scoped_procedures() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert_eq!(
        tx.run_with_access("UNWIND [7] AS x RETURN x", &BTreeMap::new(), READ)
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(7))]]
    );
    for query in [
        "MATCH (c:Customer) RETURN c",
        "CALL db.schema() YIELD version RETURN version",
        "CALL db.indexes() YIELD label RETURN label",
        "CALL db.commitState() YIELD sequence RETURN sequence",
        "CALL graph.project() YIELD nodeCount RETURN nodeCount",
        "CALL graph.degree() YIELD node RETURN node",
        "CALL graph.pagerank() YIELD node RETURN node",
        "CALL knowledge.rank({label: 'Customer', term: 'synthetic'}) YIELD node RETURN node",
    ] {
        tx.run_with_access(query, &BTreeMap::new(), READ)
            .unwrap_or_else(|error| panic!("{query}: {error}"));
    }
    assert_eq!(tx.commit().unwrap(), None);
}

#[test]
fn reader_denials_precede_execution_and_allocator_reservations() {
    let (_dir, db) = setup();
    for query in [
        CREATE,
        "UNWIND ['one'] AS id MERGE (c:Customer {entity_id: id}) RETURN c",
        "MERGE (c:Customer {entity_id: 'one'})-[:WORKS_AT]->(k:Company {entity_id: 'two'})",
        "MATCH (c:Customer) SET c.name = 'changed'",
        "MATCH (c:Customer) DETACH DELETE c",
        "UNWIND [1] AS x CREATE (c:Customer {entity_id: 'one'}) RETURN x",
        "RETURN $missing CREATE (c:Customer {entity_id: 'one'})",
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        assert!(
            matches!(
                tx.run_with_access(query, &BTreeMap::new(), READ),
                Err(ExecError::Forbidden(_))
            ),
            "{query}"
        );
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        assert!(tx.commit().is_err());
        assert_empty(&db);
        assert!(db
            .begin()
            .scan_prefix(Cf::Meta, b"seq/")
            .unwrap()
            .is_empty());
    }
}

#[test]
fn writer_can_commit_and_instance_admin_can_read_memory() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run_with_access(CREATE, &BTreeMap::new(), WRITE).unwrap();
    assert!(tx.commit().unwrap().is_some());
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run_with_access(
        "CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes",
        &BTreeMap::new(),
        QueryAccess {
            write: false,
            instance_admin: true,
        },
    )
    .unwrap();
}

#[test]
fn memory_and_unknown_procedures_fail_closed_before_earlier_writes() {
    let (_dir, db) = setup();
    for access in [READ, WRITE, QueryAccess::UNRESTRICTED] {
        for query in [
            "CREATE (c:Customer {entity_id: 'one'}) CALL unknown.secret() YIELD secret RETURN secret",
            "CALL db.MEMORY() YIELD poolLimitBytes RETURN poolLimitBytes",
        ] {
            let mut tx = QueryTransaction::begin(&db).unwrap();
            let error = tx.run_with_access(query, &BTreeMap::new(), access).unwrap_err();
            assert!(matches!(error, ExecError::Forbidden(_)));
            assert!(!error.to_string().contains("secret"));
            assert_eq!(tx.state(), QueryTransactionState::Failed);
            assert_empty(&db);
            assert!(db.begin().scan_prefix(Cf::Meta, b"seq/").unwrap().is_empty());
        }
    }
    for access in [READ, WRITE] {
        for query in [
            "CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes",
            "CREATE (c:Customer {entity_id: 'one'}) CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes",
        ] {
            let mut tx = QueryTransaction::begin(&db).unwrap();
            assert!(matches!(
                tx.run_with_access(query, &BTreeMap::new(), access),
                Err(ExecError::Forbidden(_))
            ));
            assert_empty(&db);
            assert!(db.begin().scan_prefix(Cf::Meta, b"seq/").unwrap().is_empty());
        }
    }
}

#[test]
fn denial_discards_prior_runs_and_releases_shared_reservation() {
    let (_dir, db) = setup();
    let pool = Arc::new(QueryMemoryPool::new(1 << 20));
    let mut tx =
        QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
            .unwrap();
    tx.run_with_access(CREATE, &BTreeMap::new(), WRITE).unwrap();
    assert!(pool.usage().reserved_bytes > 0);
    assert!(matches!(
        tx.run_with_access(
            "CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes",
            &BTreeMap::new(),
            WRITE
        ),
        Err(ExecError::Forbidden(_))
    ));
    assert_eq!(tx.state(), QueryTransactionState::Failed);
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(tx.commit().is_err());
    assert_empty(&db);
}

#[test]
fn source_length_guard_still_precedes_authorization() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 1,
            timeout: None,
        },
    )
    .unwrap();
    assert!(matches!(
        tx.run_with_access(CREATE, &BTreeMap::new(), READ),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(tx.state(), QueryTransactionState::Failed);
}
