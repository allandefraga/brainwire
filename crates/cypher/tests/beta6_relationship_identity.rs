use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version": "1",
        "nodes": [{"label": "Ticket"}],
        "edges": [
            {"type": "EVENT", "from": {"label": "Ticket"}, "to": {"label": "Ticket"},
             "properties": ["event_key", "summary"], "merge_key_fields": ["event_key"]},
            {"type": "RELATED", "from": {"label": "Ticket"}, "to": {"label": "Ticket"},
             "properties": ["summary"]}
        ]
    }))
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(
        &db,
        "CREATE (:Ticket {entity_id:'a'}), (:Ticket {entity_id:'b'})",
    );
    (dir, db)
}

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
}

fn text(value: &str) -> Out {
    Out::Value(Value::Str(value.into()))
}

fn events(db: &Database) -> Vec<Vec<Out>> {
    run(db, "MATCH (:Ticket {entity_id:'a'})-[r:EVENT]->(:Ticket {entity_id:'b'}) RETURN r.event_key, r.summary ORDER BY r.event_key")
}

fn event(db: &Database, key: &str, summary: &str) {
    run(db, &format!("MATCH (a:Ticket {{entity_id:'a'}}), (b:Ticket {{entity_id:'b'}}) MERGE (a)-[r:EVENT {{event_key:'{key}'}}]->(b) SET r.summary='{summary}'"));
}

#[test]
fn undeclared_merge_identity_preserves_legacy_property_upserts() {
    let (_dir, db) = setup();
    for summary in ["first", "updated"] {
        run(&db, &format!("MATCH (a:Ticket {{entity_id:'a'}}), (b:Ticket {{entity_id:'b'}}) MERGE (a)-[:RELATED {{summary:'{summary}'}}]->(b)"));
    }
    assert_eq!(
        run(
            &db,
            "MATCH (:Ticket)-[r:RELATED]->(:Ticket) RETURN r.summary"
        ),
        vec![vec![text("updated")]]
    );
}

#[test]
fn declared_key_preserves_distinct_events_and_replays_update_only_their_event() {
    let (_dir, db) = setup();
    event(&db, "opened", "first");
    event(&db, "closed", "second");
    event(&db, "opened", "updated");
    assert_eq!(
        events(&db),
        vec![
            vec![text("closed"), text("second")],
            vec![text("opened"), text("updated")]
        ]
    );
}

#[test]
fn keyed_mutation_and_delete_keep_the_other_physical_relationship() {
    let (_dir, db) = setup();
    event(&db, "opened", "first");
    event(&db, "closed", "second");
    run(
        &db,
        "MATCH (:Ticket)-[r:EVENT {event_key:'opened'}]->(:Ticket) SET r.summary='changed'",
    );
    assert_eq!(
        events(&db),
        vec![
            vec![text("closed"), text("second")],
            vec![text("opened"), text("changed")]
        ]
    );
    run(
        &db,
        "MATCH (:Ticket)-[r:EVENT {event_key:'closed'}]->(:Ticket) DELETE r",
    );
    assert_eq!(events(&db), vec![vec![text("opened"), text("changed")]]);
    assert_eq!(
        run(&db, "MATCH (:Ticket {entity_id:'b'})<-[r:EVENT]-(:Ticket {entity_id:'a'}) RETURN r.event_key, r.summary"),
        vec![vec![text("opened"), text("changed")]]
    );
}

#[test]
fn keyed_relationships_survive_reopen_without_changing_legacy_edges() {
    let (dir, db) = setup();
    event(&db, "opened", "first");
    event(&db, "closed", "second");
    drop(db);
    let db = open(dir.path());
    assert_eq!(
        events(&db),
        vec![
            vec![text("closed"), text("second")],
            vec![text("opened"), text("first")]
        ]
    );
    event(&db, "opened", "replayed");
    assert_eq!(
        events(&db),
        vec![
            vec![text("closed"), text("second")],
            vec![text("opened"), text("replayed")]
        ]
    );
}

