use std::collections::BTreeMap;

use brainwire_core::value::Value;
use brainwire_cypher::{execute, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
}

#[test]
fn relationship_index_populates_and_tracks_timeline_updates() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    run(&db, "CALL db.createLabel('Item')");
    run(
        &db,
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
    );
    run(&db, "CREATE (:Item {entity_id:'a'})");
    run(&db, "CREATE (:Item {entity_id:'b'})");
    run(&db, "MATCH (a:Item {entity_id:'a'}), (b:Item {entity_id:'b'}) CREATE (a)-[:EVENT {at:'2026-02-02'}]->(b)");
    run(
        &db,
        "CREATE INDEX IF NOT EXISTS FOR ()-[r:EVENT]-() ON (r.at)",
    );
    let status = "CALL db.indexes() YIELD label, property, state, entityType WHERE label = 'EVENT' AND property = 'at' RETURN state, entityType";
    let expected = |state: &str| {
        vec![vec![
            Out::Value(Value::Str(state.into())),
            Out::Value(Value::Str("relationship".into())),
        ]]
    };
    assert_eq!(run(&db, status), expected("populating"));
    db.build_pending_indexes().unwrap();
    assert_eq!(run(&db, status), expected("scalar_range_ready"));
    let timeline =
        "MATCH ()-[r:EVENT]->() WHERE r.at >= '2026-02-01' AND r.at < '2026-03-01' RETURN r.at";
    assert_eq!(
        run(&db, timeline),
        vec![vec![Out::Value(Value::Str("2026-02-02".into()))]]
    );
    run(&db, "MATCH ()-[r:EVENT]->() SET r.at = '2026-03-02'");
    assert!(run(&db, timeline).is_empty());
    drop(db);
    let db = Database::open(dir.path(), &resources).unwrap();
    assert_eq!(run(&db, status), expected("scalar_range_ready"));
    assert!(run(&db, timeline).is_empty());
    brainwire_store::graph::check_invariants(&db).unwrap();
}

#[test]
fn nonfinal_backfill_conflicts_with_delete_detach_and_null_from_older_snapshots() {
    use brainwire_core::ids::EdgeId;
    use brainwire_store::{
        graph,
        txn::{Cf, TxnError},
    };

    for operation in ["delete", "detach", "null"] {
        let dir = tempfile::tempdir().unwrap();
        let resources = Resources::new(StoreConfig::for_tests());
        let mut db = Database::open(dir.path(), &resources).unwrap();
        db.enable_relationship_index_format(&resources).unwrap();
        run(&db, "CALL db.createLabel('Item')");
        run(
            &db,
            "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
        );
        let catalog = db.catalog().unwrap().unwrap();
        let ty = catalog.edge_type("EVENT").unwrap().id;
        let mut seed = db.begin();
        let a = graph::merge_node(&mut seed, &catalog, "Item", "a", &BTreeMap::new()).unwrap();
        let b = graph::merge_node(&mut seed, &catalog, "Item", "b", &BTreeMap::new()).unwrap();
        let c = graph::merge_node(&mut seed, &catalog, "Item", "c", &BTreeMap::new()).unwrap();
        let props = BTreeMap::from([("at".to_string(), Value::Int(10))]);
        graph::merge_edge(&mut seed, &catalog, a, "EVENT", b, &props).unwrap();
        graph::merge_edge(&mut seed, &catalog, b, "EVENT", c, &props).unwrap();
        seed.commit().unwrap();
        run(&db, "CREATE INDEX FOR ()-[r:EVENT]-() ON (r.at)");
        let pending = db.catalog().unwrap().unwrap();
        let mut old = db.begin();
        match operation {
            "delete" => assert!(graph::delete_edge(&mut old, a, ty, b, EdgeId::MERGED).unwrap()),
            "detach" => graph::detach_delete_with_control::<graph::GraphError>(
                &mut old,
                &pending,
                a,
                |_| Ok(()),
            )
            .unwrap(),
            _ => graph::merge_edge(
                &mut old,
                &pending,
                a,
                "EVENT",
                b,
                &BTreeMap::from([("at".to_string(), Value::Null)]),
            )
            .unwrap(),
        };
        assert!(db.index_step(1).unwrap());
        assert_eq!(
            db.catalog().unwrap().unwrap(),
            pending,
            "backfill must not finalize the catalog"
        );
        assert_eq!(
            db.begin()
                .scan_prefix(Cf::RelationshipIdx, &[])
                .unwrap()
                .len(),
            2
        );
        assert!(
            matches!(old.commit(), Err(TxnError::Conflict)),
            "{operation}"
        );
        graph::check_invariants(&db).unwrap();

        match operation {
            "delete" => run(&db, "MATCH (:Item {entity_id:'a'})-[r:EVENT]->() DELETE r"),
            "detach" => run(&db, "MATCH (a:Item {entity_id:'a'}) DETACH DELETE a"),
            _ => run(
                &db,
                "MATCH (:Item {entity_id:'a'})-[r:EVENT]->() SET r.at = null",
            ),
        };
        db.build_pending_indexes().unwrap();
        assert_eq!(
            run(
                &db,
                "MATCH ()-[r:EVENT]->() WHERE r.at = 10 RETURN count(r)"
            ),
            vec![vec![Out::Value(Value::Int(1))]],
            "{operation}"
        );
        assert_eq!(
            db.begin()
                .scan_prefix(Cf::RelationshipIdx, &[])
                .unwrap()
                .len(),
            2
        );
        graph::check_invariants(&db).unwrap();
    }
}

