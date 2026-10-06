use brainwire_core::{
    ids::{EdgeId, NodeId},
    schema::GraphMapping,
    value::Value,
};
use brainwire_cypher::{execute, Out, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::check_invariants,
    txn::Cf,
};
use std::collections::BTreeMap;

fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Page","properties":["score"]}],"edges":[{"type":"WANTED","from":{"label":"Page"},"to":{"label":"Page"}},{"type":"NOISE","from":{"label":"Page"},"to":{"label":"Page"}},{"type":"EMPTY","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (h:Page {entity_id:'hub',score:0}), (a:Page {entity_id:'a',score:1}), (b:Page {entity_id:'b',score:2}), (n:Page {entity_id:'noise',score:3})");
    for (source, kind, target) in [
        ("hub", "WANTED", "hub"),
        ("hub", "WANTED", "a"),
        ("hub", "WANTED", "b"),
        ("a", "WANTED", "hub"),
        ("hub", "NOISE", "noise"),
        ("noise", "NOISE", "hub"),
        ("hub", "NOISE", "b"),
    ] {
        run(&db, &format!("MATCH (a:Page {{entity_id:'{source}'}}), (b:Page {{entity_id:'{target}'}}) MERGE (a)-[:{kind}]->(b)"));
    }
    (dir, db)
}
fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}
fn strings(values: &[&str]) -> Vec<Vec<Out>> {
    values
        .iter()
        .map(|s| vec![Out::Value(Value::Str((*s).into()))])
        .collect()
}
const OUT: &str = "MATCH (h:Page {entity_id:'hub'})-[:WANTED]->(p:Page)";
const IN: &str = "MATCH (h:Page {entity_id:'hub'})<-[:WANTED]-(p:Page)";

#[test]
fn directional_reads_and_eager_writes_select_the_same_neighbors() {
    let (_dir, db) = setup();
    for (pattern, expected) in [
        (OUT, strings(&["hub", "a", "b"])),
        (IN, strings(&["hub", "a"])),
    ] {
        assert_eq!(run(&db, &format!("{pattern} RETURN p.entity_id")), expected);
        assert_eq!(
            run(
                &db,
                &format!("{pattern} SET p.selected = true RETURN p.entity_id")
            ),
            expected
        );
    }
    assert_eq!(
        run(
            &db,
            &format!("{OUT} WHERE p.score >= 1 AND p.entity_id <> 'a' RETURN p.entity_id LIMIT 1")
        ),
        strings(&["b"])
    );
    assert_eq!(
        run(
            &db,
            "MATCH (h:Page {entity_id:'hub'})-[:WANTED]->(h:Page) RETURN h.entity_id"
        ),
        strings(&["hub"])
    );
    assert!(run(
        &db,
        "MATCH (h:Page {entity_id:'hub'})-[:EMPTY]->(p:Page) RETURN p.entity_id"
    )
    .is_empty());
    assert_eq!(
        run(&db, "MATCH (p:Page {entity_id:'noise'}) RETURN p.selected"),
        vec![vec![Out::Value(Value::Null)]]
    );
}

#[test]
fn pending_merges_and_detach_deletes_rollback_and_survive_reopen_on_commit() {
    let (dir, db) = setup();
    for commit in [false, true] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        tx.run(
            "CREATE (p:Page {entity_id:'pending',score:4})",
            &BTreeMap::new(),
        )
        .unwrap();
        tx.run("MATCH (h:Page {entity_id:'hub'}), (p:Page {entity_id:'pending'}) MERGE (h)-[:WANTED]->(p) MERGE (p)-[:WANTED]->(h)", &BTreeMap::new()).unwrap();
        tx.run(
            "MATCH (p:Page {entity_id:'b'}) DETACH DELETE p",
            &BTreeMap::new(),
        )
        .unwrap();
        for pattern in [OUT, IN] {
            let query = format!("{pattern} RETURN p.entity_id");
            assert_eq!(
                tx.run(&query, &BTreeMap::new()).unwrap().rows,
                strings(&["hub", "a", "pending"])
            );
            assert_eq!(
                tx.run(
                    &format!("{pattern} SET p.overlay = true RETURN p.entity_id"),
                    &BTreeMap::new()
                )
                .unwrap()
                .rows,
                strings(&["hub", "a", "pending"])
            );
        }
        // Detach must also remove the other relationship type, in the same overlay.
        assert_eq!(
            tx.run(
                "MATCH (h:Page {entity_id:'hub'})-[:NOISE]->(p:Page) RETURN p.entity_id",
                &BTreeMap::new()
            )
            .unwrap()
            .rows,
            strings(&["noise"])
        );
        assert_eq!(
            run(&db, &format!("{OUT} RETURN p.entity_id")),
            strings(&["hub", "a", "b"])
        );
        if commit {
            tx.commit().unwrap();
        } else {
            tx.rollback();
        }
        check_invariants(&db).unwrap();
    }
    drop(db);
    let reopened = open(dir.path());
    for pattern in [OUT, IN] {
        assert_eq!(
            run(&reopened, &format!("{pattern} RETURN p.entity_id")),
            strings(&["hub", "a", "pending"])
        );
    }
    check_invariants(&reopened).unwrap();
}

#[test]
fn projection_limit_stops_before_a_later_invalid_neighbor() {
    let (_dir, db) = setup();
    let catalog = db.catalog().unwrap().unwrap();
    let hub = brainwire_store::graph::find_by_entity_id(&db.begin(), "hub", None).unwrap()[0];
    let ty = catalog.edge_type("WANTED").unwrap().id;
    let mut key = Vec::new();
    key.extend_from_slice(&hub.0.to_be_bytes());
    key.extend_from_slice(&ty.0.to_be_bytes());
    key.extend_from_slice(&NodeId(u64::MAX).0.to_be_bytes());
    key.extend_from_slice(&EdgeId::MERGED.0.to_be_bytes());
    let mut tx = db.begin();
    tx.put(Cf::Out, key, Vec::new());
    tx.commit().unwrap();
    assert_eq!(
        run(&db, &format!("{OUT} RETURN p.entity_id LIMIT 1")),
        strings(&["hub"])
    );
    assert!(execute(&db, &format!("{OUT} RETURN p.entity_id"), &BTreeMap::new()).is_err());
}
