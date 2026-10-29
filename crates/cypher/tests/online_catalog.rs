//! Catalog changes through Cypher while the database is in use.
use std::collections::BTreeMap;

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryAccess, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, find_by_indexed_property},
};

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .rows
}

fn fails(db: &Database, query: &str) -> String {
    execute(db, query, &BTreeMap::new())
        .expect_err(query)
        .to_string()
}

fn text(value: &str) -> Out {
    Out::Value(Value::Str(value.into()))
}

fn int(value: i64) -> Out {
    Out::Value(Value::Int(value))
}

fn index_rows(db: &Database) -> Vec<Vec<Out>> {
    run(
        db,
        "CALL db.indexes() YIELD label, property, state, name \
         WHERE label = 'Customer' AND property = 'name' RETURN state, name",
    )
}

/// Whether the store would answer `Customer.name = value` from the index.
fn served_by_index(db: &Database) -> bool {
    let catalog = db.catalog().unwrap().unwrap();
    let ty = catalog.node_type("Customer").unwrap();
    find_by_indexed_property(&db.begin(), ty, "name", &Value::Str("n1".into()))
        .unwrap()
        .is_some()
}

const BY_NAME: &str = "MATCH (c:Customer) WHERE c.name IN ['n1', 'n2', 'late'] \
                       RETURN c.entity_id ORDER BY c.entity_id";

#[test]
fn index_created_over_existing_data_populates_then_serves_identical_results() {
    let (_dir, db) = setup();
    run(
        &db,
        "UNWIND range(0, 49) AS i CREATE (:Customer {entity_id: 'c' + toString(i), name: 'n' + toString(i % 5)})",
    );
    let before = run(&db, BY_NAME);
    assert_eq!(before.len(), 20);

    run(&db, "CREATE INDEX FOR (c:Customer) ON (c.name)");
    assert_eq!(
        index_rows(&db),
        vec![vec![text("populating"), text("index_Customer_name")]]
    );
    assert!(!served_by_index(&db));
    assert_eq!(run(&db, BY_NAME), before);
    check_invariants(&db).unwrap();

    // Writes during population: a new node, a changed value, a deletion.
    run(&db, "CREATE (:Customer {entity_id: 'z1', name: 'late'})");
    run(
        &db,
        "MATCH (c:Customer {entity_id: 'c1'}) SET c.name = 'moved'",
    );
    run(&db, "MATCH (c:Customer {entity_id: 'c2'}) DETACH DELETE c");
    let during = run(&db, BY_NAME);
    assert_eq!(during.len(), 19);

    db.build_pending_indexes().unwrap();
    assert_eq!(
        index_rows(&db),
        vec![vec![
            text("scalar_equality_ready"),
            text("index_Customer_name")
        ]]
    );
    assert!(served_by_index(&db));
    assert_eq!(run(&db, BY_NAME), during);
    check_invariants(&db).unwrap();
}

#[test]
fn a_writer_that_began_before_the_index_retries_instead_of_missing_it() {
    let (_dir, db) = setup();
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer
        .run(
            "CREATE (:Customer {entity_id: 'w', name: 'n1'})",
            &BTreeMap::new(),
        )
        .unwrap();
    run(&db, "CREATE INDEX by_name FOR (c:Customer) ON (c.name)");
    assert!(matches!(writer.commit(), Err(ExecError::Conflict)));
    run(&db, "CREATE (:Customer {entity_id: 'w', name: 'n1'})");
    db.build_pending_indexes().unwrap();
    assert!(served_by_index(&db));
    check_invariants(&db).unwrap();
}

