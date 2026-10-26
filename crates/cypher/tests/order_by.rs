use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out};
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

fn s(v: &str) -> Out {
    Out::Value(Value::Str(v.into()))
}

fn i(v: i64) -> Out {
    Out::Value(Value::Int(v))
}

#[test]
fn order_skip_and_limit_page_a_label() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([(
        "rows".into(),
        Value::List(
            (0..300)
                .map(|n| {
                    Value::Map(BTreeMap::from([
                        ("id".into(), Value::Str(format!("c{n:03}"))),
                        ("seg".into(), Value::Str(format!("s{}", n % 3))),
                    ]))
                })
                .collect(),
        ),
    )]);
    execute(
        &db,
        "UNWIND $rows AS r MERGE (c:Customer {entity_id: r.id}) SET c.segment = r.seg",
        &params,
    )
    .unwrap();
    // A sorted page from a scan, past a cursor: the top-k is trimmed while walking.
    assert_eq!(
        rows(&db, "MATCH (c:Customer) WHERE c.entity_id > 'c100' RETURN c.entity_id ORDER BY c.entity_id LIMIT 3"),
        vec![vec![s("c101")], vec![s("c102")], vec![s("c103")]]
    );
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) RETURN c.entity_id AS id ORDER BY id DESC SKIP 1 LIMIT 2"
        ),
        vec![vec![s("c298")], vec![s("c297")]]
    );
    // Grouped counts sort by their alias; nulls sort last ascending.
    assert_eq!(
        rows(
            &db,
            "MATCH (c:Customer) RETURN c.segment AS seg, count(*) AS n ORDER BY seg DESC LIMIT 2"
        ),
        vec![vec![s("s2"), i(100)], vec![s("s1"), i(100)]]
    );
    assert_eq!(
        rows(&db, "UNWIND [3, null, 1, 'x'] AS v RETURN v ORDER BY v"),
        vec![
            vec![s("x")],
            vec![i(1)],
            vec![i(3)],
            vec![Out::Value(Value::Null)]
        ]
    );
    assert_eq!(
        rows(&db, "UNWIND [1, 2, 3] AS v RETURN v SKIP 2"),
        vec![vec![i(3)]]
    );
}

#[test]
fn a_sorted_page_of_whole_nodes_projects_only_the_kept_rows() {
    let (_dir, db) = setup();
    let params = BTreeMap::from([(
        "rows".into(),
        Value::List(
            (0..2000)
                .map(|n| {
                    Value::Map(BTreeMap::from([
                        ("id".into(), Value::Str(format!("n{n:04}"))),
                        ("name".into(), Value::Str("x".repeat(200))),
                    ]))
                })
                .collect(),
        ),
    )]);
    execute(
        &db,
        "UNWIND $rows AS r MERGE (c:Customer {entity_id: r.id}) SET c.name = r.name",
        &params,
    )
    .unwrap();
    let out = rows(
        &db,
        "MATCH (c:Customer) WHERE c.entity_id > 'n1000' RETURN c.entity_id AS id, c ORDER BY id LIMIT 2",
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0][0], s("n1001"));
    assert!(
        matches!(&out[1][1], Out::Node(n) if n.props["entity_id"] == Value::Str("n1002".into()))
    );
}
