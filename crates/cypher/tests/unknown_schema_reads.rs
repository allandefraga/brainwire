//! Read patterns treat undeclared schema names as empty sets; writes remain strict.
use std::collections::BTreeMap;

use brainwire_core::{acl::ReadVisibility, schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryAccess, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::check_invariants,
};

fn setup() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let db = Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let schema: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Doc","properties":["title"]}],"edges":[{"type":"LINK","from":{"label":"Doc"},"to":{"label":"Doc"},"properties":[]}]}"#).unwrap();
    db.apply_ontology(&schema).unwrap();
    run(&db, "CREATE (a:Doc {entity_id:'a',title:'A'}),(b:Doc {entity_id:'b',title:'B'}) CREATE (a)-[:LINK]->(b)");
    (directory, db)
}
fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
}
fn integer(value: i64) -> Out {
    Out::Value(Value::Int(value))
}
fn text(value: &str) -> Out {
    Out::Value(Value::Str(value.into()))
}
fn null() -> Out {
    Out::Value(Value::Null)
}

#[test]
fn undeclared_read_labels_are_empty_for_scans_bound_nodes_and_optional_match() {
    let (_directory, db) = setup();
    for query in [
        "MATCH (n:Missing) RETURN n",
        "MATCH (n:Missing {entity_id:'a'}) RETURN n",
        "MATCH (n:Missing) WHERE id(n)=0 RETURN n",
        "MATCH (n:Doc {entity_id:'a'}) MATCH (n:Missing) RETURN n",
        "MATCH (a:Doc)-[:LINK]->(b:Missing) RETURN a,b",
        "MATCH (a:Missing)-[:LINK]->(b:Doc) RETURN a,b",
    ] {
        assert!(run(&db, query).is_empty(), "{query}");
    }
    assert_eq!(
        run(&db, "MATCH (n:Missing) RETURN count(n),count(*)"),
        vec![vec![integer(0), integer(0)]]
    );
    assert_eq!(
        run(&db, "WITH 7 AS x OPTIONAL MATCH (n:Missing) RETURN x,n"),
        vec![vec![integer(7), null()]]
    );
    assert_eq!(run(&db, "MATCH (a:Doc {entity_id:'a'}) OPTIONAL MATCH (a)-[r:LINK]->(b:Missing) RETURN a.entity_id,r,b"), vec![vec![text("a"),null(),null()]]);
}

#[test]
fn undeclared_relationship_types_do_not_become_wildcards() {
    let (_directory, db) = setup();
    for query in [
        "MATCH (a:Doc)-[r:MISSING]->(b:Doc) RETURN r",
        "MATCH (a:Doc {entity_id:'a'})-[r:MISSING]->(b:Doc) RETURN r",
        "MATCH (a:Doc)-[r:MISSING*1..2]->(b:Doc) RETURN r",
        "MATCH (a:Doc)-[r:MISSING]-(b:Doc) RETURN r",
        "MATCH (a:Doc)-[r:LINK]->(b:Doc) MATCH (a)-[r:MISSING]->(b) RETURN r",
    ] {
        assert!(run(&db, query).is_empty(), "{query}");
    }
    assert_eq!(
        run(
            &db,
            "MATCH (a:Doc)-[r:LINK|MISSING]->(b:Doc) RETURN a.entity_id,b.entity_id"
        ),
        vec![vec![text("a"), text("b")]]
    );
    assert_eq!(run(&db, "MATCH (a:Doc)-[r:MISSING*0..2]->(b:Doc) RETURN a.entity_id,b.entity_id,size(r) ORDER BY a.entity_id"), vec![vec![text("a"),text("a"),integer(0)],vec![text("b"),text("b"),integer(0)]]);
    assert_eq!(run(&db, "MATCH (a:Doc {entity_id:'a'}) OPTIONAL MATCH (a)-[r:MISSING]->(b) RETURN a.entity_id,r,b"), vec![vec![text("a"),null(),null()]]);
}

