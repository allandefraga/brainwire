use std::collections::BTreeMap;

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};

#[test]
fn merge_hooks_follow_the_relationship_written_in_each_direction() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item"}],"edges":[{"type":"LINK",
        "from":{"label":"Item"},"to":{"label":"Item"}}]}"#,
    )
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    let run = |query: &str| execute(&db, query, &BTreeMap::new()).unwrap().rows;
    run("CREATE (:Item {entity_id:'a'}), (:Item {entity_id:'b'})");
    let merge = |pattern: &str| {
        run(&format!(
            "MATCH (a:Item {{entity_id:'a'}}), (b:Item {{entity_id:'b'}}) \
             MERGE {pattern} ON CREATE SET r.phase='created' \
             ON MATCH SET r.phase='matched' RETURN r.phase"
        ))
    };
    let phase = |value: &str| vec![vec![Out::Value(Value::Str(value.into()))]];

    assert_eq!(merge("(a)<-[r:LINK]-(b)"), phase("created"));
    assert_eq!(merge("(a)-[r:LINK]-(b)"), phase("matched"));
    assert_eq!(merge("(a)-[r:LINK]->(b)"), phase("created"));
    assert_eq!(merge("(a)-[r:LINK]->(b)"), phase("matched"));
    assert_eq!(
        run("MATCH (a:Item {entity_id:'a'}), (b:Item {entity_id:'b'}) \
             CREATE (a)-[r:LINK {phase:'created again'}]->(b) RETURN r.phase"),
        phase("created again")
    );

    let rows = run("MATCH ()-[r:LINK]->() RETURN count(r)");
    assert_eq!(rows, vec![vec![Out::Value(Value::Int(2))]]);

    run("UNWIND [1, 2] AS i MATCH (a:Item {entity_id:'a'}) \
         MERGE (a)-[r:LINK]-(a) ON CREATE SET r.visits=1 \
         ON MATCH SET r.visits=r.visits + 1");
    assert_eq!(
        run("MATCH (a:Item {entity_id:'a'})-[r:LINK]->(a) RETURN r.visits"),
        vec![vec![Out::Value(Value::Int(2))]]
    );
}