#[test]
fn indexed_numeric_comparisons_remain_exact_and_apply_visibility_to_both_endpoints() {
    use brainwire_core::acl::{NodeAcl, ReadVisibility};
    use brainwire_cypher::QueryTransaction;
    use brainwire_store::graph::{self, set_node_acl};

    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    run(&db, "CALL db.createLabel('Item')");
    run(
        &db,
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
    );
    let catalog = db.catalog().unwrap().unwrap();
    let mut seed = db.begin();
    let visible = graph::merge_node(&mut seed, &catalog, "Item", "a", &BTreeMap::new()).unwrap();
    let hidden = graph::merge_node(&mut seed, &catalog, "Item", "b", &BTreeMap::new()).unwrap();
    let other = graph::merge_node(&mut seed, &catalog, "Item", "c", &BTreeMap::new()).unwrap();
    for (id, principal) in [(visible, "alice"), (other, "alice"), (hidden, "bob")] {
        set_node_acl(
            &mut seed,
            id,
            &NodeAcl {
                allow: vec![principal.into()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    for (from, to, at) in [
        (visible, other, 9007199254740993),
        (visible, visible, 9007199254740992),
        (visible, hidden, 9007199254740993),
        (hidden, visible, 9007199254740993),
    ] {
        graph::merge_edge(
            &mut seed,
            &catalog,
            from,
            "EVENT",
            to,
            &BTreeMap::from([("at".into(), Value::Int(at))]),
        )
        .unwrap();
    }
    seed.commit().unwrap();

    let queries = [
        ("MATCH ()-[r:EVENT]->() WHERE r.at > 9007199254740992 AND r.at <= 9007199254740993 RETURN count(r)", 1),
        ("MATCH ()<-[r:EVENT]-() WHERE 9007199254740992.0 = r.at RETURN count(r)", 1),
        ("MATCH ()-[r:EVENT]-() WHERE r.at = 9007199254740993 RETURN count(r)", 2),
        ("MATCH ()-[r:EVENT]-() WHERE r.at = 9007199254740992 RETURN count(r)", 1),
        ("MATCH ()-[r:EVENT]->() WHERE r.at > 9007199254740993 AND r.at < 9007199254740992 RETURN count(r)", 0),
    ];
    let check = || {
        let mut reader = QueryTransaction::begin(&db)
            .unwrap()
            .with_read_visibility(ReadVisibility {
                principals: vec!["alice".into()],
                ..Default::default()
            })
            .unwrap();
        for (query, count) in queries {
            assert_eq!(
                reader.run(query, &BTreeMap::new()).unwrap().rows,
                vec![vec![Out::Value(Value::Int(count))]],
                "{query}"
            );
        }
    };
    check();
    run(&db, "CREATE INDEX by_at FOR ()-[r:EVENT]-() ON (r.at)");
    check(); // still populating
    db.build_pending_indexes().unwrap();
    check();
    graph::check_invariants(&db).unwrap();
    run(&db, "DROP INDEX by_at");
    check(); // logical drop immediately restores the ordinary scan
    db.build_pending_indexes().unwrap();
    assert!(db
        .begin()
        .scan_prefix(brainwire_store::txn::Cf::RelationshipIdx, &[])
        .unwrap()
        .is_empty());
}

#[test]
fn stale_catalogs_cannot_bypass_index_maintenance() {
    use brainwire_store::{graph, txn::TxnError};
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    run(&db, "CALL db.createLabel('Item')");
    run(
        &db,
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
    );
    let ddl = "CREATE INDEX FOR ()-[r:EVENT]-() ON (r.at)";
    assert!(execute(&db, ddl, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("enable relationship index storage"));
    db.enable_relationship_index_format(&resources).unwrap();
    let stale = db.catalog().unwrap().unwrap();
    let mut seed = db.begin();
    let a = graph::merge_node(&mut seed, &stale, "Item", "a", &BTreeMap::new()).unwrap();
    let b = graph::merge_node(&mut seed, &stale, "Item", "b", &BTreeMap::new()).unwrap();
    seed.commit().unwrap();
    let props = BTreeMap::from([("at".to_owned(), Value::Int(10))]);

    let mut before = db.begin();
    graph::merge_edge(&mut before, &stale, a, "EVENT", b, &props).unwrap();
    run(&db, ddl);
    assert!(matches!(before.commit(), Err(TxnError::Conflict)));
    let mut after = db.begin();
    assert!(graph::merge_edge(&mut after, &stale, a, "EVENT", b, &props)
        .unwrap_err()
        .to_string()
        .contains("reload catalog"));
    let current = db.catalog().unwrap().unwrap();
    graph::merge_edge(&mut after, &current, a, "EVENT", b, &props).unwrap();
    after.commit().unwrap();
    db.build_pending_indexes().unwrap();
    assert_eq!(
        run(
            &db,
            "MATCH ()-[r:EVENT]->() WHERE r.at = 10 RETURN count(r)"
        ),
        vec![vec![Out::Value(Value::Int(1))]]
    );
    graph::check_invariants(&db).unwrap();
}

#[test]
fn checkpoint_resumes_pending_backfill_and_preserves_boundary_comparisons() {
    use brainwire_store::graph;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source");
    let checkpoint = dir.path().join("checkpoint");
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(&path, &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    run(&db, "CALL db.createLabel('Item')");
    run(
        &db,
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
    );
    let catalog = db.catalog().unwrap().unwrap();
    let mut seed = db.begin();
    let source =
        graph::merge_node(&mut seed, &catalog, "Item", "source", &BTreeMap::new()).unwrap();
    let common = "\0".repeat(64 * 1024);
    let before = format!("{common}a");
    let after = format!("{common}b");
    let values = [
        Value::Str(before.clone()),
        Value::Str(after.clone()),
        Value::Str("\0".into()),
        Value::Int(i64::MIN),
        Value::Int(i64::MAX),
        Value::Float(i64::MIN as f64),
        Value::Float(i64::MAX as f64),
        Value::Float(-0.0),
        Value::Float(0.0),
        Value::Int(0),
        Value::Float(f64::NEG_INFINITY),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NAN),
    ];
    for (ordinal, value) in values.into_iter().enumerate() {
        let target = graph::merge_node(
            &mut seed,
            &catalog,
            "Item",
            &ordinal.to_string(),
            &BTreeMap::new(),
        )
        .unwrap();
        graph::merge_edge(
            &mut seed,
            &catalog,
            source,
            "EVENT",
            target,
            &BTreeMap::from([("at".into(), value)]),
        )
        .unwrap();
    }
    seed.commit().unwrap();

    let queries = [
        ("r.at = $key", Value::Str(before.clone()), Value::Null),
        (
            "r.at >= $key AND r.at < $upper",
            Value::Str(before),
            Value::Str(after),
        ),
        ("r.at > $key", Value::Str("\0".into()), Value::Null),
        ("r.at = $key", Value::Float(-0.0), Value::Null),
        ("r.at = $key", Value::Float(f64::INFINITY), Value::Null),
        ("r.at = $key", Value::Float(f64::NAN), Value::Null),
        ("r.at <= $key", Value::Int(i64::MIN), Value::Null),
        (
            "r.at >= $key AND r.at < $upper",
            Value::Int(i64::MAX),
            Value::Float(i64::MAX as f64),
        ),
    ];
    let results = |db: &Database| {
        queries.iter().map(|(pred, key, upper)| {
            execute(db, &format!("MATCH ()-[r:EVENT]->(n) WHERE {pred} RETURN n.entity_id ORDER BY n.entity_id"),
                &BTreeMap::from([("key".into(), key.clone()), ("upper".into(), upper.clone())])).unwrap().rows
        }).collect::<Vec<_>>()
    };
    let baseline = results(&db);
    run(&db, "CREATE INDEX FOR ()-[r:EVENT]-() ON (r.at)");
    assert!(db.index_step(1).unwrap());
    db.checkpoint(&checkpoint).unwrap();
    drop(db);
    for path in [&path, &checkpoint] {
        let restored = Database::open(path, &resources).unwrap();
        assert_eq!(results(&restored), baseline);
        restored.build_pending_indexes().unwrap();
        assert_eq!(results(&restored), baseline);
        graph::check_invariants(&restored).unwrap();
    }
}

#[test]
fn names_are_unique_across_nodes_and_relationships_and_legacy_yield_works() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    run(&db, "CALL db.createLabel('Item')");
    run(
        &db,
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
    );
    run(&db, "CREATE INDEX node_idx FOR (n:Item) ON (n.at)");
    assert!(execute(
        &db,
        "CREATE INDEX node_idx FOR ()-[r:EVENT]-() ON (r.at)",
        &BTreeMap::new()
    )
    .is_err());
    run(&db, "CREATE INDEX rel_idx FOR ()-[r:EVENT]-() ON (r.at)");
    assert!(execute(
        &db,
        "CREATE INDEX rel_idx FOR (n:Item) ON (n.date)",
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(
        run(
            &db,
            "CALL db.indexes() YIELD label, property, state, name RETURN name ORDER BY name"
        ),
        vec![
            vec![Out::Value(Value::Str("node_idx".into()))],
            vec![Out::Value(Value::Str("rel_idx".into()))]
        ]
    );
}

#[test]
fn keyed_parallel_same_value_remain_distinct_before_and_after_build() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    let run = |query| execute(&db, query, &BTreeMap::new()).unwrap().rows;
    run("CALL db.createLabel('Item')");
    run("CALL db.createRelationshipType('EVENT', 'Item', 'Item', ['event_id'])");
    run("CREATE (:Item {entity_id:'a'}), (:Item {entity_id:'b'})");
    let a = "MATCH (a:Item {entity_id:'a'}), (b:Item {entity_id:'b'}) MERGE (a)-[:EVENT {event_id:'first', at:123, seq:502}]->(b)";
    let b = "MATCH (a:Item {entity_id:'a'}), (b:Item {entity_id:'b'}) MERGE (a)-[:EVENT {event_id:'second', at:123, seq:1001}]->(b)";
    run(a);
    let equality = "MATCH ()-[r:EVENT]->() WHERE r.at = 123 RETURN r.seq ORDER BY r.seq";
    let both = vec![
        vec![Out::Value(Value::Int(502))],
        vec![Out::Value(Value::Int(1001))],
    ];
    assert_eq!(run(equality), vec![both[0].clone()]);
    run("CREATE INDEX by_at FOR ()-[r:EVENT]-() ON (r.at)");
    db.build_pending_indexes().unwrap();
    run(b);
    assert_eq!(run(equality), both);
    assert_eq!(
        run("MATCH ()-[r:EVENT]->() WHERE r.at >= 123 AND r.at < 124 RETURN r.seq ORDER BY r.seq"),
        both
    );
    assert_eq!(
        run("MATCH ()-[r:EVENT]->() WHERE coalesce(r.at, 0) = 123 RETURN r.seq ORDER BY r.seq"),
        both
    );
    brainwire_store::graph::check_invariants(&db).unwrap();
}
