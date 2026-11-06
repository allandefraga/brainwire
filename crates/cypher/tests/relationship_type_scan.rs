use std::collections::BTreeMap;

use brainwire_core::{acl::ReadVisibility, schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryAccess, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
}

fn count(value: i64) -> Vec<Vec<Out>> {
    vec![vec![Out::Value(Value::Int(value))]]
}

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version": "1",
        "nodes": [{"label":"A"}, {"label":"B"}, {"label":"C"}, {"label":"Other"}],
        "edges": [
            {"type":"EVENT", "from":{"label":"A"}, "to":{"label":"B"}, "properties":["score"]},
            {"type":"EVENT", "from":{"label":"C"}, "to":{"label":"B"}, "properties":["score"]},
            {"type":"EVENT", "from":{"label":"A"}, "to":{"label":"A"}, "properties":["score"]},
            {"type":"OTHER", "from":{"label":"A"}, "to":{"label":"B"}},
            {"type":"OTHER", "from":{"label":"Other"}, "to":{"label":"Other"}}
        ]
    }))
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (:A {entity_id:'a'}), (:B {entity_id:'b'}), (:C {entity_id:'c'}), (:Other {entity_id:'x'})");
    run(&db, "MATCH (a:A), (b:B), (c:C), (x:Other) CREATE (a)-[:EVENT {score:3}]->(b), (c)-[:EVENT {score:1}]->(b), (a)-[:EVENT {score:2}]->(a), (a)-[:OTHER]->(b), (x)-[:OTHER]->(x)");
    (dir, db)
}

#[test]
fn directed_scan_preserves_signatures_filters_order_and_count() {
    let (_dir, db) = setup();
    for (query, expected) in [
        ("MATCH ()-[r:EVENT]->() RETURN count(r)", 3),
        ("MATCH ()<-[r:EVENT]-() RETURN count(r)", 3),
        ("MATCH ()-[r:EVENT]-() RETURN count(r)", 5),
        (
            "MATCH ()-[r:EVENT]->() WHERE r.score > 1 RETURN count(r)",
            2,
        ),
        ("MATCH ()-[r:EVENT {score:2}]->() RETURN count(r)", 1),
        ("MATCH (a)-[:EVENT]->() RETURN count(DISTINCT a)", 2),
        ("MATCH ()-[r:EVENT|OTHER]->() RETURN count(r)", 5),
        ("MATCH ()-[r:ABSENT]->() RETURN count(r)", 0),
        ("MATCH ()-[r:EVENT*1..1]->() RETURN count(r)", 3),
        ("MATCH ()-[r:EVENT]->(b:B) RETURN count(r)", 2),
        (
            "MATCH (a)-[r:EVENT]->() WHERE a.entity_id = 'c' RETURN count(r)",
            1,
        ),
        ("MATCH (a)-[r:EVENT]->(a) RETURN count(r)", 1),
    ] {
        assert_eq!(run(&db, query), count(expected), "{query}");
    }
    assert_eq!(
        run(
            &db,
            "MATCH (a)-[r:EVENT]->() RETURN a.entity_id, r.score ORDER BY r.score DESC LIMIT 2"
        ),
        vec![
            vec![
                Out::Value(Value::Str("a".into())),
                Out::Value(Value::Int(3))
            ],
            vec![
                Out::Value(Value::Str("a".into())),
                Out::Value(Value::Int(2))
            ],
        ],
    );
}

#[test]
fn scan_reads_pending_edge_changes_and_respects_endpoint_acl() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    let params = BTreeMap::new();
    tx.run("MATCH (c:C)-[r:EVENT]->(:B) DELETE r", &params)
        .unwrap();
    tx.run("CREATE (:A {entity_id:'a2'})", &params).unwrap();
    tx.run("MATCH (a:A {entity_id:'a2'}), (b:B), (old:A {entity_id:'a'}) CREATE (a)-[:EVENT {score:4}]->(b), (a)-[:EVENT {score:5}]->(old)", &params).unwrap();
    assert_eq!(
        tx.run("MATCH ()-[r:EVENT]->() RETURN count(r)", &params)
            .unwrap()
            .rows,
        count(4)
    );
    assert_eq!(run(&db, "MATCH ()-[r:EVENT]->() RETURN count(r)"), count(3));
    tx.commit().unwrap();
    assert_eq!(run(&db, "MATCH ()-[r:EVENT]->() RETURN count(r)"), count(4));

    for (label, id, principal) in [
        ("A", "a", "alice"),
        ("B", "b", "bob"),
        ("C", "c", "bob"),
        ("A", "a2", "bob"),
    ] {
        run(&db, &format!("CALL db.setNodeAcl({{label:'{label}',entityId:'{id}',allow:['{principal}']}}) YIELD updated RETURN updated"));
    }
    let mut read = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    for query in [
        "MATCH ()-[r:EVENT]->() RETURN count(r)",
        "MATCH ()<-[r:EVENT]-() RETURN count(r)",
        "MATCH ()-[r:EVENT]-() RETURN count(r)",
    ] {
        assert_eq!(
            read.run_with_access(
                query,
                &params,
                QueryAccess {
                    write: false,
                    instance_admin: false
                }
            )
            .unwrap()
            .rows,
            count(1),
            "{query}"
        );
    }
}

#[test]
fn optional_typed_relationship_scan_keeps_a_null_row_when_no_edge_is_visible() {
    let (_dir, db) = setup();
    let expected = vec![vec![Out::Value(Value::Int(1)), Out::Value(Value::Int(0))]];
    assert_eq!(
        run(
            &db,
            "OPTIONAL MATCH ()-[r:EVENT]->() WHERE r.score > 10 RETURN count(*), count(r)"
        ),
        expected
    );
    assert_eq!(
        run(
            &db,
            "OPTIONAL MATCH ()-[r:ABSENT]->() RETURN count(*), count(r)"
        ),
        expected
    );

    let mut read = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            deny_all: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.run_with_access(
            "OPTIONAL MATCH ()-[r:EVENT]->() RETURN count(*), count(r)",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
        )
        .unwrap()
        .rows,
        expected
    );
}

#[test]
fn typed_relationship_scan_preserves_snapshot_after_external_commit() {
    let (_dir, db) = setup();
    let params = BTreeMap::new();
    let query = "MATCH ()-[r:EVENT]->() RETURN count(r)";
    let mut reader = QueryTransaction::begin(&db).unwrap();
    assert_eq!(reader.run(query, &params).unwrap().rows, count(3));

    run(&db, "MATCH (c:C)-[r:EVENT]->(:B) DELETE r");

    assert_eq!(run(&db, query), count(2));
    assert_eq!(reader.run(query, &params).unwrap().rows, count(3));
    reader.commit().unwrap();
}
