use brainwire_core::{
    acl::{NodeAcl, ReadVisibility},
    schema::GraphMapping,
    value::Value,
};
use brainwire_cypher::{
    execute, ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{find_by_entity_id, set_node_acl},
    txn::Cf,
};
use std::{collections::BTreeMap, sync::Arc};

const LIMIT: usize = 64 << 10;

fn setup() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"First","properties":["text"]},{"label":"Last","properties":["text"]}],"edges":[]}"#,
    ).unwrap();
    database.apply_ontology(&mapping).unwrap();
    (directory, database)
}

fn bounded<'a>(database: &'a Database, pool: &Arc<QueryMemoryPool>) -> QueryTransaction<'a> {
    QueryTransaction::begin_with_pool(
        database,
        ExecutionLimits {
            memory_bytes: LIMIT,
            timeout: None,
        },
        Arc::clone(pool),
    )
    .unwrap()
}

#[test]
fn unlabelled_limit_stops_before_a_later_label_and_bounds_retention() {
    let (_directory, database) = setup();
    execute(
        &database,
        "UNWIND range(0,999) AS i CREATE (:First {entity_id:'first-' + i,text:'value'})",
        &BTreeMap::new(),
    )
    .unwrap();
    execute(
        &database,
        "CREATE (:Last {entity_id:'tail',text:'tail'})",
        &BTreeMap::new(),
    )
    .unwrap();
    let pool = Arc::new(QueryMemoryPool::new(LIMIT));
    let mut limited = bounded(&database, &pool);
    let result = limited
        .run("MATCH (n) RETURN n LIMIT 3", &BTreeMap::new())
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(result
        .rows
        .iter()
        .all(|row| matches!(&row[0], Out::Node(node) if node.label == "First")));
    limited.rollback();
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(pool.usage().peak_bytes <= LIMIT);
    let mut unlimited = bounded(&database, &pool);
    assert!(matches!(
        unlimited.run("MATCH (n) RETURN n", &BTreeMap::new()),
        Err(ExecError::MemoryLimit { .. }) | Err(ExecError::MemoryPoolExhausted { .. })
    ));
    drop(unlimited);
    assert_eq!(pool.usage().reserved_bytes, 0);

    // A later unread record is deliberately invalid: success proves the scan
    // stops before decoding that label, independently of a timing threshold.
    let catalog = database.catalog().unwrap().unwrap();
    let ty = catalog.node_type("Last").unwrap().id;
    let mut writer = database.begin();
    let tail = find_by_entity_id(&writer, "tail", Some(ty)).unwrap()[0];
    writer.put(Cf::Node, tail.0.to_be_bytes().to_vec(), vec![]);
    writer.commit().unwrap();
    assert_eq!(
        execute(&database, "MATCH (n) RETURN n LIMIT 3", &BTreeMap::new())
            .unwrap()
            .rows
            .len(),
        3
    );
    assert!(matches!(
        execute(&database, "MATCH (n) RETURN n", &BTreeMap::new()),
        Err(ExecError::Graph(_))
    ));
}

#[test]
fn unlabelled_limit_counts_visible_rows_across_labels_in_its_snapshot() {
    let (_directory, database) = setup();
    execute(&database,
        "CREATE (:First {entity_id:'hidden',text:'hidden'}),(:First {entity_id:'first',text:'before'}),(:Last {entity_id:'last',text:'before'})",
        &BTreeMap::new(),
    ).unwrap();
    let catalog = database.catalog().unwrap().unwrap();
    let mut acl = database.begin();
    for (label, entity_id, principal) in [
        ("First", "hidden", "other"),
        ("First", "first", "reader"),
        ("Last", "last", "reader"),
    ] {
        let ty = catalog.node_type(label).unwrap().id;
        let id = find_by_entity_id(&acl, entity_id, Some(ty)).unwrap()[0];
        set_node_acl(
            &mut acl,
            id,
            &NodeAcl {
                allow: vec![principal.into()],
                ..Default::default()
            },
        )
        .unwrap();
    }
    acl.commit().unwrap();
    let pool = Arc::new(QueryMemoryPool::new(LIMIT));
    let mut reader = bounded(&database, &pool)
        .with_read_visibility(ReadVisibility {
            principals: vec!["reader".into()],
            ..Default::default()
        })
        .unwrap();
    execute(&database, "MATCH (n) SET n.text='after'", &BTreeMap::new()).unwrap();
    let mut acl = database.begin();
    let id =
        find_by_entity_id(&acl, "last", Some(catalog.node_type("Last").unwrap().id)).unwrap()[0];
    set_node_acl(
        &mut acl,
        id,
        &NodeAcl {
            allow: vec!["other".into()],
            ..Default::default()
        },
    )
    .unwrap();
    acl.commit().unwrap();
    let output = reader
        .run(
            "MATCH (n) RETURN n.entity_id,n.text LIMIT 2",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        output.rows,
        vec![
            vec![
                Out::Value(Value::Str("first".into())),
                Out::Value(Value::Str("before".into()))
            ],
            vec![
                Out::Value(Value::Str("last".into())),
                Out::Value(Value::Str("before".into()))
            ],
        ]
    );
    reader.rollback();
    assert_eq!(pool.usage().reserved_bytes, 0);
    let mut fresh = bounded(&database, &pool)
        .with_read_visibility(ReadVisibility {
            principals: vec!["reader".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        fresh
            .run(
                "MATCH (n) RETURN n.entity_id,n.text LIMIT 2",
                &BTreeMap::new()
            )
            .unwrap()
            .rows,
        vec![vec![
            Out::Value(Value::Str("first".into())),
            Out::Value(Value::Str("after".into()))
        ]]
    );
    fresh.rollback();
    assert_eq!(pool.usage().reserved_bytes, 0);
}