#[test]
fn rollback_removes_the_new_event_and_keeps_committed_events() {
    let (_dir, db) = setup();
    event(&db, "opened", "first");
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run("MATCH (a:Ticket {entity_id:'a'}), (b:Ticket {entity_id:'b'}) MERGE (a)-[r:EVENT {event_key:'closed'}]->(b) SET r.summary='second'", &BTreeMap::new()).unwrap();
    assert_eq!(
        tx.run(
            "MATCH (:Ticket)-[r:EVENT]->(:Ticket) RETURN r.event_key ORDER BY r.event_key",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![text("closed")], vec![text("opened")]]
    );
    tx.rollback();
    assert_eq!(events(&db), vec![vec![text("opened"), text("first")]]);
}

#[test]
fn missing_or_null_declared_key_refuses_the_write_without_mutation() {
    let (_dir, db) = setup();
    event(&db, "opened", "first");
    for pattern in ["[r:EVENT]", "[r:EVENT {event_key:null}]"] {
        let query = format!("MATCH (a:Ticket {{entity_id:'a'}}), (b:Ticket {{entity_id:'b'}}) MERGE (a)-{pattern}->(b) SET r.summary='invalid'");
        assert!(
            execute(&db, &query, &BTreeMap::new()).is_err(),
            "accepted: {query}"
        );
        assert_eq!(events(&db), vec![vec![text("opened"), text("first")]]);
    }
}

#[test]
fn declared_key_cannot_be_changed_or_removed() {
    let (_dir, db) = setup();
    event(&db, "opened", "first");
    for update in ["SET r.event_key='renamed'", "REMOVE r.event_key"] {
        let query = format!("MATCH (:Ticket)-[r:EVENT {{event_key:'opened'}}]->(:Ticket) {update}");
        assert!(
            execute(&db, &query, &BTreeMap::new()).is_err(),
            "accepted: {query}"
        );
        assert_eq!(events(&db), vec![vec![text("opened"), text("first")]]);
    }
}

#[test]
fn extra_pattern_properties_update_payload_without_changing_declared_identity() {
    let (_dir, db) = setup();
    for summary in ["first", "updated"] {
        run(&db, &format!("MATCH (a:Ticket {{entity_id:'a'}}), (b:Ticket {{entity_id:'b'}}) MERGE (a)-[:EVENT {{event_key:'opened', summary:'{summary}'}}]->(b)"));
    }
    assert_eq!(events(&db), vec![vec![text("opened"), text("updated")]]);
    let wrong = "MATCH (a:Ticket {entity_id:'a'}), (b:Ticket {entity_id:'b'}) MERGE (a)-[:EVENT {wrong_key:'opened'}]->(b)";
    assert!(execute(&db, wrong, &BTreeMap::new()).is_err());
    assert_eq!(events(&db), vec![vec![text("opened"), text("updated")]]);
}

#[test]
fn concurrent_same_key_creates_cannot_commit_duplicate_relationships() {
    let (_dir, db) = setup();
    let mut first = QueryTransaction::begin(&db).unwrap();
    let mut second = QueryTransaction::begin(&db).unwrap();
    let query = "MATCH (a:Ticket {entity_id:'a'}), (b:Ticket {entity_id:'b'}) MERGE (a)-[r:EVENT {event_key:'opened'}]->(b) SET r.summary='first'";
    first.run(query, &BTreeMap::new()).unwrap();
    second.run(query, &BTreeMap::new()).unwrap();
    first.commit().unwrap();
    assert!(matches!(
        second.commit(),
        Err(brainwire_cypher::ExecError::Conflict)
    ));
    assert_eq!(events(&db), vec![vec![text("opened"), text("first")]]);
    event(&db, "opened", "replayed");
    assert_eq!(events(&db), vec![vec![text("opened"), text("replayed")]]);
}
