use std::{collections::BTreeMap, sync::Arc};

use brainwire_core::{
    acl::{NodeAcl, ReadVisibility},
    schema::GraphMapping,
    value::Value,
};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryMemoryPool,
    QueryTransaction,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{find_by_entity_id, merge_node, set_node_acl},
};

fn setup(count: usize) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item","properties":[]}],"edges":[]}"#,
    )
    .unwrap();
    let catalog = db.apply_ontology(&mapping).unwrap();
    let mut tx = db.begin();
    for i in 0..count {
        merge_node(
            &mut tx,
            &catalog,
            "Item",
            &format!("item-{i:04}"),
            &BTreeMap::new(),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    (dir, db)
}

fn params(count: usize) -> BTreeMap<String, Value> {
    BTreeMap::from([(
        "ids".into(),
        Value::List(
            (0..count)
                .map(|i| Value::Str(format!("item-{i:04}")))
                .collect(),
        ),
    )])
}

const COUNT: &str = "MATCH (n:Item) WHERE n.entity_id IN $ids RETURN count(n)";

#[test]
fn entity_in_candidates_obey_the_query_pool_and_release_on_failure() {
    let (_dir, db) = setup(1024);
    let pool = Arc::new(QueryMemoryPool::new(300 * 1024));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert!(matches!(
        tx.run(COUNT, &params(1024)),
        Err(ExecError::MemoryPoolExhausted { .. })
    ));
    drop(tx);
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(pool.usage().peak_bytes <= 300 * 1024);
    assert_eq!(
        execute(&db, "MATCH (n:Item) RETURN count(n)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![Out::Value(Value::Int(1024))]]
    );
}

#[test]
fn entity_in_memory_failure_rolls_back_prior_writes() {
    let (_dir, db) = setup(1024);
    let mut tx = QueryTransaction::begin_with_limits(
        &db,
        ExecutionLimits {
            memory_bytes: 300 * 1024,
            timeout: None,
        },
    )
    .unwrap();
    tx.run("CREATE (:Item {entity_id:'pending'})", &BTreeMap::new())
        .unwrap();
    assert!(matches!(
        tx.run(COUNT, &params(1024)),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(tx.commit().is_err());
    assert_eq!(
        execute(
            &db,
            "MATCH (n:Item {entity_id:'pending'}) RETURN count(n)",
            &BTreeMap::new()
        )
        .unwrap()
        .rows,
        vec![vec![Out::Value(Value::Int(0))]]
    );
}

#[test]
fn duplicate_and_nonstring_members_preserve_visibility_without_duplicate_rows() {
    let (_dir, db) = setup(3);
    let catalog = db.catalog().unwrap().unwrap();
    let mut tx = db.begin();
    for i in 0..3 {
        let id = find_by_entity_id(
            &tx,
            &format!("item-{i:04}"),
            Some(catalog.node_type("Item").unwrap().id),
        )
        .unwrap()[0];
        let principal = if i == 1 { "bob" } else { "alice" };
        set_node_acl(
            &mut tx,
            id,
            &NodeAcl {
                allow: vec![principal.into()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let mut ids = vec![Value::Str("item-0002".into()); 200];
    ids.extend([
        Value::Str("item-0000".into()),
        Value::Str("item-0001".into()),
        Value::Null,
        Value::Int(2),
    ]);
    let input = BTreeMap::from([("ids".into(), Value::List(ids))]);
    let mut reader = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .run(
                "MATCH (n:Item) WHERE n.entity_id IN $ids RETURN n.entity_id ORDER BY n.entity_id",
                &input
            )
            .unwrap()
            .rows,
        vec![
            vec![Out::Value(Value::Str("item-0000".into()))],
            vec![Out::Value(Value::Str("item-0002".into()))]
        ]
    );
    let result = execute_with_limits(
        &db,
        COUNT,
        &BTreeMap::from([(
            "ids".into(),
            Value::List(vec![Value::Str("item-0000".into()); 200]),
        )]),
        ExecutionLimits {
            memory_bytes: 128 * 1024,
            timeout: None,
        },
    )
    .unwrap();
    assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(1))]]);
}
