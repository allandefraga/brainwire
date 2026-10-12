use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, parse, ExecError, ExecutionLimits, Out, QueryMemoryPool,
    QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

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
fn string(value: &str) -> Out {
    Out::Value(Value::Str(value.into()))
}
fn int(value: i64) -> Out {
    Out::Value(Value::Int(value))
}

#[test]
fn unicode_null_and_nested_functions() {
    let (_dir, db) = setup();
    assert_eq!(rows(&db, "RETURN ToLoWeR('AbC Éİ ΟΣ'), TOUPPER('aBc éß σς'), toLower(null), toUpper(null), size(toUpper('ß')), toLower(coalesce(null, 'OPEN'))"), vec![vec![string("abc éi\u{307} ος"), string("ABC ÉSS ΣΣ"), Out::Value(Value::Null), Out::Value(Value::Null), int(2), string("open")]]);
    assert_eq!(
        rows(&db, "UNWIND ['A', null, 'B'] AS x RETURN count(toLower(x))"),
        vec![vec![int(2)]]
    );
    rows(&db, "CREATE (c:Customer {entity_id: 'one'})");
    assert_eq!(
        rows(&db, "MATCH (c:Customer) RETURN toLower(c.missing)"),
        vec![vec![Out::Value(Value::Null)]]
    );
}

#[test]
fn arity_types_names_and_zero_rows_are_validated() {
    let (_dir, db) = setup();
    for query in [
        "RETURN toLower()",
        "RETURN toUpper('a', 'b')",
        "RETURN toLower(*)",
    ] {
        assert!(parse(query).is_err(), "{query}");
    }
    for query in [
        "RETURN toLower(1)",
        "RETURN toUpper(1.5)",
        "RETURN toLower(true)",
        "RETURN toLower([])",
        "RETURN toUpper({a: 1})",
        "UNWIND [] AS x RETURN toLower(1)",
        "RETURN toUpper(false) LIMIT 0",
        "UNWIND [] AS x RETURN toLower(unknown)",
        "MATCH (c:Customer) RETURN toLower(c)",
    ] {
        if query == "MATCH (c:Customer) RETURN toLower(c)" {
            rows(&db, "CREATE (c:Customer {entity_id: 'node'})");
        }
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{query}"
        );
    }
    for query in [
        "RETURN toLower(count(*))",
        "RETURN count(toUpper(count(*)))",
    ] {
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Unsupported(_))
            ),
            "{query}"
        );
    }
}

#[test]
fn indexed_set_overwrite_and_reopen() {
    let (dir, db) = setup();
    rows(
        &db,
        "CREATE (c:Customer {entity_id: 'one', email: 'UPPER@EXAMPLE.COM'})",
    );
    rows(&db, "MATCH (c:Customer) SET c.email = toLower(c.email)");
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer {email: 'UPPER@EXAMPLE.COM'}) RETURN count(*)"
        ),
        vec![vec![int(0)]]
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer {email: 'upper@example.com'}) RETURN c.email"
        ),
        vec![vec![string("upper@example.com")]]
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) WHERE toUpper(c.email) = 'UPPER@EXAMPLE.COM' RETURN count(*)"
        ),
        vec![vec![int(1)]]
    );
    drop(db);
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer {email: 'upper@example.com'}) RETURN c.email"
        ),
        vec![vec![string("upper@example.com")]]
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer {email: 'UPPER@EXAMPLE.COM'}) RETURN count(*)"
        ),
        vec![vec![int(0)]]
    );
}

#[test]
fn later_row_type_error_rolls_back_all_writes() {
    let (_dir, db) = setup();
    assert!(matches!(execute(&db, "UNWIND [{id: 'one', name: 'OK'}, {id: 'two', name: 42}] AS x CREATE (c:Customer {entity_id: x.id}) SET c.name = toLower(x.name)", &BTreeMap::new()), Err(ExecError::Semantic(_))));
    assert_eq!(
        rows(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![int(0)]]
    );
}

#[test]
fn conversion_allocation_is_guarded() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([("text".into(), Value::Str("ß".repeat(5000)))]);
    let limits = ExecutionLimits {
        memory_bytes: 200_000,
        timeout: None,
    };
    assert!(execute_with_limits(&db, "RETURN $text", &params, limits).is_ok());
    assert!(matches!(
        execute_with_limits(&db, "RETURN toUpper($text)", &params, limits),
        Err(ExecError::MemoryLimit { .. })
    ));
}

#[test]
fn conversion_uses_shared_pool_and_deadline() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([("text".into(), Value::Str("ß".repeat(5000)))]);
    let pool = Arc::new(QueryMemoryPool::new(200_000));
    let mut tx =
        QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
            .unwrap();
    assert!(matches!(
        tx.run("RETURN toUpper($text)", &params),
        Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: Some(std::time::Duration::ZERO),
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert!(matches!(
        tx.run("RETURN toLower($text)", &params),
        Err(ExecError::TimedOut)
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
}
