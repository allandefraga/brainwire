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
fn shortest_path_returns_ordered_nodes_in_the_transaction_overlay() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    seed(&mut tx);
    tx.run(
        "CREATE (b:Page {entity_id:'d'}) MATCH (c:Page {entity_id:'c'}) CREATE (c)-[:LINK]->(b)",
        &BTreeMap::new(),
    )
    .unwrap();
    let query="CALL graph.shortestPath({source:'a', sourceLabel:'Page', target:'d', targetLabel:'Page', relationship:'LINK', maxDepth:2}) YIELD node, depth RETURN node.entity_id, depth";
    let expected = vec![
        vec![Out::Value(Value::Str("a".into())), int(0)],
        vec![Out::Value(Value::Str("c".into())), int(1)],
        vec![Out::Value(Value::Str("d".into())), int(2)],
    ];
    assert_eq!(tx.run(query, &BTreeMap::new()).unwrap().rows, expected);
    assert!(execute(&db, query, &BTreeMap::new()).is_err());
    tx.commit().unwrap();
    assert_eq!(run(&db, query).rows, expected);
    assert!(run(&db, &query.replace("maxDepth:2", "maxDepth:1"))
        .rows
        .is_empty());
    assert!(run(
        &db,
        &query
            .replace("source:'a'", "source:'d'")
            .replace("target:'d'", "target:'a'")
    )
    .rows
    .is_empty());
    assert!(execute(
        &db,
        &query.replace("target:'d'", "target:'missing'"),
        &BTreeMap::new()
    )
    .is_err());
    assert!(execute(
        &db,
        &query.replace("maxDepth:2", "weightProperty:'weight'"),
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn reachable_uses_csr_with_depth_relationship_limits_and_transaction_snapshot() {
    let (_dir, db) = setup();
    let query = "CALL graph.reachable({source:'a', sourceLabel:'Page', relationship:'LINK', maxDepth:1}) YIELD node, depth RETURN node.entity_id, depth";
    let mut tx = QueryTransaction::begin(&db).unwrap();
    seed(&mut tx);
    let expected = vec![
        vec![Out::Value(Value::Str("a".into())), int(0)],
        vec![Out::Value(Value::Str("b".into())), int(1)],
        vec![Out::Value(Value::Str("c".into())), int(1)],
    ];
    assert_eq!(tx.run(query, &BTreeMap::new()).unwrap().rows, expected);
    assert!(execute(&db, query, &BTreeMap::new()).is_err());
    tx.commit().unwrap();
    assert_eq!(run(&db, query).rows, expected);
    assert_eq!(run(&db, "CALL graph.reachable({source:'a', sourceLabel:'Page', relationship:'OTHER'}) YIELD node, depth RETURN node.entity_id, depth").rows, vec![expected[0].clone()]);
    for options in [
        "{}",
        "{source:'a'}",
        "{source:'missing', sourceLabel:'Page'}",
        "{source:'a', sourceLabel:'Unknown'}",
        "{source:'a', sourceLabel:'Page', maxDepth:0}",
        "{source:'a', sourceLabel:'Page', maxNodes:2}",
        "{source:'a', sourceLabel:'Page', weightProperty:'weight'}",
    ] {
        let query = format!("CALL graph.reachable({options}) YIELD node RETURN node");
        assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{options}");
    }
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

fn component_seed(db: &Database) {
    run(db, "CREATE (a:Page {entity_id:'a'}), (b:Page {entity_id:'b'}), (c:Page {entity_id:'c'}), (d:Page {entity_id:'d'}), (e:Page {entity_id:'e'}), (f:Page {entity_id:'f'}), (z:Page {entity_id:'z'}) CREATE (a)-[:LINK {weight:0}]->(b) CREATE (b)-[:LINK {weight:-2}]->(a) CREATE (b)-[:LINK]->(c) CREATE (c)-[:LINK]->(d) CREATE (d)-[:LINK]->(c) CREATE (d)-[:LINK]->(e) CREATE (e)-[:OTHER]->(f) CREATE (f)-[:LINK]->(f)");
}
fn component_rows(db: &Database, procedure: &str, options: &str) -> Vec<Vec<Out>> {
    run(db,&format!("CALL {procedure}({options}) YIELD node, component, size RETURN node.entity_id AS id, component.entity_id AS representative, size")).rows
}
fn component_expected(values: &[(&str, &str, i64)]) -> Vec<Vec<Out>> {
    values
        .iter()
        .map(|(id, representative, size)| {
            vec![
                Out::Value(Value::Str((*id).into())),
                Out::Value(Value::Str((*representative).into())),
                int(*size),
            ]
        })
        .collect()
}

#[test]
fn weak_and_strong_components_include_isolates_and_ignore_stored_weight_properties() {
    let (_dir, db) = setup();
    component_seed(&db);
    let weak = component_expected(&[
        ("a", "a", 5),
        ("b", "a", 5),
        ("c", "a", 5),
        ("d", "a", 5),
        ("e", "a", 5),
        ("f", "f", 1),
        ("z", "z", 1),
    ]);
    let strong = component_expected(&[
        ("a", "a", 2),
        ("b", "a", 2),
        ("c", "c", 2),
        ("d", "c", 2),
        ("e", "e", 1),
        ("f", "f", 1),
        ("z", "z", 1),
    ]);
    assert_eq!(
        component_rows(&db, "graph.wcc", "{relationship:'LINK'}"),
        weak
    );
    assert_eq!(
        component_rows(&db, "graph.scc", "{relationship:'LINK'}"),
        strong
    );
    for (procedure, expected) in [("graph.wcc", 3), ("graph.scc", 5)] {
        assert_eq!(run(&db,&format!("CALL {procedure}({{relationship:'LINK'}}) YIELD node,component WHERE node.entity_id=component.entity_id RETURN count(*) AS components")).rows,vec![vec![int(expected)]]);
    }
    let all = component_rows(&db, "graph.wcc", "{}");
    assert_eq!(
        all,
        component_expected(&[
            ("a", "a", 6),
            ("b", "a", 6),
            ("c", "a", 6),
            ("d", "a", 6),
            ("e", "a", 6),
            ("f", "a", 6),
            ("z", "z", 1)
        ])
    );
    let aliased=run(&db,"CALL graph.scc({relationship:'LINK'}) YIELD node AS member, component AS representative, size AS members WHERE members=2 AND representative.entity_id='a' RETURN member.entity_id, representative.entity_id, members");
    assert_eq!(aliased.rows, strong[..2]);
    for procedure in ["graph.wcc", "graph.scc"] {
        let query=format!("CALL {procedure}({{relationship:'LINK'}}) YIELD component AS c, size WHERE size>1 RETURN c.entity_id LIMIT 1");
        let mut read = QueryTransaction::begin(&db).unwrap();
        let output = read
            .run_with_access(
                &query,
                &BTreeMap::new(),
                brainwire_cypher::QueryAccess {
                    write: false,
                    instance_admin: false,
                },
            )
            .unwrap();
        assert_eq!(output.rows, vec![vec![Out::Value(Value::Str("a".into()))]]);
        assert_eq!(read.commit().unwrap(), None);
    }
}

#[test]
fn component_projection_observes_staged_overlay_snapshot_rollback_and_reopen() {
    let (dir, db) = setup();
    component_seed(&db);
    let before = component_rows(&db, "graph.scc", "{relationship:'LINK'}");
    let mut old = QueryTransaction::begin(&db).unwrap();
    let mut staged = QueryTransaction::begin(&db).unwrap();
    staged
        .run(
            "MATCH (e:Page {entity_id:'e'}),(a:Page {entity_id:'a'}) CREATE (e)-[:LINK]->(a)",
            &BTreeMap::new(),
        )
        .unwrap();
    let query="CALL graph.scc({relationship:'LINK'}) YIELD node,component,size RETURN node.entity_id,component.entity_id,size";
    assert_eq!(
        staged.run(query, &BTreeMap::new()).unwrap().rows[4],
        component_expected(&[("e", "a", 5)])[0]
    );
    assert_eq!(run(&db, query).rows, before);
    staged.rollback();
    assert_eq!(run(&db, query).rows, before);
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer
        .run(
            "MATCH (e:Page {entity_id:'e'}),(a:Page {entity_id:'a'}) CREATE (e)-[:LINK]->(a)",
            &BTreeMap::new(),
        )
        .unwrap();
    writer.commit().unwrap();
    drop(writer);
    assert_eq!(old.run(query, &BTreeMap::new()).unwrap().rows, before);
    old.rollback();
    drop(old);
    drop(staged);
    let after = run(&db, query).rows;
    assert_eq!(after[4], component_expected(&[("e", "a", 5)])[0]);
    drop(db);
    let reopened = open(dir.path());
    assert_eq!(run(&reopened, query).rows, after);
}

#[test]
fn component_validation_and_caps_cannot_be_hidden_by_empty_graph_or_limit_zero() {
    let (_dir, db) = setup();
    for procedure in ["graph.wcc", "graph.scc"] {
        let empty=run(&db,&format!("CALL {procedure}() YIELD component AS representative,size RETURN representative.entity_id,size"));
        assert_eq!(empty.fields, ["representative.entity_id", "size"]);
        assert!(empty.rows.is_empty());
        for options in [
            "{weightProperty:'weight'}",
            "{relationship:'UNKNOWN'}",
            "{maxNodes:0}",
            "{maxRelationships:0}",
            "{source:'a'}",
            "'invalid'",
        ] {
            let query = format!("CALL {procedure}({options}) YIELD node RETURN node LIMIT 0");
            assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{query}");
        }
        for tail in [
            "YIELD missing RETURN missing LIMIT 0",
            "YIELD node AS duplicate,component AS duplicate RETURN duplicate LIMIT 0",
            "YIELD component RETURN unknown LIMIT 0",
            "YIELD component WHERE 123 RETURN component LIMIT 0",
        ] {
            let query = format!("CALL {procedure}() {tail}");
            assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{query}");
        }
    }
    component_seed(&db);
    for procedure in ["graph.wcc", "graph.scc"] {
        for cap in ["maxNodes:6", "maxRelationships:7"] {
            let query = format!("CALL {procedure}({{{cap}}}) YIELD node RETURN node LIMIT 0");
            assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{query}");
        }
    }
}

#[test]
fn component_projection_preserves_pool_exhaustion_and_releases_failed_reservations() {
    use brainwire_cypher::{ExecError, ExecutionLimits, QueryMemoryPool};
    use std::sync::Arc;
    let (_dir, db) = setup();
    component_seed(&db);
    let params = BTreeMap::from([("text".into(), Value::Str("x".repeat(128)))]);
    let probe = Arc::new(QueryMemoryPool::new(64_000));
    let mut holder =
        QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&probe))
            .unwrap();
    holder.run("RETURN $text", &params).unwrap();
    let held = probe.usage().reserved_bytes;
    holder.rollback();
    assert_eq!(probe.usage().reserved_bytes, 0);
    for procedure in ["graph.wcc", "graph.scc", "graph.project"] {
        let pool = Arc::new(QueryMemoryPool::new(held + 1000));
        let mut holder =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap();
        holder.run("RETURN $text", &params).unwrap();
        assert_eq!(pool.usage().reserved_bytes, held);
        let mut constrained =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap();
        let query = if procedure == "graph.project" {
            "CALL graph.project() YIELD nodeCount RETURN nodeCount".into()
        } else {
            format!("CALL {procedure}() YIELD node,component,size RETURN node.entity_id,component.entity_id,size")
        };
        let error = constrained.run(&query, &BTreeMap::new()).unwrap_err();
        // A 128-byte allocation delta identifies the controlled projection precharge.
        assert!(
            matches!(error, ExecError::MemoryPoolExhausted { requested: 128, .. }),
            "{error:?}"
        );
        assert_eq!(constrained.state(), QueryTransactionState::Failed);
        assert!(constrained.commit().is_err());
        assert_eq!(pool.usage().reserved_bytes, held);
        assert_eq!(pool.usage().rejections, 1);
        holder.rollback();
        assert_eq!(pool.usage().reserved_bytes, 0);
        let mut fresh =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap();
        fresh.run("RETURN 1", &BTreeMap::new()).unwrap();
        fresh.commit().unwrap();
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
    for procedure in ["graph.wcc", "graph.scc"] {
        let pool = Arc::new(QueryMemoryPool::new(64_000));
        let mut transaction =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap();
        transaction.run(&format!("CALL {procedure}() YIELD node,component,size RETURN node.entity_id,component.entity_id,size"),&BTreeMap::new()).unwrap();
        assert!(pool.usage().reserved_bytes > 0);
        transaction.rollback();
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}

#[test]
fn components_enforce_query_memory_and_cooperative_deadlines_without_leaking_pool() {
    use brainwire_cypher::{execute_with_limits, ExecError, ExecutionLimits, QueryMemoryPool};
    use std::{sync::Arc, time::Duration};
    let (_dir, db) = setup();
    component_seed(&db);
    for procedure in ["graph.wcc", "graph.scc"] {
        let query=format!("CALL {procedure}() YIELD node,component,size RETURN node.entity_id,component.entity_id,size LIMIT 0");
        assert!(matches!(
            execute_with_limits(
                &db,
                &query,
                &BTreeMap::new(),
                ExecutionLimits {
                    memory_bytes: 1200,
                    timeout: None
                }
            ),
            Err(ExecError::MemoryLimit { .. })
        ));
        let pool = Arc::new(QueryMemoryPool::new(64_000));
        let mut expired = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: 64_000,
                timeout: Some(Duration::ZERO),
            },
            Arc::clone(&pool),
        )
        .unwrap();
        assert!(matches!(
            expired.run(&query, &BTreeMap::new()),
            Err(ExecError::TimedOut)
        ));
        assert_eq!(expired.state(), QueryTransactionState::Failed);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}
