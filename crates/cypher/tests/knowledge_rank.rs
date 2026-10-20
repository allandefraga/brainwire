use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryOutput, QueryTransaction,
    QueryTransactionState,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup(indexed: bool) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let schema: GraphMapping = serde_json::from_value(serde_json::json!({
        "version":"1", "nodes":[{"label":"Source",
        "properties":["term","domain","authority","authorRelevance","usage","certified","freshness"],
        "indexed_fields": if indexed { vec!["term"] } else { vec![] }}]
    })).unwrap();
    db.apply_ontology(&schema).unwrap();
    (dir, db)
}
fn run(db: &Database, query: &str) -> QueryOutput {
    execute(db, query, &BTreeMap::new()).unwrap()
}
const QUERY: &str = "CALL knowledge.rank({label:'Source', term:'revenue', domain:'finance', topK:2}) YIELD node, score, contributions RETURN node.entity_id, score, contributions";
fn seed(db: &Database) {
    run(db, "CREATE (a:Source {entity_id:'official', term:'revenue', domain:'finance', authority:1, authorRelevance:1, certified:true, freshness:1}), (b:Source {entity_id:'popular', term:'revenue', domain:'finance', usage:1}), (c:Source {entity_id:'other', term:'revenue', domain:'sales', authority:1, authorRelevance:1, usage:1, certified:true, freshness:1})");
}

#[test]
fn ranks_explains_filters_and_survives_reopen_with_index_or_scan() {
    for indexed in [false, true] {
        let (dir, db) = setup(indexed);
        seed(&db);
        let ranked = run(&db, QUERY);
        assert_eq!(ranked.rows.len(), 2);
        assert_eq!(ranked.rows[0][0], Out::Value(Value::Str("official".into())));
        assert_eq!(ranked.rows[1][0], Out::Value(Value::Str("popular".into())));
        let Out::Value(Value::Float(score)) = ranked.rows[0][1] else {
            panic!()
        };
        assert!((score - 0.9).abs() < 1e-12);
        let Out::Value(Value::Map(parts)) = &ranked.rows[0][2] else {
            panic!()
        };
        let total: f64 = parts
            .values()
            .map(|v| match v {
                Value::Float(n) => *n,
                _ => panic!(),
            })
            .sum();
        assert!((total - score).abs() < 1e-12);
        assert_eq!(parts["usage"], Value::Float(0.0));
        assert!(run(&db, &QUERY.replace("term:'revenue'", "term:'missing'"))
            .rows
            .is_empty());
        assert_eq!(
            run(&db, &QUERY.replace("topK:2", "topK:1")).rows,
            ranked.rows[..1]
        );
        drop(db);
        assert_eq!(run(&open(dir.path()), QUERY), ranked);
    }
}

#[test]
fn custom_weights_change_ranking_and_expose_normalized_contributions() {
    let (_dir, db) = setup(true);
    seed(&db);
    let query = "CALL knowledge.rank({label:'Source',term:'revenue',domain:'finance',topK:2,weights:{authority:1,authorRelevance:1,usage:16,certified:1,freshness:1}}) YIELD node,score,contributions RETURN node.entity_id,score,contributions";
    let ranked = run(&db, query);
    assert_eq!(ranked.rows[0][0], Out::Value(Value::Str("popular".into())));
    let Out::Value(Value::Float(score)) = ranked.rows[0][1] else {
        panic!()
    };
    assert!((score - 0.8).abs() < 1e-12);
    let Out::Value(Value::Map(parts)) = &ranked.rows[0][2] else {
        panic!()
    };
    let Value::Float(usage) = parts["usage"] else {
        panic!()
    };
    assert!((usage - 0.8).abs() < 1e-12);
    assert_eq!(parts["authority"], Value::Float(0.0));
    let total: f64 = parts
        .values()
        .map(|value| match value {
            Value::Float(number) => *number,
            _ => panic!("contributions must be floating point values"),
        })
        .sum();
    assert!((total - score).abs() < 1e-12);

    for weights in [
        "{authority:1}",
        "{authority:-1,authorRelevance:0,usage:0,certified:0,freshness:0}",
        "{authority:0,authorRelevance:0,usage:0,certified:0,freshness:0}",
        "{authority:'high',authorRelevance:1,usage:1,certified:1,freshness:1}",
        "{authority:1,authorRelevance:1,usage:1,certified:1,freshness:1,typo:1}",
    ] {
        let query = format!(
            "CALL knowledge.rank({{label:'Source',term:'revenue',weights:{weights}}}) YIELD node RETURN node LIMIT 0"
        );
        assert!(execute(&db, &query, &BTreeMap::new()).is_err(), "{weights}");
    }

    for invalid_weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let params = BTreeMap::from([(
            "weights".into(),
            Value::Map(BTreeMap::from([
                ("authority".into(), Value::Float(invalid_weight)),
                ("authorRelevance".into(), Value::Float(1.0)),
                ("usage".into(), Value::Float(1.0)),
                ("certified".into(), Value::Float(1.0)),
                ("freshness".into(), Value::Float(1.0)),
            ])),
        )]);
        let query = "CALL knowledge.rank({label:'Source',term:'revenue',weights:$weights}) YIELD node RETURN node LIMIT 0";
        assert!(execute(&db, query, &params).is_err());
    }
}