#[test]
fn index_existence_rules_follow_if_not_exists_and_if_exists() {
    let (_dir, db) = setup();
    run(&db, "CREATE INDEX by_name FOR (c:Customer) ON (c.name)");
    assert!(fails(&db, "CREATE INDEX by_name FOR (c:Customer) ON (c.name)").contains("by_name"));
    assert!(fails(&db, "CREATE INDEX FOR (c:Customer) ON (c.name)").contains("already covers"));
    assert!(fails(&db, "CREATE INDEX ON :Customer(name)").contains("already covers"));
    run(
        &db,
        "CREATE INDEX by_name IF NOT EXISTS FOR (c:Customer) ON (c.name)",
    );
    run(
        &db,
        "CREATE INDEX other IF NOT EXISTS FOR (c:Customer) ON (c.name)",
    );
    assert!(fails(&db, "CREATE INDEX FOR (c:Nope) ON (c.name)").contains("Nope"));
    assert_eq!(
        index_rows(&db),
        vec![vec![text("populating"), text("by_name")]]
    );

    run(&db, "DROP INDEX by_name");
    assert!(index_rows(&db).is_empty());
    assert!(fails(&db, "DROP INDEX by_name").contains("no index named by_name"));
    run(&db, "DROP INDEX by_name IF EXISTS");
    // Ontology-declared indexes have default names and can be dropped too.
    run(&db, "DROP INDEX ON :Customer(email)");
    run(&db, "DROP INDEX index_Customer_segment");
    assert!(fails(&db, "DROP INDEX ON :Customer(email)").contains("no index"));
    db.build_pending_indexes().unwrap();
    let remaining = run(
        &db,
        "CALL db.indexes() YIELD label, property WHERE label = 'Customer' RETURN property",
    );
    assert_eq!(remaining, vec![vec![text("phone")]]);
    assert_eq!(
        run(&db, "MATCH (c:Customer {email: 'x'}) RETURN count(*)"),
        vec![vec![int(0)]]
    );
    check_invariants(&db).unwrap();
}

#[test]
fn dropped_index_stops_serving_at_once_and_its_keys_go_away() {
    let (_dir, db) = setup();
    run(&db, "CREATE INDEX FOR (c:Customer) ON (c.name)");
    run(
        &db,
        "UNWIND range(0, 9) AS i CREATE (:Customer {entity_id: 'c' + toString(i), name: 'n' + toString(i)})",
    );
    db.build_pending_indexes().unwrap();
    assert!(served_by_index(&db));
    run(&db, "DROP INDEX index_Customer_name");
    assert!(!served_by_index(&db));
    assert_eq!(
        run(&db, "MATCH (c:Customer {name: 'n1'}) RETURN c.entity_id"),
        vec![vec![text("c1")]]
    );
    assert!(fails(&db, "CREATE INDEX FOR (c:Customer) ON (c.name)").contains("still being removed"));
    db.build_pending_indexes().unwrap();
    run(&db, "CREATE INDEX FOR (c:Customer) ON (c.name)");
    db.build_pending_indexes().unwrap();
    assert!(served_by_index(&db));
    check_invariants(&db).unwrap();
}

