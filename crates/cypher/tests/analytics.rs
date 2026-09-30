use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryOutput, QueryTransaction, QueryTransactionState};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let schema: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Page"}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"},"properties":["weight"]},{"type":"OTHER","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
    db.apply_ontology(&schema).unwrap();
    (dir, db)
}
fn run(db: &Database, query: &str) -> QueryOutput {
    execute(db, query, &BTreeMap::new()).unwrap()
}
fn seed(tx: &mut QueryTransaction<'_>) {
    tx.run("CREATE (a:Page {entity_id:'a'}), (b:Page {entity_id:'b'}), (c:Page {entity_id:'c'}) CREATE (a)-[:LINK {weight: 2.0}]->(b) CREATE (a)-[:LINK {weight: 1.0}]->(c)", &BTreeMap::new()).unwrap();
}
fn int(n: i64) -> Out {
    Out::Value(Value::Int(n))
}

#[test]
fn project_degree_and_weighted_pagerank_use_cypher() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    seed(&mut tx);
    tx.commit().unwrap();
    assert_eq!(run(&db, "CALL graph.project({relationship: 'LINK'}) YIELD nodeCount, relationshipCount, nonZeroCount RETURN nodeCount, relationshipCount, nonZeroCount").rows, vec![vec![int(3), int(2), int(2)]]);
    let degrees = run(&db, "CALL graph.degree({relationship:'LINK'}) YIELD node, outDegree, inDegree RETURN node.entity_id AS id, outDegree, inDegree");
    assert_eq!(degrees.fields, ["id", "outDegree", "inDegree"]);
    assert_eq!(
        degrees.rows[0],
        vec![Out::Value(Value::Str("a".into())), int(2), int(0)]
    );
    let scores = run(&db, "CALL graph.pagerank({relationship:'LINK', weightProperty:'weight', maxIterations:100, tolerance:0.00000001}) YIELD node, score, converged RETURN node.entity_id, score, converged");
    let values: Vec<f64> = scores
        .rows
        .iter()
        .map(|r| match r[1] {
            Out::Value(Value::Float(v)) => v,
            _ => panic!("expected score"),
        })
        .collect();
    assert!((values.iter().sum::<f64>() - 1.0).abs() < 1e-10);
    assert!(values[0] > values[1] && values[1] > values[2]);
    assert_eq!(scores.rows[0][0], Out::Value(Value::Str("b".into())));
    assert!(scores
        .rows
        .iter()
        .all(|r| r[2] == Out::Value(Value::Bool(true))));
    assert_eq!(run(&db, "CALL graph.degree({}) YIELD node AS n, outDegree AS degree WHERE degree > 0 RETURN n.entity_id LIMIT 1").rows.len(), 1);
}

#[test]
fn analytics_see_staged_writes_and_remain_private_then_rebuild_after_reopen() {
    let (dir, db) = setup();
    let q = "CALL graph.project({}) YIELD nodeCount, relationshipCount RETURN nodeCount, relationshipCount";
    let mut tx = QueryTransaction::begin(&db).unwrap();
    seed(&mut tx);
    assert_eq!(
        tx.run(q, &BTreeMap::new()).unwrap().rows,
        vec![vec![int(3), int(2)]]
    );
    assert_eq!(run(&db, q).rows, vec![vec![int(0), int(0)]]);
    tx.commit().unwrap();
    drop(tx);
    let before = run(
        &db,
        "CALL graph.pagerank({}) YIELD node, score RETURN node.entity_id, score",
    );
    drop(db);
    let reopened = open(dir.path());
    assert_eq!(run(&reopened, q).rows, vec![vec![int(3), int(2)]]);
    assert_eq!(
        run(
            &reopened,
            "CALL graph.pagerank({}) YIELD node, score RETURN node.entity_id, score"
        ),
        before
    );
}

#[test]
fn failure_poisons_transaction_and_limits_cannot_be_hidden_by_limit_zero() {
    let (_dir, db) = setup();
    for query in [
        "CALL graph.project({maxNodes:2}) YIELD nodeCount RETURN nodeCount LIMIT 0",
        "CALL graph.project({maxRelationships:1}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.project({relationship:'UNKNOWN'}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.pagerank({dampingFactor:1.5}) YIELD score RETURN score",
        "CALL graph.pagerank({maxIterations:0}) YIELD score RETURN score",
        "CALL graph.pagerank({tolerance:-1}) YIELD score RETURN score",
        "CALL graph.project({typo:1}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.degree({}) YIELD missing RETURN missing",
        "CALL graph.degree({}) YIELD node AS x, outDegree AS x RETURN x",
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        seed(&mut tx);
        assert!(tx.run(query, &BTreeMap::new()).is_err(), "{query}");
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        assert!(tx.commit().is_err());
        assert_eq!(db.stats().unwrap().nodes, 0);
    }
}

