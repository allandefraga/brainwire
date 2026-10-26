use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

fn rows(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}

fn b(value: Option<bool>) -> Out {
    Out::Value(value.map(Value::Bool).unwrap_or(Value::Null))
}

#[test]
fn is_null_is_two_valued_on_values_properties_and_parameters() {
    let (_dir, db) = setup();
    assert_eq!(
        rows(
            &db,
            "RETURN null IS NULL, 1 IS NULL, null IS NOT NULL, 'x' IS NOT NULL"
        ),
        vec![vec![
            b(Some(true)),
            b(Some(false)),
            b(Some(false)),
            b(Some(true))
        ]]
    );
    rows(
        &db,
        "CREATE (:Customer {entity_id: 'a', name: 'Ana'}), (:Customer {entity_id: 'b'})",
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) WHERE c.name IS NULL RETURN c.entity_id"
        ),
        vec![vec![Out::Value(Value::Str("b".into()))]]
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) WHERE NOT c.name IS NULL AND c IS NOT NULL RETURN c.entity_id"
        ),
        vec![vec![Out::Value(Value::Str("a".into()))]]
    );
    assert!(execute(&db, "RETURN 1 IS 2", &BTreeMap::new()).is_err());
}

#[test]
fn in_follows_cypher_three_valued_membership() {
    let (_dir, db) = setup();
    assert_eq!(
        rows(&db, "RETURN 1 IN [1, 2], 3 IN [1, 2], 3 IN [1, null], null IN [], null IN [1], 1 IN null, 1.0 IN [1]"),
        vec![vec![
            b(Some(true)),
            b(Some(false)),
            b(None),
            b(Some(false)),
            b(None),
            b(None),
            b(Some(true)),
        ]]
    );
    assert!(matches!(
        execute(&db, "RETURN 1 IN 1", &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    rows(&db, "CREATE (:Customer {entity_id: 'a', segment: 's1'}), (:Customer {entity_id: 'b', segment: 's2'})");
    let params = BTreeMap::from([("keep".into(), Value::List(vec![Value::Str("a".into())]))]);
    let out = execute(
        &db,
        "MATCH (c:Customer) WHERE NOT c.entity_id IN $keep RETURN c.entity_id",
        &params,
    )
    .unwrap()
    .rows;
    assert_eq!(out, vec![vec![Out::Value(Value::Str("b".into()))]]);
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) WHERE 's1' IN [c.segment] RETURN count(*)"
        ),
        vec![vec![Out::Value(Value::Int(1))]]
    );
}