#[test]
fn no_match_still_validates_queries_and_write_schema_before_execution() {
    let (_directory, db) = setup();
    for query in [
        "CREATE (:Missing {entity_id:'x'})",
        "MERGE (:Missing {entity_id:'x'})",
        "MATCH (a:Doc),(b:Doc) CREATE (a)-[:MISSING]->(b)",
        "MATCH (a:Missing) CREATE (:Missing {entity_id:'x'})",
        "MATCH (n:Missing {title:unknown}) RETURN n",
        "MATCH (n:Missing {title:'x',title:'y'}) RETURN n",
        "MATCH (n:Missing) RETURN unknown",
        "MATCH (n:Missing) RETURN unsupportedFunction(n)",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
    }
    assert_eq!(
        run(&db, "MATCH (n:Doc) RETURN count(n)"),
        vec![vec![integer(2)]]
    );
    check_invariants(&db).unwrap();
}

#[test]
fn delegated_visibility_and_catalog_changes_keep_no_match_semantics() {
    let (_directory, db) = setup();
    let mut reader = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            deny_all: true,
            ..Default::default()
        })
        .unwrap();
    for query in [
        "MATCH (n:Missing) RETURN count(n)",
        "MATCH (n:Doc) RETURN count(n)",
        "MATCH (a:Doc)-[:MISSING*0..1]->(b:Doc) RETURN count(a)",
    ] {
        let result = reader
            .run_with_access(
                query,
                &BTreeMap::new(),
                QueryAccess {
                    write: false,
                    instance_admin: false,
                },
            )
            .unwrap();
        assert_eq!(result.rows, vec![vec![integer(0)]]);
    }
    reader.commit().unwrap();
    let mut before = QueryTransaction::begin(&db).unwrap();
    assert!(before
        .run("MATCH (n:Later) RETURN n", &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
    run(&db, "CALL db.createLabel('Later')");
    run(&db, "CREATE (:Later {entity_id:'later'})");
    assert!(before
        .run("MATCH (n:Later) RETURN n", &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
    before.commit().unwrap();
    assert_eq!(
        run(&db, "MATCH (n:Later) RETURN n.entity_id"),
        vec![vec![text("later")]]
    );
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert!(tx
        .run(
            "MATCH (a:Doc)-[:LATER_LINK]->(b:Doc) RETURN a",
            &BTreeMap::new()
        )
        .unwrap()
        .rows
        .is_empty());
    tx.run(
        "CALL db.createRelationshipType('LATER_LINK','Doc','Doc')",
        &BTreeMap::new(),
    )
    .unwrap();
    tx.run(
        "MATCH (a:Doc {entity_id:'a'}),(b:Doc {entity_id:'b'}) CREATE (a)-[:LATER_LINK]->(b)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        tx.run(
            "MATCH (a:Doc)-[:LATER_LINK]->(b:Doc) RETURN b.entity_id",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![text("b")]]
    );
    tx.commit().unwrap();
    check_invariants(&db).unwrap();
}

#[test]
fn pattern_predicates_and_bound_optional_reads_use_empty_type_sets() {
    let (_directory, db) = setup();
    assert_eq!(run(&db, "MATCH (a:Doc {entity_id:'a'}) RETURN exists((a)-[:MISSING]->()), exists((a)-[:LINK|MISSING]->())"), vec![vec![Out::Value(Value::Bool(false)),Out::Value(Value::Bool(true))]]);
    assert_eq!(run(&db, "MATCH (a:Doc {entity_id:'a'}) OPTIONAL MATCH (a:Missing)-[:LINK]->(b) RETURN a.entity_id,b"), vec![vec![text("a"),null()]]);
    assert_eq!(
        run(
            &db,
            "MATCH (a:Doc {entity_id:'a'}) RETURN exists((a)-[:MISSING*0..1]->())"
        ),
        vec![vec![Out::Value(Value::Bool(true))]]
    );
}
