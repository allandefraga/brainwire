use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::check_invariants,
};
use std::collections::BTreeMap;

fn db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}
fn run(db: &Database, q: &str) -> brainwire_cypher::QueryOutput {
    execute(db, q, &BTreeMap::new()).unwrap()
}
fn values(db: &Database, q: &str) -> Vec<Vec<Out>> {
    run(db, q).rows
}
fn s(s: &str) -> Out {
    Out::Value(Value::Str(s.into()))
}
fn i(i: i64) -> Out {
    Out::Value(Value::Int(i))
}

#[test]
fn create_merge_set_node_and_null() {
    let (_dir, db) = db();
    let create = "CREATE (c:Customer {entity_id: 'c1', name: 'Ana'})";
    assert!(run(&db, create).rows.is_empty());
    assert!(matches!(
        execute(&db, create, &BTreeMap::new()),
        Err(ExecError::ConstraintViolation(_))
    ));
    let out = run(&db, "MATCH (c:Customer {entity_id: 'c1'}) RETURN c.name");
    assert_eq!(out.fields, ["c.name"]);
    assert_eq!(out.rows, vec![vec![s("Ana")]]);
    for name in ["Bob", "Last"] {
        execute(
            &db,
            "MERGE (c:Customer {entity_id: $id}) SET c.name = $name",
            &BTreeMap::from([
                ("id".into(), Value::Str("c1".into())),
                ("name".into(), Value::Str(name.into())),
            ]),
        )
        .unwrap();
    }
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![i(1)]]
    );
    let Out::Node(node) = &values(&db, "MATCH (c:Customer) RETURN c")[0][0] else {
        panic!()
    };
    assert_eq!(node.label, "Customer");
    assert_eq!(node.props["entity_id"], Value::Str("c1".into()));
    assert_eq!(node.props["name"], Value::Str("Last".into()));
    assert_eq!(
        values(&db, "MATCH (c:Customer) SET c.name = null RETURN c.name"),
        vec![vec![Out::Value(Value::Null)]]
    );
}

#[test]
fn hops_and_detach_delete() {
    let (_dir, db) = db();
    run(&db, "CREATE (c:Customer {entity_id: 'c1', name: 'Ana'}), (k:Company {entity_id: 'k1', name: 'Acme'})");
    run(
        &db,
        "MATCH (c:Customer), (k:Company) MERGE (c)-[:WORKS_AT]->(k)",
    );
    let out = run(
        &db,
        "MATCH (c:Customer)-[:WORKS_AT]->(k:Company) RETURN c.name, k.name AS company",
    );
    assert_eq!(out.fields, ["c.name", "company"]);
    assert_eq!(out.rows, vec![vec![s("Ana"), s("Acme")]]);
    assert_eq!(
        values(
            &db,
            "MATCH (k:Company)<-[:WORKS_AT]-(c:Customer) RETURN c.entity_id"
        ),
        vec![vec![s("c1")]]
    );
    assert!(values(&db, "MATCH (c:Customer)-[:WORKS_AT]->(c:Company) RETURN c").is_empty());
    run(&db, "MATCH (c:Customer) DETACH DELETE c");
    check_invariants(&db).unwrap();
    assert!(values(&db, "MATCH (c:Customer)-[:WORKS_AT]->(k:Company) RETURN k").is_empty());
}

#[test]
fn bulk_unwind_and_reopen() {
    let (dir, db) = db();
    let rows = (0..1000)
        .map(|n| {
            Value::Map(BTreeMap::from([
                ("id".into(), Value::Str(format!("c{n}"))),
                ("name".into(), Value::Str(format!("name-{n}"))),
            ]))
        })
        .collect();
    execute(
        &db,
        "UNWIND $rows AS row MERGE (c:Customer {entity_id: row.id}) SET c.name = row.name",
        &BTreeMap::from([("rows".into(), Value::List(rows))]),
    )
    .unwrap();
    drop(db);
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(
        values(
            &db,
            "MATCH (c:Customer) WHERE c.name = 'name-500' RETURN c.entity_id"
        ),
        vec![vec![s("c500")]]
    );
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![i(1000)]]
    );
}

#[test]
fn static_errors_survive_empty_input() {
    let (_dir, db) = db();
    for q in [
        "MATCH (x:Nope) RETURN x",
        "UNWIND [] AS x RETURN unknown",
        "MATCH (c:Customer) SET c.entity_id = 'x'",
        "RETURN 1 MATCH (c:Customer) RETURN c",
        "MATCH (c:Customer) WHERE count(*) = 0 RETURN c",
        "RETURN count(count(*))",
        "UNWIND [] AS x RETURN x LIMIT x",
        "MATCH (c:Customer)-[:NOPE]->(k:Company) RETURN k",
    ] {
        assert!(
            matches!(
                execute(&db, q, &BTreeMap::new()),
                Err(ExecError::Semantic(_)) | Err(ExecError::Unsupported(_))
            ),
            "{q}"
        );
    }
    for q in [
        "RETURN $missing",
        "UNWIND [] AS x RETURN $missing",
        "MATCH (c:Customer {name: $missing}) RETURN c",
    ] {
        assert!(
            matches!(
                execute(&db, q, &BTreeMap::new()),
                Err(ExecError::ParameterMissing(_))
            ),
            "{q}"
        );
    }
    // Unlabeled scans, relationship property filters and nodes without an
    // explicit entity_id are supported.
    for q in [
        "MATCH (c) RETURN c",
        "MATCH (c:Customer)-[:WORKS_AT {role: 'x'}]->(k:Company) RETURN c",
        "CREATE (c:Customer)",
    ] {
        assert!(execute(&db, q, &BTreeMap::new()).is_ok(), "{q}");
    }
    assert!(matches!(
        execute(&db, "RETURN (", &BTreeMap::new()),
        Err(ExecError::Syntax(_))
    ));
}