#[test]
fn updates_are_snapshot_local_and_invalid_signals_abort_prior_writes() {
    let (_dir, db) = setup(true);
    seed(&db);
    let mut old = QueryTransaction::begin(&db).unwrap();
    let before = old.run(QUERY, &BTreeMap::new()).unwrap();
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer.run("MATCH (s:Source {entity_id:'popular'}) SET s.authority=1, s.authorRelevance=1, s.certified=true, s.freshness=1", &BTreeMap::new()).unwrap();
    assert_eq!(
        writer.run(QUERY, &BTreeMap::new()).unwrap().rows[0][0],
        Out::Value(Value::Str("popular".into()))
    );
    assert_eq!(run(&db, QUERY), before);
    writer.commit().unwrap();
    assert_eq!(old.run(QUERY, &BTreeMap::new()).unwrap(), before);
    old.rollback();
    for bad in [
        Value::Float(-0.1),
        Value::Float(1.1),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Str("high".into()),
    ] {
        let mut tx = QueryTransaction::begin(&db).unwrap();
        tx.run(
            "MATCH (s:Source {entity_id:'official'}) SET s.authority=$bad",
            &BTreeMap::from([("bad".into(), bad)]),
        )
        .unwrap();
        assert!(tx.run(QUERY, &BTreeMap::new()).is_err());
        assert_eq!(tx.state(), QueryTransactionState::Failed);
        assert!(tx.commit().is_err());
        assert_eq!(
            run(
                &db,
                "MATCH (s:Source {entity_id:'official'}) RETURN s.authority"
            )
            .rows[0][0],
            Out::Value(Value::Int(1))
        );
    }
}

#[test]
fn options_candidate_limits_and_ties_are_deterministic() {
    let (_dir, db) = setup(true);
    run(&db, "CREATE (a:Source {entity_id:'a', term:'revenue'}), (b:Source {entity_id:'b', term:'revenue'}), (c:Source {entity_id:'c', term:'revenue'})");
    let q = "CALL knowledge.rank({label:'Source', term:'revenue', topK:2}) YIELD node, score RETURN node.entity_id, score";
    let ranked = run(&db, q);
    assert_eq!(
        ranked.rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        vec![
            Out::Value(Value::Str("a".into())),
            Out::Value(Value::Str("b".into()))
        ]
    );
    assert!(ranked
        .rows
        .iter()
        .all(|r| r[1] == Out::Value(Value::Float(0.0))));
    for opts in [
        "{}",
        "{label:'Source'}",
        "{label:'Unknown',term:'revenue'}",
        "{label:'Source',term:'revenue',topK:0}",
        "{label:'Source',term:'revenue',topK:1001}",
        "{label:'Source',term:'revenue',typo:1}",
        "{label:'Source',term:'revenue',maxCandidates:2}",
    ] {
        let q = format!("CALL knowledge.rank({opts}) YIELD node RETURN node LIMIT 0");
        assert!(execute(&db, &q, &BTreeMap::new()).is_err(), "{opts}");
    }
    assert!(execute(
        &db,
        "CALL knowledge.rank({}, {}) YIELD node RETURN node",
        &BTreeMap::new()
    )
    .is_err());
    run(&db, "MATCH (s:Source {entity_id:'c'}) SET s.certified=1");
    assert!(execute(&db, q, &BTreeMap::new()).is_err());
}

#[test]
fn term_index_bounds_work_before_unrelated_nodes_are_read() {
    for indexed in [true, false] {
        let (_dir, db) = setup(indexed);
        seed(&db);
        run(
            &db,
            "CREATE (x:Source {entity_id:'unrelated',term:'expenses',authority:'invalid'})",
        );
        let q = QUERY.replace("topK:2", "topK:2,maxCandidates:3");
        assert_eq!(execute(&db, &q, &BTreeMap::new()).is_ok(), indexed);
    }
}

#[test]
fn top_k_replaces_earlier_candidates_and_obeys_memory_and_deadline_guards() {
    let (_dir, db) = setup(true);
    let rows = (0..500)
        .map(|i| {
            Value::Map(BTreeMap::from([
                ("id".into(), Value::Str(format!("source-{i}"))),
                ("authority".into(), Value::Float(i as f64 / 499.0)),
            ]))
        })
        .collect();
    execute(&db, "UNWIND $rows AS r CREATE (s:Source {entity_id:r.id, term:'revenue',authority:r.authority})", &BTreeMap::from([("rows".into(), Value::List(rows))])).unwrap();
    let query = "CALL knowledge.rank($options) YIELD node, score RETURN node.entity_id, score";
    let params = BTreeMap::from([(
        "options".into(),
        Value::Map(BTreeMap::from([
            ("label".into(), Value::Str("Source".into())),
            ("term".into(), Value::Str("revenue".into())),
            ("topK".into(), Value::Int(1)),
        ])),
    )]);
    let limits = ExecutionLimits {
        memory_bytes: 32_768,
        timeout: None,
    };
    let output = execute_with_limits(&db, query, &params, limits).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Str("source-499".into())),
            Out::Value(Value::Float(0.3))
        ]]
    );
    assert!(matches!(
        execute_with_limits(
            &db,
            query,
            &params,
            ExecutionLimits {
                memory_bytes: 1024,
                ..limits
            }
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(matches!(
        execute_with_limits(
            &db,
            query,
            &params,
            ExecutionLimits {
                timeout: Some(std::time::Duration::ZERO),
                ..limits
            }
        ),
        Err(ExecError::TimedOut)
    ));
    run(
        &db,
        "MATCH (s:Source {entity_id:'source-499'}) SET s.term='expenses'",
    );
    assert_eq!(
        execute(&db, query, &params).unwrap().rows[0][0],
        Out::Value(Value::Str("source-498".into()))
    );
}
