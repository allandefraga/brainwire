use brainwire_core::{
    acl::{NodeAcl, ReadVisibility},
    schema::GraphMapping,
    value::Value,
};
use brainwire_cypher::{execute, ExecError, ExecutionLimits, Out, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{find_by_entity_id, set_node_acl},
};
use std::collections::BTreeMap;

const DOCS: usize = 1_503;
const PAGE_SIZE: usize = 200;

fn visible(index: usize) -> bool {
    !index.is_multiple_of(3) && !index.is_multiple_of(17) && !index.is_multiple_of(19)
}

fn id(index: usize) -> String {
    format!("row-{index:05}")
}

fn setup() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Doc","properties":[]},{"label":"Noise","properties":[]}],"edges":[]}"#,
    )
    .unwrap();
    database.apply_ontology(&mapping).unwrap();
    let rows = Value::List((0..DOCS).map(|index| Value::Str(id(index))).collect());
    execute(
        &database,
        "UNWIND $ids AS identity CREATE (:Doc {entity_id:identity}), (:Noise {entity_id:identity})",
        &BTreeMap::from([("ids".into(), rows)]),
    )
    .unwrap();
    execute(
        &database,
        "CREATE (:Noise {entity_id:'zz-noise-only'})",
        &BTreeMap::new(),
    )
    .unwrap();
    let catalog = database.catalog().unwrap().unwrap();
    let doc_type = catalog.node_type("Doc").unwrap().id;
    let noise_type = catalog.node_type("Noise").unwrap().id;
    let mut transaction = database.begin();
    for index in 0..DOCS {
        let identity = id(index);
        let noise = find_by_entity_id(&transaction, &identity, Some(noise_type)).unwrap()[0];
        set_node_acl(
            &mut transaction,
            noise,
            &NodeAcl {
                allow: vec!["alice".into()],
                ..Default::default()
            },
        )
        .unwrap();
        if index.is_multiple_of(19) {
            continue;
        }
        let doc = find_by_entity_id(&transaction, &identity, Some(doc_type)).unwrap()[0];
        set_node_acl(
            &mut transaction,
            doc,
            &NodeAcl {
                allow: vec![if index.is_multiple_of(3) {
                    "bob"
                } else {
                    "alice"
                }
                .into()],
                deny: if index.is_multiple_of(17) {
                    vec!["alice".into()]
                } else {
                    vec![]
                },
                ..Default::default()
            },
        )
        .unwrap();
    }
    let noise = find_by_entity_id(&transaction, "zz-noise-only", Some(noise_type)).unwrap()[0];
    set_node_acl(
        &mut transaction,
        noise,
        &NodeAcl {
            allow: vec!["alice".into()],
            ..Default::default()
        },
    )
    .unwrap();
    transaction.commit().unwrap();
    (directory, database)
}

fn restricted(database: &Database, query: &str, params: &BTreeMap<String, Value>) -> Vec<Vec<Out>> {
    let mut transaction = QueryTransaction::begin(database)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    let rows = transaction.run(query, params).unwrap().rows;
    assert_eq!(transaction.commit().unwrap(), None);
    rows
}

fn page(database: &Database, after: &str) -> Vec<String> {
    restricted(
        database,
        "MATCH (n:Doc) WHERE n.entity_id > $after RETURN n.entity_id AS id ORDER BY n.entity_id LIMIT 200",
        &BTreeMap::from([("after".into(), Value::Str(after.into()))]),
    )
    .into_iter()
    .map(|row| match row.as_slice() {
        [Out::Value(Value::Str(identity))] => identity.clone(),
        other => panic!("unexpected cursor row: {other:?}"),
    })
    .collect()
}

#[test]
fn acl_seek_pages_preserve_order_visibility_and_complete_cardinality() {
    let (_directory, database) = setup();
    let expected: Vec<_> = (0..DOCS).filter(|index| visible(*index)).map(id).collect();
    assert!(expected.len() > PAGE_SIZE * 3);
    assert_eq!(
        restricted(&database, "MATCH (n:Doc) RETURN count(n)", &BTreeMap::new()),
        vec![vec![Out::Value(Value::Int(expected.len() as i64))]]
    );
    let mut after = String::new();
    let mut actual = vec![];
    let mut sizes = vec![];
    loop {
        let next = page(&database, &after);
        assert!(next.len() <= PAGE_SIZE);
        assert!(next.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(next.iter().all(|identity| identity > &after));
        sizes.push(next.len());
        if next.is_empty() {
            break;
        }
        after = next.last().unwrap().clone();
        actual.extend(next);
        assert!(
            actual.len() <= expected.len(),
            "cursor repeated or leaked rows"
        );
    }
    assert_eq!(actual, expected);
    assert_eq!(&sizes[..3], &[PAGE_SIZE; 3]);
    assert_eq!(sizes.last(), Some(&0));
}

#[test]
fn acl_seek_skips_hidden_boundaries_and_same_ids_in_other_labels() {
    let (_directory, database) = setup();
    let after = id(1_002);
    assert!(!visible(1_002));
    let expected: Vec<_> = (1_003..DOCS)
        .filter(|index| visible(*index))
        .map(id)
        .take(PAGE_SIZE)
        .collect();
    assert_eq!(page(&database, &after), expected);
    assert!(page(&database, &id(DOCS - 1)).is_empty());
}

#[test]
fn unlabelled_limit_zero_and_ordered_skip_keep_visible_rows() {
    let (_directory, database) = setup();
    let query = "MATCH (n) RETURN elementId(n) AS identity ORDER BY identity";
    let all = restricted(&database, query, &BTreeMap::new());
    assert!(all.len() > 401);
    assert!(restricted(&database, &format!("{query} LIMIT 0"), &BTreeMap::new()).is_empty());
    assert_eq!(
        restricted(&database, &format!("{query} LIMIT 1"), &BTreeMap::new()),
        all[..1]
    );
    assert_eq!(
        restricted(
            &database,
            &format!("{query} SKIP 201 LIMIT 200"),
            &BTreeMap::new()
        ),
        all[201..401]
    );
}

#[test]
fn unlabelled_limit_does_not_materialize_a_later_large_node() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"ASmall","properties":[]},{"label":"ZLarge","properties":["payload"]}],"edges":[]}"#,
    )
    .unwrap();
    database.apply_ontology(&mapping).unwrap();
    execute(
        &database,
        "CREATE (:ASmall {entity_id:'first'}), (:ZLarge {entity_id:'later',payload:$payload})",
        &BTreeMap::from([("payload".into(), Value::Str("x".repeat(1 << 20)))]),
    )
    .unwrap();
    let limits = ExecutionLimits {
        memory_bytes: 8_192,
        timeout: None,
    };
    let mut transaction = QueryTransaction::begin_with_limits(&database, limits).unwrap();
    let first = transaction
        .run("MATCH (n) RETURN n LIMIT 1", &BTreeMap::new())
        .unwrap();
    assert!(
        matches!(first.rows.as_slice(), [row] if matches!(row.as_slice(), [Out::Node(node)] if node.label == "ASmall"))
    );
    transaction.commit().unwrap();
    let mut transaction = QueryTransaction::begin_with_limits(&database, limits).unwrap();
    assert!(transaction
        .run("MATCH (n) RETURN n LIMIT 0", &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
    transaction.commit().unwrap();
    let mut transaction = QueryTransaction::begin_with_limits(&database, limits).unwrap();
    assert!(matches!(
        transaction.run("MATCH (n) RETURN n SKIP 1 LIMIT 1", &BTreeMap::new()),
        Err(ExecError::MemoryLimit { .. })
    ));
}
