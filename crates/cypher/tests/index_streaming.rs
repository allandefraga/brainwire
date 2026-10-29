use std::collections::BTreeMap;

use brainwire_core::{
    keys::{put_bytes, put_u16},
    schema::GraphMapping,
    value::Value,
};
use brainwire_cypher::{execute, Out, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::merge_node,
    txn::Cf,
};

fn setup(indexed: bool, values: &[Value]) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let fields = if indexed { "[\"x\"]" } else { "[]" };
    let schema: GraphMapping = serde_json::from_str(&format!(r#"{{"version":"1","nodes":[{{"label":"Item","properties":["x"],"indexed_fields":{fields}}}],"edges":[]}}"#)).unwrap();
    let catalog = db.apply_ontology(&schema).unwrap();
    let mut tx = db.begin();
    for (i, value) in values.iter().enumerate() {
        merge_node(
            &mut tx,
            &catalog,
            "Item",
            &format!("item-{i:04}"),
            &BTreeMap::from([("x".into(), value.clone())]),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    (dir, db)
}

fn rows(db: &Database, query: &str, params: BTreeMap<String, Value>) -> Vec<Vec<Out>> {
    execute(db, query, &params).unwrap().rows
}

#[test]
fn numeric_range_matches_scan_at_exact_integer_boundaries() {
    let values = [
        i64::MIN,
        i64::MIN + 1,
        -9_007_199_254_740_993,
        -1,
        0,
        1,
        9_007_199_254_740_991,
        9_007_199_254_740_992,
        9_007_199_254_740_993,
        i64::MAX - 1,
        i64::MAX,
    ]
    .map(Value::Int);
    let (_a, indexed) = setup(true, &values);
    let (_b, scan) = setup(false, &values);
    for bound in &values {
        for op in [">", ">=", "<", "<="] {
            let query = format!(
                "MATCH (n:Item) WHERE n.x {op} $bound RETURN n.entity_id ORDER BY n.entity_id"
            );
            let params = BTreeMap::from([("bound".into(), bound.clone())]);
            assert_eq!(
                rows(&indexed, &query, params.clone()),
                rows(&scan, &query, params),
                "{query} {bound:?}"
            );
        }
    }
}

#[test]
fn numeric_range_does_not_omit_stored_infinity() {
    let values = [
        Value::Float(f64::NEG_INFINITY),
        Value::Int(0),
        Value::Float(f64::INFINITY),
    ];
    let (_a, indexed) = setup(true, &values);
    let (_b, scan) = setup(false, &values);
    for bound in [
        Value::Int(0),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
    ] {
        for op in [">", ">=", "<", "<="] {
            let query = format!(
                "MATCH (n:Item) WHERE n.x {op} $bound RETURN n.entity_id ORDER BY n.entity_id"
            );
            let params = BTreeMap::from([("bound".into(), bound.clone())]);
            assert_eq!(
                rows(&indexed, &query, params.clone()),
                rows(&scan, &query, params),
                "{query} {bound:?}"
            );
        }
    }
}

#[test]
fn prefix_limit_stops_before_a_later_malformed_synthetic_index_key() {
    let (_dir, db) = setup(true, &[Value::Str("a".into()), Value::Str("b".into())]);
    let ty = db.catalog().unwrap().unwrap().node_type("Item").unwrap().id;
    let mut key = Vec::new();
    put_u16(&mut key, ty.0);
    put_bytes(&mut key, b"x");
    key.extend_from_slice(b"sz");
    assert!(key.len() < 8);
    let mut tx = db.begin();
    tx.put(Cf::Idx, key, vec![]);
    tx.commit().unwrap();
    let query = "MATCH (n:Item) WHERE n.x STARTS WITH '' RETURN n.entity_id LIMIT 1";
    assert_eq!(
        rows(&db, query, BTreeMap::new()),
        vec![vec![Out::Value(Value::Str("item-0000".into()))]]
    );
    assert!(execute(
        &db,
        "MATCH (n:Item) WHERE n.x STARTS WITH '' RETURN n.entity_id",
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn range_and_prefix_keep_snapshot_and_readiness_fallback() {
    let values = [
        Value::Str("alpha".into()),
        Value::Str("beta".into()),
        Value::Str("alphabet".into()),
    ];
    let (_dir, db) = setup(true, &values);
    let mut old = QueryTransaction::begin(&db).unwrap();
    execute(
        &db,
        "MATCH (n:Item {entity_id:'item-0000'}) SET n.x='zeta'",
        &BTreeMap::new(),
    )
    .unwrap();
    let query =
        "MATCH (n:Item) WHERE n.x STARTS WITH 'alpha' RETURN n.entity_id ORDER BY n.entity_id";
    let original = vec![
        vec![Out::Value(Value::Str("item-0000".into()))],
        vec![Out::Value(Value::Str("item-0002".into()))],
    ];
    assert_eq!(old.run(query, &BTreeMap::new()).unwrap().rows, original);
    let expected = vec![vec![Out::Value(Value::Str("item-0002".into()))]];
    assert_eq!(rows(&db, query, BTreeMap::new()), expected);
    let mut tx = db.begin();
    tx.delete(Cf::Meta, b"scalar-index-v2-ready".to_vec());
    tx.commit().unwrap();
    assert_eq!(rows(&db, query, BTreeMap::new()), expected);
    let range = "MATCH (n:Item) WHERE n.x >= 'alpha' AND n.x < 'gamma' RETURN n.entity_id ORDER BY n.entity_id";
    assert_eq!(
        rows(&db, range, BTreeMap::new()),
        vec![
            vec![Out::Value(Value::Str("item-0001".into()))],
            vec![Out::Value(Value::Str("item-0002".into()))]
        ]
    );
}

#[test]
fn equality_and_in_keep_numeric_overlap_and_nonfinite_fallback() {
    let values = [
        Value::Int(i64::MIN),
        Value::Float(i64::MIN as f64),
        Value::Int(0),
        Value::Float(-0.0),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
        Value::Int(i64::MAX),
    ];
    let (_a, indexed) = setup(true, &values);
    let (_b, scan) = setup(false, &values);
    for bound in &values {
        let query = "MATCH (n:Item) WHERE n.x=$bound RETURN n.entity_id ORDER BY n.entity_id";
        let params = BTreeMap::from([("bound".into(), bound.clone())]);
        assert_eq!(
            rows(&indexed, query, params.clone()),
            rows(&scan, query, params),
            "{bound:?}"
        );
    }
    for items in [
        vec![Value::Int(i64::MIN), Value::Float(i64::MIN as f64)],
        vec![Value::Int(0), Value::Float(-0.0), Value::Int(0)],
        values.to_vec(),
    ] {
        let query = "MATCH (n:Item) WHERE n.x IN $items RETURN n.entity_id ORDER BY n.entity_id";
        let params = BTreeMap::from([("items".into(), Value::List(items))]);
        assert_eq!(
            rows(&indexed, query, params.clone()),
            rows(&scan, query, params)
        );
    }
}

#[test]
fn text_range_limit_stops_before_a_later_malformed_synthetic_index_key() {
    let (_dir, db) = setup(true, &[Value::Str("a".into()), Value::Str("b".into())]);
    let ty = db.catalog().unwrap().unwrap().node_type("Item").unwrap().id;
    let mut key = Vec::new();
    put_u16(&mut key, ty.0);
    put_bytes(&mut key, b"x");
    key.extend_from_slice(b"sz");
    assert!(key.len() < 8);
    let mut tx = db.begin();
    tx.put(Cf::Idx, key, vec![]);
    tx.commit().unwrap();
    let query = "MATCH (n:Item) WHERE n.x >= 'a' AND n.x < '{' RETURN n.entity_id LIMIT 1";
    assert_eq!(
        rows(&db, query, BTreeMap::new()),
        vec![vec![Out::Value(Value::Str("item-0000".into()))]]
    );
    assert!(execute(
        &db,
        "MATCH (n:Item) WHERE n.x >= 'a' AND n.x < '{' RETURN n.entity_id",
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn indexed_limit_filters_acl_before_stopping_and_reads_own_overlay() {
    use brainwire_core::acl::{NodeAcl, ReadVisibility};
    use brainwire_store::graph::{find_by_entity_id, set_node_acl};
    let (_dir, db) = setup(
        true,
        &[
            Value::Str("alpha".into()),
            Value::Str("alphabet".into()),
            Value::Str("beta".into()),
        ],
    );
    let ty = db.catalog().unwrap().unwrap().node_type("Item").unwrap().id;
    let mut tx = db.begin();
    for (id, principal) in [
        ("item-0000", "bob"),
        ("item-0001", "alice"),
        ("item-0002", "alice"),
    ] {
        let node = find_by_entity_id(&tx, id, Some(ty)).unwrap()[0];
        set_node_acl(
            &mut tx,
            node,
            &NodeAcl {
                allow: vec![principal.into()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    for predicate in ["n.x STARTS WITH 'alpha'", "n.x >= 'alpha' AND n.x < 'beta'"] {
        assert_eq!(
            tx.run(
                &format!("MATCH (n:Item) WHERE {predicate} RETURN n.entity_id LIMIT 1"),
                &BTreeMap::new()
            )
            .unwrap()
            .rows,
            vec![vec![Out::Value(Value::Str("item-0001".into()))]]
        );
    }
    let mut writer = QueryTransaction::begin(&db).unwrap();
    writer
        .run(
            "MATCH (n:Item {entity_id:'item-0002'}) SET n.x='alpine'",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(writer.run("MATCH (n:Item) WHERE n.x STARTS WITH 'alp' RETURN n.entity_id ORDER BY n.entity_id",&BTreeMap::new()).unwrap().rows.len(),3);
    writer.rollback();
    assert_eq!(
        rows(
            &db,
            "MATCH (n:Item) WHERE n.x STARTS WITH 'alp' RETURN n.entity_id",
            BTreeMap::new()
        )
        .len(),
        2
    );
}

#[test]
fn comparable_non_string_ranges_match_scan_instead_of_silent_empty() {
    let values = [
        Value::Bool(false),
        Value::Bool(true),
        Value::List(vec![Value::Int(1)]),
        Value::List(vec![Value::Int(1), Value::Int(2)]),
        Value::List(vec![Value::Int(2)]),
        Value::Str("a".into()),
    ];
    let (_a, indexed) = setup(true, &values);
    let (_b, scan) = setup(false, &values);
    for bound in &values {
        for op in [">", ">=", "<", "<="] {
            let query = format!(
                "MATCH (n:Item) WHERE n.x {op} $bound RETURN n.entity_id ORDER BY n.entity_id"
            );
            let params = BTreeMap::from([("bound".into(), bound.clone())]);
            assert_eq!(
                rows(&indexed, &query, params.clone()),
                rows(&scan, &query, params),
                "{query} {bound:?}"
            );
        }
    }
    let query = "MATCH (n:Item) WHERE n.x >= $lower AND n.x <= $upper RETURN n.entity_id";
    let params = BTreeMap::from([
        ("lower".into(), Value::Bool(false)),
        ("upper".into(), Value::Str("z".into())),
    ]);
    assert_eq!(
        rows(&indexed, query, params.clone()),
        rows(&scan, query, params)
    );
}