#[test]
fn rollback_late_runtime_errors_and_conflict_mapping() {
    let (_dir, db) = db();
    for suffix in [
        "UNWIND [1, 0] AS x RETURN 1 / x",
        "RETURN 1 AND true",
        "RETURN 1 LIMIT -1",
        "CREATE (d:Customer {entity_id: 'c1'})",
    ] {
        let q = format!("CREATE (c:Customer {{entity_id: 'c1'}}) {suffix}");
        assert!(execute(&db, &q, &BTreeMap::new()).is_err(), "{q}");
        assert_eq!(
            values(&db, "MATCH (c:Customer) RETURN count(*)"),
            vec![vec![i(0)]]
        );
    }
    assert!(matches!(
        ExecError::from(brainwire_store::txn::TxnError::Conflict),
        ExecError::Conflict
    ));
}

#[test]
fn grouping_empty_aggregation_null_and_numeric_precision() {
    let (_dir, db) = db();
    assert_eq!(
        values(&db, "UNWIND [] AS x RETURN count(*), count(x)"),
        vec![vec![i(0), i(0)]]
    );
    assert!(values(&db, "UNWIND [] AS x RETURN x, count(*)").is_empty());
    assert_eq!(
        values(
            &db,
            "UNWIND [1, 1, 2, null] AS x RETURN x, count(*), count(x)"
        ),
        vec![
            vec![i(1), i(2), i(2)],
            vec![i(2), i(1), i(1)],
            vec![Out::Value(Value::Null), i(1), i(0)]
        ]
    );
    assert_eq!(values(&db, "RETURN 9007199254740993 = 9007199254740992.0, 9223372036854775807 < 9223372036854775808.0, null AND false, null OR true, NOT null"), vec![vec![Out::Value(Value::Bool(false)), Out::Value(Value::Bool(true)), Out::Value(Value::Bool(false)), Out::Value(Value::Bool(true)), Out::Value(Value::Null)]]);
    assert!(values(&db, "RETURN 1 LIMIT 0").is_empty());
}

#[test]
fn literal_query_without_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    assert_eq!(values(&db, "RETURN 1"), vec![vec![i(1)]]);
}

#[test]
fn mixed_comparisons_and_empty_boolean_operand_validation() {
    let (_dir, db) = db();
    assert_eq!(
        values(&db, "RETURN 1 <> '1', 1 = '1', 1 < '1'"),
        vec![vec![
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(false)),
            Out::Value(Value::Null)
        ]]
    );
    for q in [
        "MATCH (c:Customer) WHERE c.name AND 1 RETURN c",
        "UNWIND [] AS x RETURN x OR 42",
        "UNWIND [] AS x RETURN NOT 42",
        "MATCH (c:Customer) WHERE 42 RETURN c",
    ] {
        assert!(
            matches!(
                execute(&db, q, &BTreeMap::new()),
                Err(ExecError::Semantic(_))
            ),
            "{q}"
        );
    }
}

#[test]
fn repeated_bindings_cartesian_product_and_runtime_rollback() {
    let (_dir, db) = db();
    run(
        &db,
        "CREATE (a:Customer {entity_id: 'a'}), (b:Customer {entity_id: 'b'})",
    );
    assert_eq!(
        values(&db, "MATCH (a:Customer), (b:Customer) RETURN count(*)"),
        vec![vec![i(4)]]
    );
    assert_eq!(
        values(&db, "MATCH (a:Customer), (a:Customer) RETURN count(*)"),
        vec![vec![i(2)]]
    );
    assert!(values(
        &db,
        "MATCH (a:Customer {entity_id: 'a'}), (a:Customer {entity_id: 'b'}) RETURN a"
    )
    .is_empty());
    assert!(matches!(
        execute(
            &db,
            "MATCH (a:Customer) CREATE (a:Customer {entity_id: 'c'})",
            &BTreeMap::new()
        ),
        Err(ExecError::Unsupported(_))
    ));
    assert!(execute(
        &db,
        "UNWIND ['new', 42] AS id CREATE (c:Customer {entity_id: id})",
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![i(2)]]
    );
}