#[test]
fn empty_graph_and_parameter_options_have_stable_fields() {
    let (_dir, db) = setup();
    let output = execute(
        &db,
        "CALL graph.pagerank($options) YIELD node, score RETURN node, score",
        &BTreeMap::from([("options".into(), Value::Map(BTreeMap::new()))]),
    )
    .unwrap();
    assert_eq!(output.fields, ["node", "score"]);
    assert!(output.rows.is_empty());
    assert!(execute(
        &db,
        "CALL unknown.proc({}) YIELD x RETURN x",
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn snapshot_isolation_deletions_and_rollback_apply_to_analytics() {
    let (_dir, db) = setup();
    let q = "CALL graph.project() YIELD nodeCount, relationshipCount RETURN nodeCount, relationshipCount";
    let mut old = QueryTransaction::begin(&db).unwrap();
    let mut writer = QueryTransaction::begin(&db).unwrap();
    seed(&mut writer);
    writer.commit().unwrap();
    assert_eq!(
        old.run(q, &BTreeMap::new()).unwrap().rows,
        vec![vec![int(0), int(0)]]
    );
    assert_eq!(old.commit().unwrap(), None);
    let mut deletion = QueryTransaction::begin(&db).unwrap();
    deletion
        .run(
            "MATCH (a:Page {entity_id:'a'}) DETACH DELETE a",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        deletion.run(q, &BTreeMap::new()).unwrap().rows,
        vec![vec![int(2), int(0)]]
    );
    deletion.rollback();
    assert_eq!(run(&db, q).rows, vec![vec![int(3), int(2)]]);
}

#[test]
fn weighted_edges_reject_invalid_data_and_zero_weight_is_dangling() {
    let (_dir, db) = setup();
    let query = "CALL graph.pagerank({weightProperty:'weight'}) YIELD node, score RETURN node.entity_id, score";
    for weight in [
        Value::Str("bad".into()),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NAN),
        Value::Float(-1.0),
        Value::Float(1e300),
        Value::Float(1e-300),
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        tx.run("CREATE (a:Page {entity_id:'a'}), (b:Page {entity_id:'b'}) CREATE (a)-[:LINK {weight:$w}]->(b)", &BTreeMap::from([("w".into(), weight)])).unwrap();
        assert!(tx.run(query, &BTreeMap::new()).is_err());
        assert_eq!(db.stats().unwrap().nodes, 0);
    }
    run(&db, "CREATE (a:Page {entity_id:'a'}), (b:Page {entity_id:'b'}) CREATE (a)-[:LINK {weight:0}]->(b)");
    let output = run(&db, query);
    assert!(output
        .rows
        .iter()
        .all(|r| r[1] == Out::Value(Value::Float(0.5))));
    assert_eq!(run(&db, "CALL graph.project({weightProperty:'weight'}) YIELD relationshipCount, nonZeroCount RETURN relationshipCount, nonZeroCount").rows, vec![vec![int(1), int(0)]]);
    assert_eq!(
        run(&db, "CALL graph.degree() YIELD outDegree RETURN outDegree").rows,
        vec![vec![int(1)], vec![int(0)]]
    );
}

#[test]
fn filters_and_selected_fields_cannot_hide_bad_arguments_or_bad_return() {
    let (_dir, db) = setup();
    for query in [
        "CALL graph.project('name') YIELD nodeCount RETURN nodeCount",
        "CALL graph.project({}, {}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.project({maxNodes:1.5}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.project({maxNodes:1000001}) YIELD nodeCount RETURN nodeCount",
        "CALL graph.degree({weightProperty:'x'}) YIELD node RETURN node",
        "CALL graph.project({}) YIELD nodeCount",
        "CALL graph.project({}) YIELD nodeCount RETURN unknown",
        "CALL graph.degree({}) YIELD node WHERE 123 RETURN node",
        "UNWIND [] AS x CALL graph.project({}) YIELD nodeCount RETURN nodeCount",
        "CREATE (a:Page {entity_id:'a'}) CALL graph.project() YIELD nodeCount RETURN nodeCount",
        "CALL graph.degree() YIELD node SET node.x = 1 RETURN node",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
        assert_eq!(db.stats().unwrap().nodes, 0);
    }
}
