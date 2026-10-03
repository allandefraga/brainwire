use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Page","properties":["text","score"],"indexed_fields":["text"]}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
    db.apply_ontology(&mapping).unwrap();
    for i in 0..200 {
        execute(
            &db,
            "CREATE (p:Page {entity_id:$id,text:'shared',score:$score})",
            &BTreeMap::from([
                ("id".into(), Value::Str(format!("p-{i}"))),
                ("score".into(), Value::Int(i)),
            ]),
        )
        .unwrap();
    }
    (dir, db)
}

#[test]
fn scalar_property_reads_match_whole_nodes_before_and_after_ontology_promotion() {
    let (_dir, db) = setup();
    execute(
        &db,
        "CREATE (p:Page {entity_id:'extras',text:'some',extra:{values:[1,null,'x']}})",
        &BTreeMap::new(),
    )
    .unwrap();
    for promoted in [false, true] {
        if promoted {
            let mapping: GraphMapping = serde_json::from_str(r#"{"version":"2","nodes":[{"label":"Page","properties":["text","score","extra"],"indexed_fields":["text"]}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
            db.apply_ontology(&mapping).unwrap();
        }
        let output = execute(
            &db,
            "MATCH (p:Page {entity_id:'extras'}) RETURN p,p.entity_id,p.text,p.extra,p.missing",
            &BTreeMap::new(),
        )
        .unwrap();
        let Out::Node(node) = &output.rows[0][0] else {
            panic!("node expected")
        };
        for (index, property) in ["entity_id", "text", "extra", "missing"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                output.rows[0][index + 1],
                Out::Value(node.props.get(property).cloned().unwrap_or(Value::Null))
            );
        }
    }
}

#[test]
fn bounded_reads_and_counts_do_not_materialize_all_input_rows() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([(
        "values".into(),
        Value::List((0..40).map(Value::Int).collect()),
    )]);
    let limits = ExecutionLimits {
        memory_bytes: 32_768,
        timeout: None,
    };
    for (query, expected) in [
        ("MATCH (p:Page) RETURN p.entity_id LIMIT 1", 1),
        ("MATCH (p:Page) WHERE p.text='shared' RETURN p LIMIT 2", 2),
        ("MATCH (p:Page) WHERE p.score=199 RETURN p.score LIMIT 1", 1),
        (
            "MATCH (p:Page), (q:Page) RETURN p.entity_id,q.entity_id LIMIT 3",
            3,
        ),
        (
            "UNWIND $values AS a UNWIND $values AS b RETURN a,b LIMIT 1",
            1,
        ),
        (
            "MATCH (p:Page) RETURN count(*),count(p.score),count(p.missing)",
            1,
        ),
        ("UNWIND $values AS a UNWIND $values AS b RETURN count(*)", 1),
    ] {
        let actual = execute_with_limits(&db, query, &params, limits)
            .unwrap_or_else(|e| panic!("{query}: {e}"));
        assert_eq!(actual.rows.len(), expected, "{query}");
        assert_eq!(actual, execute(&db, query, &params).unwrap());
        if query.contains("count(p.score)") {
            assert_eq!(
                actual.rows,
                vec![vec![
                    Out::Value(Value::Int(200)),
                    Out::Value(Value::Int(200)),
                    Out::Value(Value::Int(0))
                ]]
            );
        }
        if query.ends_with("RETURN count(*)") {
            assert_eq!(actual.rows, vec![vec![Out::Value(Value::Int(1600))]]);
        }
    }
    assert!(matches!(
        execute_with_limits(&db, "MATCH (p:Page) RETURN p", &BTreeMap::new(), limits),
        Err(ExecError::MemoryLimit { .. })
    ));
}

#[test]
fn streaming_hops_overlay_empty_counts_and_validation_keep_semantics() {
    let (_dir, db) = setup();
    execute(
        &db,
        "MATCH (a:Page {entity_id:'p-0'}), (b:Page) MERGE (a)-[:LINK]->(b)",
        &BTreeMap::new(),
    )
    .unwrap();
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 32_768,
            timeout: None,
        },
    )
    .unwrap();
    tx.run(
        "MATCH (p:Page {entity_id:'p-199'}) SET p.text='changed'",
        &BTreeMap::new(),
    )
    .unwrap();
    for query in [
        "MATCH (a:Page {entity_id:'p-0'})-[:LINK]->(b:Page) WHERE b.score=199 RETURN b.text LIMIT 1",
        "MATCH (b:Page {entity_id:'p-199'})<-[:LINK]-(a:Page) RETURN a.entity_id LIMIT 1",
        "MATCH (p:Page) WHERE p.text='changed' RETURN p.entity_id LIMIT 1",
    ] {
        assert_eq!(tx.run(query,&BTreeMap::new()).unwrap().rows.len(),1,"{query}");
    }
    assert_eq!(
        tx.run(
            "MATCH (p:Page) WHERE p.text='absent' RETURN count(*)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
    assert!(tx
        .run("MATCH (p:Page) RETURN p LIMIT 0", &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
    assert!(tx
        .run("MATCH (p:Page) RETURN missing LIMIT 0", &BTreeMap::new())
        .is_err());
    assert!(tx.commit().is_err());
    assert!(execute(
        &db,
        "MATCH (p:Page) WHERE p.text='changed' RETURN p",
        &BTreeMap::new()
    )
    .unwrap()
    .rows
    .is_empty());
}

#[test]
fn read_pipeline_matches_eager_fallback_for_correlated_and_empty_inputs() {
    let (_dir, db) = setup();
    // Singleton stages preserve rows while selecting the existing deep-plan
    // eager fallback. Compare operator behavior without another query engine.
    let eager_prefix = (0..65)
        .map(|i| format!("UNWIND [1] AS singleton_{i} "))
        .collect::<String>();
    for query in [
        "MATCH (p:Page) WHERE p.score<3 RETURN p.entity_id,p.score",
        "MATCH (p:Page) WHERE p.score<3 MATCH (q:Page {entity_id:p.entity_id}) RETURN p.entity_id,q.score",
        "MATCH (p:Page) WHERE p.score<3 MATCH (q:Page) WHERE q.score<3 RETURN p.score,q.score LIMIT 4",
        "MATCH (p:Page) WHERE p.score<3 MATCH (p:Page) RETURN p.score",
        "MATCH (p:Page) WHERE p.score<3 RETURN count(*),count(p.score),count(p.missing)",
        "UNWIND [[],null,[1,2]] AS group UNWIND group AS value RETURN value",
        "UNWIND [1,null,2] AS value RETURN count(*),count(value)",
        "UNWIND [] AS value RETURN count(*),count(value)",
        "MATCH (p:Page) WHERE p.text='absent' RETURN count(*),count(p.score)",
        "MATCH (p:Page) RETURN p LIMIT 0",
    ] {
        let streamed = execute(&db, query, &BTreeMap::new()).unwrap();
        let eager = execute(&db, &format!("{eager_prefix}{query}"), &BTreeMap::new()).unwrap();
        assert_eq!(streamed, eager, "{query}");
    }
}
