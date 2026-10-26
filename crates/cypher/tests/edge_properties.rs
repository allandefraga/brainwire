use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer"}],
            "edges":[{"type":"FOLLOWS","from":{"label":"Customer"},"to":{"label":"Customer"},
                      "properties":["weight","note"]}]}"#,
    )
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(
        &db,
        "CREATE (:Customer {entity_id:'a'}), (:Customer {entity_id:'b'}), (:Customer {entity_id:'c'})",
    );
    run(&db, "MATCH (a:Customer {entity_id:'a'}), (b:Customer {entity_id:'b'}) MERGE (a)-[:FOLLOWS {weight: 0.5, note: 'x'}]->(b)");
    run(&db, "MATCH (a:Customer {entity_id:'a'}), (c:Customer {entity_id:'c'}) MERGE (a)-[:FOLLOWS {weight: 0.9}]->(c)");
    (dir, db)
}

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}

fn s(text: &str) -> Out {
    Out::Value(Value::Str(text.into()))
}

fn f(number: f64) -> Out {
    Out::Value(Value::Float(number))
}

fn sorted(mut rows: Vec<Vec<Out>>) -> Vec<Vec<Out>> {
    rows.sort_by_key(|row| format!("{row:?}"));
    rows
}

#[test]
fn relationship_properties_can_be_returned_and_filtered_in_both_directions() {
    let (_dir, db) = setup();
    assert_eq!(
        sorted(run(
            &db,
            "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) RETURN b.entity_id, r.weight, r.note"
        )),
        vec![
            vec![s("b"), f(0.5), s("x")],
            vec![s("c"), f(0.9), Out::Value(Value::Null)],
        ]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (b:Customer {entity_id:'c'})<-[r:FOLLOWS]-(a:Customer) RETURN a.entity_id, r.weight"
        ),
        vec![vec![s("a"), f(0.9)]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) WHERE r.weight > 0.6 RETURN b.entity_id"
        ),
        vec![vec![s("c")]]
    );
    assert_eq!(
        run(
            &db,
            "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) RETURN r.undeclared LIMIT 1"
        ),
        vec![vec![Out::Value(Value::Null)]]
    );
}

#[test]
fn relationship_property_reads_see_staged_changes_and_writes_stay_unsupported() {
    let (_dir, db) = setup();
    let params = BTreeMap::new();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run("MATCH (a:Customer {entity_id:'a'}), (b:Customer {entity_id:'b'}) MERGE (a)-[:FOLLOWS {weight: 0.1}]->(b)", &params).unwrap();
    assert_eq!(
        tx.run(
            "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer {entity_id:'b'}) RETURN r.weight, r.note",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![f(0.1), s("x")]]
    );
    tx.rollback();
    for query in [
        "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) RETURN r",
        "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) SET r.weight = 2",
    ] {
        assert!(execute(&db, query, &params).is_err(), "{query}");
    }
}

#[test]
fn large_relationship_property_reads_are_charged_to_the_query_guard() {
    let (_dir, db) = setup();
    let big = "x".repeat(1 << 20);
    let params = BTreeMap::from([("note".to_string(), Value::Str(big))]);
    execute(&db, "MATCH (a:Customer {entity_id:'a'}), (b:Customer {entity_id:'b'}) MERGE (a)-[:FOLLOWS {note: $note}]->(b)", &params).unwrap();
    let limited = brainwire_cypher::execute_with_limits(
        &db,
        "MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer {entity_id:'b'}) RETURN size(r.note)",
        &BTreeMap::new(),
        brainwire_cypher::ExecutionLimits {
            memory_bytes: 64 * 1024,
            timeout: None,
        },
    );
    assert!(
        matches!(
            limited,
            Err(brainwire_cypher::ExecError::MemoryLimit { .. })
        ),
        "{limited:?}"
    );
}
