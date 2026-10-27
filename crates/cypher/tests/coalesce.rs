use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, execute_with_limits, parse, ExecError, ExecutionLimits, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

fn rows(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}

fn int(value: i64) -> Out {
    Out::Value(Value::Int(value))
}

#[test]
fn first_nonnull_is_lazy_and_accepts_values_and_parameters() {
    let (_dir, db) = setup();
    assert_eq!(
        rows(&db, "RETURN coalesce(null), COALESCE(null, 0, $missing), coalesce(false, size(42)), coalesce(null, [1, null]), coalesce(null, {a: 1}), coalesce(null, coalesce(null, 7))"),
        vec![vec![
            Out::Value(Value::Null),
            int(0),
            Out::Value(Value::Bool(false)),
            Out::Value(Value::List(vec![Value::Int(1), Value::Null])),
            Out::Value(Value::Map(BTreeMap::from([("a".into(), Value::Int(1))]))),
            int(7),
        ]]
    );
    let params = BTreeMap::from([("priority".into(), Value::Null)]);
    assert_eq!(
        execute(&db, "RETURN coalesce($priority, 'normal')", &params)
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Str("normal".into()))]]
    );
    assert!(matches!(
        execute(&db, "RETURN coalesce(null, $missing)", &BTreeMap::new()),
        Err(ExecError::ParameterMissing(_))
    ));
}

#[test]
fn names_and_aggregate_shapes_are_validated_even_in_skipped_arguments() {
    let (_dir, db) = setup();
    for query in [
        "RETURN coalesce(1, unknown)",
        "UNWIND [] AS x RETURN coalesce(1, unknown)",
        "RETURN coalesce(1, [unknown])",
    ] {
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{query}"
        );
    }
    // An aggregate may sit inside an expression, but not inside another aggregate.
    assert_eq!(
        rows(&db, "RETURN coalesce(1, count(*))"),
        vec![vec![int(1)]]
    );
    assert!(execute(&db, "RETURN count(coalesce(1, count(*)))", &BTreeMap::new()).is_err());
    for query in [
        "RETURN coalesce()",
        "RETURN coalesce(*)",
        "RETURN coalesce(1,)",
    ] {
        assert!(parse(query).is_err(), "{query}");
    }
    let deep = format!("RETURN {}null{}", "coalesce(".repeat(100), ")".repeat(100));
    assert!(parse(&deep).is_err());
}

#[test]
fn per_row_defaults_work_in_unwind_predicates_and_exact_counts() {
    let (_dir, db) = setup();
    assert_eq!(
        rows(
            &db,
            "UNWIND [null, 1, null, 2] AS x RETURN count(coalesce(x, 0)), count(coalesce(x, null))"
        ),
        vec![vec![int(4), int(2)]]
    );
    assert_eq!(
        rows(
            &db,
            "UNWIND coalesce(null, [null, 2]) AS x RETURN coalesce(x, 9)"
        ),
        vec![vec![int(9)], vec![int(2)]]
    );
    rows(
        &db,
        "CREATE (c:Customer {entity_id: 'one'}) SET c.priority = coalesce(c.priority, 'normal')",
    );
    assert_eq!(
        rows(&db, "MATCH (c:Customer) WHERE coalesce(c.priority, 'low') = 'normal' RETURN coalesce(c.priority, 'low')"),
        vec![vec![Out::Value(Value::Str("normal".into()))]]
    );
    assert!(matches!(
        rows(&db, "MATCH (c:Customer) RETURN coalesce(c, null)")[0][0],
        Out::Node(_)
    ));
}

#[test]
fn later_row_error_rolls_back_writes_and_memory_guards_remain_active() {
    let (_dir, db) = setup();
    assert!(matches!(
        execute(&db, "UNWIND [{id: 'one', priority: 'high'}, {id: 'two', priority: null}] AS x CREATE (c:Customer {entity_id: x.id}) SET c.priority = coalesce(x.priority, size(42))", &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    assert_eq!(
        rows(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![int(0)]]
    );
    let params = BTreeMap::from([("large".into(), Value::Str("x".repeat(10_000)))]);
    assert!(matches!(
        execute_with_limits(
            &db,
            "RETURN coalesce(null, $large)",
            &params,
            ExecutionLimits {
                memory_bytes: 1024,
                timeout: None
            }
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
}