#[test]
fn numeric_groups_and_three_valued_logic() {
    let (_dir, db) = db();
    assert_eq!(
        values(
            &db,
            "UNWIND [1, 1.0, 9007199254740993, 9007199254740992.0] AS x RETURN x, count(*)"
        ),
        vec![
            vec![i(1), i(2)],
            vec![i(9007199254740993), i(1)],
            vec![Out::Value(Value::Float(9007199254740992.0)), i(1)]
        ]
    );
    assert_eq!(
        values(
            &db,
            "RETURN null AND true, false OR null, NOT false, -1 > -1.5"
        ),
        vec![vec![
            Out::Value(Value::Null),
            Out::Value(Value::Null),
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(true))
        ]]
    );
}

#[test]
fn integer_min_and_nonfinite_parameter_comparison() {
    let (_dir, db) = db();
    let params = BTreeMap::from([
        ("min".into(), Value::Int(i64::MIN)),
        ("inf".into(), Value::Float(f64::INFINITY)),
    ]);
    assert_eq!(
        execute(
            &db,
            "RETURN $min = -9223372036854775808.0, $min < $inf",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![
            Out::Value(Value::Bool(true)),
            Out::Value(Value::Bool(true))
        ]]
    );
}

#[test]
fn self_loops_bound_endpoints_and_duplicate_delete_rows() {
    let (_dir, db) = db();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version": "2", "nodes": [{"label": "Customer"}],
        "edges": [{"type":"PEER", "from":{"label":"Customer"},"to":{"label":"Customer"},"properties":["role"]}]
    })).unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db,"CREATE (a:Customer {entity_id: 'a'}), (b:Customer {entity_id: 'b'}) CREATE (a)-[:PEER {role: 'self'}]->(a) MERGE (a)-[:PEER]->(b)");
    assert_eq!(
        values(
            &db,
            "MATCH (a:Customer)-[:PEER]->(a:Customer) RETURN a.entity_id"
        ),
        vec![vec![s("a")]]
    );
    assert_eq!(values(&db,"MATCH (a:Customer {entity_id: 'a'}), (b:Customer {entity_id: 'b'}) MATCH (b:Customer)<-[:PEER]-(a:Customer) RETURN a.entity_id, b.entity_id"),vec![vec![s("a"),s("b")]]);
    run(&db, "MATCH (a:Customer), (b:Customer) DETACH DELETE a, b");
    check_invariants(&db).unwrap();
    assert_eq!(
        values(&db, "MATCH (a:Customer) RETURN count(*)"),
        vec![vec![i(0)]]
    );
}

#[test]
fn flexible_properties_count_nodes_and_parameter_limit() {
    let (_dir, db) = db();
    run(&db, "CREATE (c:Customer {entity_id: 'c', custom: 7})");
    assert_eq!(
        values(
            &db,
            "MATCH (c:Customer {custom: 7.0}) RETURN c.custom, count(c), count(c.missing)"
        ),
        vec![vec![i(7), i(1), i(0)]]
    );
    assert_eq!(
        execute(
            &db,
            "MATCH (c:Customer) RETURN c LIMIT $limit",
            &BTreeMap::from([("limit".into(), Value::Int(0))])
        )
        .unwrap()
        .rows,
        Vec::<Vec<Out>>::new()
    );
    run(&db, "MATCH (c:Customer) SET c.custom = null");
    let Out::Node(node) = &values(&db, "MATCH (c:Customer) RETURN c")[0][0] else {
        panic!()
    };
    assert!(!node.props.contains_key("custom"));
}

#[test]
fn real_store_conflict_converts_to_public_executor_error() {
    use brainwire_store::graph::merge_node;
    let (_dir, db) = db();
    let catalog = db.catalog().unwrap().unwrap();
    let mut first = db.begin();
    let mut second = db.begin();
    merge_node(&mut first, &catalog, "Customer", "same", &BTreeMap::new()).unwrap();
    merge_node(&mut second, &catalog, "Customer", "same", &BTreeMap::new()).unwrap();
    first.commit().unwrap();
    let error = second.commit().map_err(ExecError::from).unwrap_err();
    assert!(matches!(error, ExecError::Conflict));
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![i(1)]]
    );
}

#[test]
fn signed_minimum_literal_executes_and_persists_exactly() {
    let (_dir, db) = db();
    assert_eq!(
        values(
            &db,
            "RETURN -9223372036854775808, - 0009223372036854775808 = -9223372036854775808.0"
        ),
        vec![vec![i(i64::MIN), Out::Value(Value::Bool(true))]]
    );
    run(
        &db,
        "CREATE (c:Customer {entity_id: 'min', exact: -9223372036854775808})",
    );
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN c.exact"),
        vec![vec![i(i64::MIN)]]
    );
}

#[test]
fn overflowing_float_literals_fail_without_writes() {
    let (_dir, db) = db();
    for literal in ["1e309", "-1e309"] {
        let query =
            format!("CREATE (c:Customer {{entity_id: 'overflow', amount: {literal}}}) RETURN c");
        assert!(matches!(
            execute(&db, &query, &BTreeMap::new()),
            Err(ExecError::Syntax(_))
        ));
    }
    assert_eq!(
        values(&db, "MATCH (c:Customer) RETURN count(*)"),
        vec![vec![i(0)]]
    );
    assert_eq!(
        values(&db, "RETURN 1e308"),
        vec![vec![Out::Value(Value::Float(1e308))]]
    );
}