#[test]
fn labels_and_relationship_types_created_online_are_usable_at_once() {
    let (_dir, db) = setup();
    assert!(fails(&db, "CREATE (:Ticket {entity_id: 't1'})").contains("Ticket"));
    run(&db, "CALL db.createLabel('Ticket')");
    run(&db, "CALL db.createLabel('Ticket')");
    run(&db, "CREATE (:Ticket {entity_id: 't1', title: 'printer'})");
    run(&db, "CREATE (:Customer {entity_id: 'c1'})");
    assert!(fails(
        &db,
        "MATCH (t:Ticket), (c:Customer) MERGE (t)-[:OPENED_BY]->(c)"
    )
    .contains("OPENED_BY"));
    run(
        &db,
        "CALL db.createRelationshipType('OPENED_BY', 'Ticket', 'Customer')",
    );
    run(
        &db,
        "MATCH (t:Ticket), (c:Customer) MERGE (t)-[:OPENED_BY]->(c)",
    );
    // A second signature for an existing type.
    run(
        &db,
        "CALL db.createRelationshipType('IN_SEGMENT', 'Ticket', 'Segment')",
    );
    run(&db, "CREATE (:Segment {entity_id: 's1'})");
    run(
        &db,
        "MATCH (t:Ticket), (s:Segment) MERGE (t)-[:IN_SEGMENT]->(s)",
    );
    assert_eq!(
        run(
            &db,
            "MATCH (t:Ticket)-[r]->(n) RETURN type(r), n.entity_id ORDER BY type(r)"
        ),
        vec![
            vec![text("IN_SEGMENT"), text("s1")],
            vec![text("OPENED_BY"), text("c1")],
        ]
    );
    assert!(fails(&db, "CALL db.createRelationshipType('X', 'Ticket', 'Nope')").contains("Nope"));
    // An index on the new label, created in the same transaction as data.
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run("CALL db.createLabel('Agent')", &BTreeMap::new())
        .unwrap();
    tx.run(
        "CREATE (:Agent {entity_id: 'a1', team: 'x'})",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.run("CREATE INDEX FOR (a:Agent) ON (a.team)", &BTreeMap::new())
        .unwrap();
    tx.run(
        "CREATE (:Agent {entity_id: 'a2', team: 'x'})",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.commit().unwrap();
    db.build_pending_indexes().unwrap();
    assert_eq!(
        run(&db, "MATCH (a:Agent {team: 'x'}) RETURN count(*)"),
        vec![vec![int(2)]]
    );
    check_invariants(&db).unwrap();
}

#[test]
fn dropped_property_is_hidden_and_later_writes_are_undeclared_extras() {
    let (_dir, db) = setup();
    run(
        &db,
        "CREATE (:Customer {entity_id: 'c1', cpf: 'secret', name: 'Ana'})",
    );
    assert!(fails(&db, "CALL db.dropProperty('Customer', 'email')").contains("drop index"));
    assert!(fails(&db, "CALL db.dropProperty('Customer', 'nope')").contains("no declared"));
    run(&db, "CALL db.dropProperty('Customer', 'cpf')");
    assert_eq!(
        run(&db, "MATCH (c:Customer) RETURN c.cpf, keys(c)"),
        vec![vec![
            Out::Value(Value::Null),
            Out::Value(Value::List(vec![Value::Str("name".into())])),
        ]]
    );
    let schema = run(
        &db,
        "CALL db.schema() YIELD nodes UNWIND nodes AS n WITH n WHERE n.label = 'Customer' RETURN 'cpf' IN n.properties",
    );
    assert_eq!(schema, vec![vec![Out::Value(Value::Bool(false))]]);
    run(&db, "MATCH (c:Customer) SET c.cpf = 'new'");
    assert_eq!(
        run(&db, "MATCH (c:Customer {cpf: 'new'}) RETURN c.cpf"),
        vec![vec![text("new")]]
    );
    check_invariants(&db).unwrap();
}

#[test]
fn catalog_changes_need_instance_admin_and_a_standalone_transaction() {
    let (_dir, db) = setup();
    let writer = QueryAccess {
        write: true,
        instance_admin: false,
    };
    for query in [
        "CREATE INDEX FOR (c:Customer) ON (c.name)",
        "DROP INDEX index_Customer_email",
        "CALL db.createLabel('X')",
        "CALL db.createRelationshipType('R', 'Customer', 'Customer')",
        "CALL db.dropProperty('Customer', 'cpf')",
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        let denied = tx.run_with_access(query, &BTreeMap::new(), writer);
        assert!(matches!(denied, Err(ExecError::Forbidden(_))), "{query}");
        let mut tx = QueryTransaction::begin(&db).unwrap().with_replicated_ids();
        let refused = tx.run(query, &BTreeMap::new());
        assert!(matches!(refused, Err(ExecError::Forbidden(_))), "{query}");
    }
    assert!(db.catalog().unwrap().unwrap().node_type("X").is_none());
}
