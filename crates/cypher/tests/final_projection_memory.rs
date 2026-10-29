use brainwire_core::acl::ReadVisibility;
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryMemoryPool,
    QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;
use std::sync::Arc;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Page","properties":["text","score"]}],"edges":[]}"#,
    )
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

#[test]
fn final_scalar_projection_discards_wide_unreturned_slots_before_retention() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(4096)))]);
    let query = "UNWIND range(0,199) AS i WITH i, $payload AS unused RETURN i AS id";
    let expected = execute(&db, query, &params).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(16 << 20));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 16 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert_eq!(tx.run(query, &params).unwrap(), expected);
    eprintln!(
        "final scalar projection high-water bytes: {}",
        pool.usage().peak_bytes
    );

    let output = execute_with_limits(
        &db,
        query,
        &params,
        ExecutionLimits {
            memory_bytes: 131_072,
            timeout: None,
        },
    )
    .unwrap();
    assert_eq!(output, expected);
    assert_eq!(output.rows.len(), 200);
    assert_eq!(output.rows.first(), Some(&vec![Out::Value(Value::Int(0))]));
    assert_eq!(output.rows.last(), Some(&vec![Out::Value(Value::Int(199))]));
    assert!(pool.usage().peak_bytes <= 131_072);
}

#[test]
fn output_slots_keep_repetition_order_and_union_semantics() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([("payload".into(), Value::Str("wide".repeat(128)))]);
    for query in [
        "WITH $payload AS x RETURN x,x",
        "WITH 2 AS x, 1 AS y RETURN y,x,y",
        "UNWIND [3,1,2] AS x WITH x,$payload AS unused RETURN x ORDER BY x LIMIT 2",
        "UNWIND [3,1,2] AS x WITH x,$payload AS unused RETURN x UNION RETURN 2 AS x UNION RETURN 4 AS x",
        "UNWIND [3,1,2] AS x WITH x,$payload AS unused RETURN DISTINCT x % 2 AS parity ORDER BY parity",
    ] {
        let expected = execute(&db, query, &params).unwrap();
        let actual = execute_with_limits(&db, query, &params, ExecutionLimits {
            memory_bytes: 32_768,
            timeout: None,
        }).unwrap();
        assert_eq!(actual, expected, "{query}");
    }
}

#[test]
fn terminal_compaction_preserves_hidden_sort_keys_and_intermediate_bindings() {
    let (_dir, db) = setup();
    let query = "UNWIND [3,1,2] AS x WITH x, 10-x AS hidden RETURN x ORDER BY hidden";
    assert_eq!(
        execute(&db, query, &BTreeMap::new()).unwrap().rows,
        vec![
            vec![Out::Value(Value::Int(3))],
            vec![Out::Value(Value::Int(2))],
            vec![Out::Value(Value::Int(1))]
        ]
    );
    assert_eq!(
        execute(&db, "WITH 1 AS a,2 AS b RETURN *", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(1)), Out::Value(Value::Int(2))]]
    );
    let payload = Value::Str("x".repeat(4096));
    let params = BTreeMap::from([("payload".into(), payload.clone())]);
    let output = execute(
        &db,
        "WITH $payload AS p WITH p, size(p) AS n WHERE n=4096 RETURN p,p,n",
        &params,
    )
    .unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(payload.clone()),
            Out::Value(payload),
            Out::Value(Value::Int(4096))
        ]]
    );
}

#[test]
fn discarded_working_slots_do_not_bypass_output_limits_or_writes() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([("payload".into(), Value::Str("x".repeat(4096)))]);
    assert!(matches!(
        execute_with_limits(
            &db,
            "UNWIND range(0,199) AS i WITH i,$payload AS payload RETURN payload",
            &params,
            ExecutionLimits {
                memory_bytes: 131_072,
                timeout: None
            }
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
    let output = execute(
        &db,
        "UNWIND range(0,9) AS i CREATE (:Page {entity_id:toString(i),text:'ok'})",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(output.fields.is_empty());
    assert_eq!(
        execute(&db, "MATCH (p:Page) RETURN count(*) AS n", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(10))]]
    );
}

#[test]
fn projected_entity_values_still_respect_acl() {
    let (_dir, db) = setup();
    execute(&db,"CREATE (:Page {entity_id:'visible',text:'public'}),(:Page {entity_id:'hidden',text:'secret'})",&BTreeMap::new()).unwrap();
    execute(&db,"CALL db.setNodeAcl({label:'Page',entityId:'visible',allow:['reader'],deny:[]}) YIELD updated RETURN updated",&BTreeMap::new()).unwrap();
    execute(&db,"CALL db.setNodeAcl({label:'Page',entityId:'hidden',allow:['other'],deny:[]}) YIELD updated RETURN updated",&BTreeMap::new()).unwrap();
    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["reader".into()],
            ..Default::default()
        })
        .unwrap();
    let output = tx
        .run(
            "MATCH (p:Page) WITH p,p.text AS unused RETURN p.entity_id,p.entity_id",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Str("visible".into())),
            Out::Value(Value::Str("visible".into()))
        ]]
    );
}

#[test]
fn repeated_slots_and_union_have_independent_expected_values() {
    let (_dir, db) = setup();
    let output = execute(&db, "WITH 2 AS x,1 AS y RETURN y,x,y", &BTreeMap::new()).unwrap();
    assert_eq!(output.fields, vec!["y", "x", "y"]);
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Int(1)),
            Out::Value(Value::Int(2)),
            Out::Value(Value::Int(1))
        ]]
    );
    let mut output = execute(
        &db,
        "UNWIND [3,1,2] AS x RETURN x UNION RETURN 2 AS x UNION RETURN 4 AS x",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.fields, vec!["x"]);
    output.rows.sort_by_key(|row| match row[0] {
        Out::Value(Value::Int(value)) => value,
        _ => panic!("integer expected"),
    });
    assert_eq!(
        output.rows,
        (1..=4)
            .map(|n| vec![Out::Value(Value::Int(n))])
            .collect::<Vec<_>>()
    );
    let output = execute(
        &db,
        "UNWIND [3,1,2] AS x RETURN x UNION ALL RETURN 2 AS x",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.fields, vec!["x"]);
    assert_eq!(
        output.rows,
        vec![
            vec![Out::Value(Value::Int(3))],
            vec![Out::Value(Value::Int(1))],
            vec![Out::Value(Value::Int(2))],
            vec![Out::Value(Value::Int(2))]
        ]
    );
}
