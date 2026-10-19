use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryAccess, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::check_invariants,
    txn::Cf,
};
use std::collections::BTreeMap;

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Page"}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}},{"type":"OTHER","from":{"label":"Page"},"to":{"label":"Page"}}]}"#).unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(
        &db,
        "CREATE (a:Page {entity_id:'a'}), (b:Page {entity_id:'b'}), (c:Page {entity_id:'c'})",
    );
    for (a, t, b) in [
        ("a", "LINK", "b"),
        ("b", "LINK", "a"),
        ("a", "LINK", "a"),
        ("a", "LINK", "c"),
        ("a", "OTHER", "b"),
    ] {
        run(&db,&format!("MATCH (a:Page {{entity_id:'{a}'}}), (b:Page {{entity_id:'{b}'}}) MERGE (a)-[:{t}]->(b)"));
    }
    (dir, db)
}
fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}
fn count(n: i64) -> Vec<Vec<Out>> {
    vec![vec![Out::Value(Value::Int(n))]]
}
const DELETE: &str =
    "MATCH (a:Page {entity_id:'a'})-[r:LINK]->(b:Page {entity_id:'b'}) DELETE r RETURN count(*)";

#[test]
fn relationship_delete_preserves_nodes_orientation_type_and_other_endpoints() {
    let (_dir, db) = setup();
    assert_eq!(run(&db, DELETE), count(1));
    assert_eq!(run(&db, DELETE), count(0));
    assert_eq!(run(&db, "MATCH (p:Page) RETURN count(*)"), count(3));
    assert_eq!(
        run(&db, "MATCH (a:Page)-[:LINK]->(b:Page) RETURN count(*)"),
        count(3)
    );
    assert_eq!(
        run(&db, "MATCH (a:Page)-[:OTHER]->(b:Page) RETURN count(*)"),
        count(1)
    );
    assert_eq!(run(&db,"MATCH (a:Page {entity_id:'a'})<-[r:LINK]-(b:Page {entity_id:'b'}) DELETE r RETURN count(*)"),count(1));
    assert_eq!(
        run(
            &db,
            "MATCH (a:Page)-[r:LINK]->(a:Page) DELETE r RETURN count(*)"
        ),
        count(1)
    );
    let txn = db.begin();
    assert_eq!(txn.scan_prefix(Cf::Out, &[]).unwrap().len(), 2);
    assert_eq!(txn.scan_prefix(Cf::In, &[]).unwrap().len(), 2);
    check_invariants(&db).unwrap();
}

#[test]
fn relationship_delete_rolls_back_and_cannot_be_used_by_reader_or_for_nodes() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    assert!(tx
        .run_with_access(
            DELETE,
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false
            }
        )
        .is_err());
    tx.rollback();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run(DELETE, &BTreeMap::new()).unwrap();
    tx.rollback();
    assert_eq!(run(&db, DELETE), count(1));
    assert!(execute(&db, "MATCH (a:Page) DELETE a", &BTreeMap::new()).is_err());
    assert!(execute(
        &db,
        "MATCH (a:Page {entity_id:'missing'}) DELETE absent",
        &BTreeMap::new()
    )
    .is_err());
}

#[test]
fn deletes_all_physical_relationships_and_counts_once_per_matched_edge() {
    let (_dir, db) = setup();
    let mut txn = db.begin();
    for cf in [Cf::Out, Cf::In] {
        for (mut key, value) in txn.scan_prefix(cf, &[]).unwrap() {
            let mut out = key.clone();
            if cf == Cf::In {
                out[..8].copy_from_slice(&key[10..18]);
                out[10..18].copy_from_slice(&key[..8]);
            }
            let source =
                brainwire_core::ids::NodeId(u64::from_be_bytes(out[..8].try_into().unwrap()));
            let target =
                brainwire_core::ids::NodeId(u64::from_be_bytes(out[10..18].try_into().unwrap()));
            let a = brainwire_store::graph::find_by_entity_id(&txn, "a", None).unwrap()[0];
            let b = brainwire_store::graph::find_by_entity_id(&txn, "b", None).unwrap()[0];
            let kind = db.catalog().unwrap().unwrap().edge_type("LINK").unwrap().id;
            if source == a
                && target == b
                && u16::from_be_bytes(out[8..10].try_into().unwrap()) == kind.0
            {
                key[18..].copy_from_slice(&1u64.to_be_bytes());
                txn.put(cf, key, value);
            }
        }
    }
    txn.commit().unwrap();
    assert_eq!(run(&db, DELETE), count(2));
    assert_eq!(run(&db, DELETE), count(0));
    check_invariants(&db).unwrap();
}

#[test]
fn visibility_write_denial_and_query_failure_leave_relationships_unchanged() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(brainwire_core::acl::ReadVisibility {
            principals: vec!["reader".into()],
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(
        tx.run(DELETE, &BTreeMap::new()),
        Err(brainwire_cypher::ExecError::Forbidden(_))
    ));
    assert!(execute(
        &db,
        &DELETE.replace("RETURN count(*)", "RETURN size(a)"),
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(run(&db, DELETE), count(1));
}

#[test]
fn relationship_binding_reuse_filters_identity_and_unsupported_shapes_fail_early() {
    let (_dir, db) = setup();
    assert_eq!(
        run(
            &db,
            "MATCH (a:Page)-[r:LINK]->(b:Page) MATCH (b:Page)<-[r:LINK]-(a:Page) RETURN count(r)"
        ),
        count(4)
    );
    assert_eq!(run(&db,"MATCH (a:Page {entity_id:'a'})-[r:LINK]->(b:Page {entity_id:'b'}) MATCH (b:Page)-[r:LINK]->(a:Page) RETURN count(*)"),count(0));
    for query in [
        "MATCH (a:Page {entity_id:'missing'})-[r:LINK]->(b:Page) RETURN r",
        "MATCH (a:Page {entity_id:'missing'})-[r:LINK]->(b:Page) RETURN r.weight",
        "MATCH (a:Page)-[r:LINK]->(b:Page) SET r.weight=2",
        "MATCH (r:Page)-[r:LINK]->(b:Page) RETURN count(*)",
        "MATCH (a:Page)-[r:LINK]->(r:Page) RETURN count(*)",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
    }
}

#[test]
fn large_relationship_properties_can_be_deleted_under_small_query_guard() {
    let (_dir, db) = setup();
    let mut txn = db.begin();
    let catalog = db.catalog().unwrap().unwrap();
    let source = brainwire_store::graph::find_by_entity_id(&txn, "a", None).unwrap()[0];
    let target = brainwire_store::graph::find_by_entity_id(&txn, "b", None).unwrap()[0];
    brainwire_store::graph::merge_edge(
        &mut txn,
        &catalog,
        source,
        "LINK",
        target,
        &BTreeMap::from([("payload".into(), Value::Str("x".repeat(1 << 20)))]),
    )
    .unwrap();
    txn.commit().unwrap();
    assert_eq!(
        brainwire_cypher::execute_with_limits(
            &db,
            DELETE,
            &BTreeMap::new(),
            brainwire_cypher::ExecutionLimits {
                memory_bytes: 16 * 1024,
                timeout: None
            }
        )
        .unwrap()
        .rows,
        count(1)
    );
    check_invariants(&db).unwrap();
}
