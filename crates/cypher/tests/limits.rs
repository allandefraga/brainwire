use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, QueryTransaction,
    QueryTransactionState,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Page","properties":["text"],"indexed_fields":["text"]}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}
fn limits(bytes: usize) -> ExecutionLimits {
    ExecutionLimits {
        memory_bytes: bytes,
        timeout: None,
    }
}

#[test]
fn oversized_parameters_and_fanout_fail_without_writes_and_poison_prior_runs() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin_with_limits(&db, limits(32_768)).unwrap();
    tx.run("CREATE (p:Page {entity_id:'pending'})", &BTreeMap::new())
        .unwrap();
    let params = BTreeMap::from([(
        "rows".into(),
        Value::List(vec![Value::Str("x".repeat(4096)); 20]),
    )]);
    assert!(matches!(
        tx.run("UNWIND $rows AS r RETURN r", &params),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(tx.state(), QueryTransactionState::Failed);
    assert!(tx.commit().is_err());
    assert_eq!(db.stats().unwrap().nodes, 0);
    let params = BTreeMap::from([(
        "rows".into(),
        Value::List((0..40).map(Value::Int).collect()),
    )]);
    let query = "UNWIND $rows AS a UNWIND $rows AS b RETURN a,b";
    assert!(matches!(
        execute_with_limits(&db, query, &params, limits(32_768)),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(execute(&db, query, &params).unwrap().rows.len(), 1600);
}

#[test]
fn label_index_hop_analytics_and_staged_writes_obey_the_guard() {
    let (_dir, db) = setup();
    for i in 0..60 {
        execute(
            &db,
            "CREATE (p:Page {entity_id:$id, text:'shared'})",
            &BTreeMap::from([("id".into(), Value::Str(format!("p-{i}")))]),
        )
        .unwrap();
    }
    for query in [
        "MATCH (p:Page) RETURN p",
        "MATCH (p:Page) WHERE p.text='shared' RETURN p",
        "CALL graph.project() YIELD nodeCount RETURN nodeCount",
        "CALL graph.reachable({source:'p-0', sourceLabel:'Page'}) YIELD node,depth RETURN node,depth",
    ] {
        assert!(matches!(execute_with_limits(&db, query, &BTreeMap::new(), limits(4096)), Err(ExecError::MemoryLimit { .. })), "{query}");
    }
    assert_eq!(
        execute_with_limits(
            &db,
            "MATCH (p:Page) WHERE p.entity_id='p-0' RETURN p.entity_id",
            &BTreeMap::new(),
            limits(4096)
        )
        .unwrap()
        .rows
        .len(),
        1
    );
    // Repeated scalar reads decode sequential temporary records, not four retained rows.
    let repeated = execute_with_limits(
        &db,
        "MATCH (p:Page) WHERE p.entity_id='p-0' RETURN p.entity_id,p.text,p.entity_id,p.text",
        &BTreeMap::new(),
        limits(4096),
    )
    .unwrap();
    assert_eq!(repeated.rows.len(), 1);
    assert_eq!(repeated.rows[0].len(), 4);
    let mut tx = QueryTransaction::begin_with_limits(&db, limits(8192)).unwrap();
    for i in 0..100 {
        if tx
            .run(
                "CREATE (p:Page {entity_id:$id, text:$text})",
                &BTreeMap::from([
                    ("id".into(), Value::Str(format!("new-{i}"))),
                    ("text".into(), Value::Str("x".repeat(512))),
                ]),
            )
            .is_err()
        {
            break;
        }
    }
    assert_eq!(tx.state(), QueryTransactionState::Failed);
    assert_eq!(db.stats().unwrap().nodes, 60);
    brainwire_store::graph::check_invariants(&db).unwrap();
}

#[test]
fn zero_budget_is_invalid_and_failed_query_does_not_prevent_a_new_transaction() {
    let (_dir, db) = setup();
    assert!(QueryTransaction::begin_with_limits(&db, limits(0)).is_err());
    assert!(execute_with_limits(&db, "RETURN 'large'", &BTreeMap::new(), limits(1)).is_err());
    assert_eq!(
        execute_with_limits(&db, "RETURN 1", &BTreeMap::new(), limits(4096))
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn hop_grouping_and_shortest_path_limits_discard_an_indexed_overwrite() {
    let (_dir, db) = setup();
    execute(
        &db,
        "CREATE (p:Page {entity_id:'root', text:'before'})",
        &BTreeMap::new(),
    )
    .unwrap();
    for i in 0..40 {
        execute(&db, "MATCH (a:Page {entity_id:'root'}) CREATE (b:Page {entity_id:$id, text:$id}) MERGE (a)-[:LINK]->(b)",
            &BTreeMap::from([("id".into(), Value::Str(format!("leaf-{i}")))])).unwrap();
    }
    for query in [
        "MATCH (a:Page {entity_id:'root'})-[:LINK]->(b:Page) RETURN b",
        "MATCH (p:Page) RETURN p.text, count(*) LIMIT 1",
        "CALL graph.shortestPath({source:'root',sourceLabel:'Page',target:'leaf-0',targetLabel:'Page'}) YIELD node,depth RETURN node,depth",
    ] {
        let mut tx = QueryTransaction::begin_with_limits(&db, limits(4096)).unwrap();
        tx.run("MATCH (p:Page {entity_id:'root'}) SET p.text='pending'", &BTreeMap::new()).unwrap();
        assert!(matches!(tx.run(query,&BTreeMap::new()),Err(ExecError::MemoryLimit{..})),"{query}");
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        assert!(tx.commit().is_err());
        assert_eq!(execute(&db,"MATCH (p:Page) WHERE p.text='before' RETURN p.entity_id",&BTreeMap::new()).unwrap().rows.len(),1);
        assert!(execute(&db,"MATCH (p:Page) WHERE p.text='pending' RETURN p.entity_id",&BTreeMap::new()).unwrap().rows.is_empty());
    }
    brainwire_store::graph::check_invariants(&db).unwrap();
}

#[test]
fn source_and_unused_parameters_share_one_run_guard() {
    let (_dir, db) = setup();
    let query = format!("RETURN 1{}", " ".repeat(3000));
    let params = BTreeMap::from([("unused".into(), Value::Str("x".repeat(2500)))]);
    // Each input fits separately; their combined baseline does not.
    assert!(matches!(
        execute_with_limits(&db, &query, &params, limits(4096)),
        Err(ExecError::MemoryLimit { .. })
    ));
    let mut tx = QueryTransaction::begin_with_limits(&db, limits(4096)).unwrap();
    assert!(matches!(
        tx.run(&query, &params),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(tx.state(), QueryTransactionState::Failed);
}
