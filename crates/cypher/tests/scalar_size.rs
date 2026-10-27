use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, parse, ExecError, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

fn db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}

#[test]
fn sizes_strings_lists_null_nested_values_and_parameters() {
    let (_dir, db) = db();
    let params = BTreeMap::from([(
        "items".into(),
        Value::List(vec![Value::Null, Value::Int(1)]),
    )]);
    let output = execute(&db, "RETURN size('é🧠'), size('é'), size(''), size([1, null, [2]]), size([]), size(null), size([size('abc')]), SiZe($items)", &params).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![
            Out::Value(Value::Int(2)),
            Out::Value(Value::Int(2)),
            Out::Value(Value::Int(0)),
            Out::Value(Value::Int(3)),
            Out::Value(Value::Int(0)),
            Out::Value(Value::Null),
            Out::Value(Value::Int(1)),
            Out::Value(Value::Int(2)),
        ]]
    );
}

#[test]
fn property_predicates_unwind_and_count_arguments_use_size() {
    let (_dir, db) = db();
    execute(
        &db,
        "CREATE (c:Customer {entity_id: 'c1', name: 'é🧠'})",
        &BTreeMap::new(),
    )
    .unwrap();
    let output = execute(
        &db,
        "MATCH (c:Customer) WHERE size(c.name) = 2 RETURN size(c.name), size(c.email)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        output.rows,
        vec![vec![Out::Value(Value::Int(2)), Out::Value(Value::Null)]]
    );
    let output = execute(
        &db,
        "UNWIND ['a', 'é🧠', null] AS item RETURN size(item)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        output.rows,
        vec![
            vec![Out::Value(Value::Int(1))],
            vec![Out::Value(Value::Int(2))],
            vec![Out::Value(Value::Null)]
        ]
    );
    let output = execute(
        &db,
        "UNWIND ['a', null] AS item RETURN count(size(item))",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Int(1))]]);
    let grouped = execute(
        &db,
        "UNWIND ['a', 'b', 'cc', null] AS x RETURN size(x), count(*)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(grouped.rows.len(), 3);
    for expected in [
        vec![Out::Value(Value::Int(1)), Out::Value(Value::Int(2))],
        vec![Out::Value(Value::Int(2)), Out::Value(Value::Int(1))],
        vec![Out::Value(Value::Null), Out::Value(Value::Int(1))],
    ] {
        assert!(grouped.rows.contains(&expected));
    }
}

#[test]
fn rejects_invalid_types_names_arity_and_nested_aggregates() {
    let (_dir, db) = db();
    for query in [
        "RETURN size(1)",
        "RETURN size({a: 1})",
        "RETURN size(true)",
        "RETURN size(size('a'))",
        "RETURN size(unknown)",
        "MATCH (c:Customer) RETURN size(unknown)",
    ] {
        assert!(
            matches!(
                execute(&db, query, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{query}"
        );
    }
    assert!(matches!(
        execute(&db, "RETURN size($missing)", &BTreeMap::new()),
        Err(ExecError::ParameterMissing(_))
    ));
    execute(
        &db,
        "CREATE (c:Customer {entity_id: 'c1'})",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(matches!(
        execute(&db, "MATCH (c:Customer) RETURN size(c)", &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    for query in [
        "RETURN size()",
        "RETURN size(1, 2)",
        "RETURN size(*)",
        "RETURN foo('a')",
    ] {
        assert!(parse(query).is_err(), "{query}");
    }
    // An aggregate may sit inside a function, but aggregates cannot nest.
    assert_eq!(
        execute(&db, "RETURN size(collect(1))", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(1))]]
    );
    assert!(execute(&db, "RETURN count(size(count(*)))", &BTreeMap::new()).is_err());
    let deep = format!("RETURN {}null{}", "size(".repeat(100), ")".repeat(100));
    assert!(parse(&deep).is_err());
}

#[test]
fn size_in_set_persists_and_an_invalid_argument_rolls_back_create() {
    let (_dir, db) = db();
    execute(
        &db,
        "CREATE (c:Customer {entity_id: 'valid'}) SET c.components = size([1, 2, 3])",
        &BTreeMap::new(),
    )
    .unwrap();
    let output = execute(
        &db,
        "MATCH (c:Customer {entity_id: 'valid'}) RETURN c.components",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Int(3))]]);
    assert!(matches!(
        execute(
            &db,
            "CREATE (c:Customer {entity_id: 'invalid'}) SET c.components = size(42)",
            &BTreeMap::new(),
        ),
        Err(ExecError::Semantic(_)),
    ));
    // A node property is row-dependent: this error happens during execution,
    // after CREATE has staged the node, rather than during constant validation.
    assert!(matches!(
        execute(
            &db,
            "CREATE (c:Customer {entity_id: 'runtime-invalid', components: 42}) SET c.components = size(c.components)",
            &BTreeMap::new(),
        ),
        Err(ExecError::Semantic(_)),
    ));
    let absent = execute(
        &db,
        "MATCH (c:Customer {entity_id: 'runtime-invalid'}) RETURN count(*)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(absent.rows, vec![vec![Out::Value(Value::Int(0))]]);
    let output = execute(
        &db,
        "MATCH (c:Customer {entity_id: 'invalid'}) RETURN count(*)",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![Out::Value(Value::Int(0))]]);
}
