use brainwire_core::{
    ids::{EdgeTypeId, TypeId},
    schema::{Catalog, Column, EdgeType, GraphMapping, NodeType},
    value::Value,
};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryTransaction,
    QueryTransactionState,
};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    txn::Cf,
};
use std::{collections::BTreeMap, time::Duration};

const QUERY: &str = "CALL db.schema() YIELD version, nodes, relationships, indexesReady RETURN version, nodes, relationships, indexesReady";

fn open() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    (dir, db)
}
fn string(s: &str) -> Value {
    Value::Str(s.into())
}
fn strings(names: &[&str]) -> Value {
    Value::List(names.iter().map(|s| string(s)).collect())
}
fn map(fields: &[(&str, Value)]) -> Value {
    Value::Map(
        fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    )
}
fn run(db: &Database) -> Vec<Out> {
    let output = execute(db, QUERY, &BTreeMap::new()).unwrap();
    assert_eq!(
        output.fields,
        ["version", "nodes", "relationships", "indexesReady"]
    );
    assert_eq!(output.rows.len(), 1);
    output.rows.into_iter().next().unwrap()
}
fn store_catalog(db: &Database, catalog: &Catalog) {
    let mut tx = db.begin();
    tx.put(
        Cf::Meta,
        b"catalog".to_vec(),
        serde_json::to_vec(catalog).unwrap(),
    );
    tx.commit().unwrap();
}
fn node(label: &str, id: u16) -> NodeType {
    NodeType {
        description: None,
        id: TypeId(id),
        label: label.into(),
        columns: vec![],
        indexed: vec![],
        identifiers: vec![],
        display_color: None,
        index_names: Default::default(),
        populating: vec![],
        retired_columns: vec![],
    }
}
fn catalog() -> Catalog {
    Catalog {
        version: 7,
        node_types: vec![node("Page", 1)],
        edge_types: vec![],
        streams: vec![],
    }
}

#[test]
fn empty_catalog_returns_one_row() {
    let (_dir, db) = open();
    assert_eq!(
        run(&db),
        vec![
            Out::Value(Value::Int(0)),
            Out::Value(strings(&[])),
            Out::Value(strings(&[])),
            Out::Value(Value::Bool(false))
        ]
    );
}

#[test]
fn returns_effective_columns_and_every_signature_in_catalog_order() {
    let (_dir, db) = open();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version":"1", "nodes":[
            {"label":"Zulu", "properties":["old", "entity_id", "ACL_hidden"], "indexed_fields":["old"]},
            {"label":"Alpha", "properties":["title"]}, {"label":"Isolated"}
        ], "edges":[
            {"type":"LINK", "from":{"label":"Zulu"}, "to":{"label":"Alpha"}, "properties":["weight"]},
            {"type":"LINK", "from":{"label":"Alpha"}, "to":{"label":"Zulu"}, "properties":["since"]},
            {"type":"LINK", "from":{"label":"Zulu"}, "to":{"label":"Zulu"}}
        ]
    })).unwrap();
    let first = db.apply_ontology(&mapping).unwrap();
    let update: GraphMapping = serde_json::from_value(serde_json::json!({"version":"2", "nodes":[{"label":"Zulu", "properties":["new"], "indexed_fields":["new"]}]})).unwrap();
    let current = db.apply_ontology(&update).unwrap();
    assert!(current.version > first.version);
    let nodes = Value::List(vec![
        map(&[
            ("label", string("Zulu")),
            ("properties", strings(&["entity_id", "old", "new"])),
            ("indexedFields", strings(&["new"])),
        ]),
        map(&[
            ("label", string("Alpha")),
            ("properties", strings(&["entity_id", "title"])),
            ("indexedFields", strings(&[])),
        ]),
        map(&[
            ("label", string("Isolated")),
            ("properties", strings(&["entity_id"])),
            ("indexedFields", strings(&[])),
        ]),
    ]);
    let relationships = Value::List(
        [("Zulu", "Alpha"), ("Alpha", "Zulu"), ("Zulu", "Zulu")]
            .into_iter()
            .map(|(source, target)| {
                map(&[
                    ("name", string("LINK")),
                    ("source", string(source)),
                    ("target", string(target)),
                    ("properties", strings(&["weight", "since"])),
                ])
            })
            .collect(),
    );
    assert_eq!(
        run(&db),
        vec![
            Out::Value(Value::Int(current.version.into())),
            Out::Value(nodes),
            Out::Value(relationships),
            Out::Value(Value::Bool(true))
        ]
    );
}

#[test]
fn exposes_declared_merge_keys_for_each_keyed_relationship_signature() {
    let (_dir, db) = open();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version":"1", "nodes":[{"label":"Ticket"}, {"label":"Person"}],
        "edges":[
            {"type":"EVENT", "from":{"label":"Ticket"}, "to":{"label":"Ticket"},
             "properties":["event_key"], "merge_key_fields":["event_key"]},
            {"type":"EVENT", "from":{"label":"Person"}, "to":{"label":"Ticket"},
             "properties":["event_key"]},
            {"type":"LINK", "from":{"label":"Ticket"}, "to":{"label":"Ticket"}}
        ]
    }))
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    let Out::Value(Value::List(relationships)) = &run(&db)[2] else {
        panic!("expected relationships")
    };
    assert_eq!(relationships.len(), 3);
    for relationship in &relationships[..2] {
        let Value::Map(fields) = relationship else {
            panic!("expected relationship map")
        };
        assert_eq!(fields.get("mergeKeyFields"), Some(&strings(&["event_key"])));
    }
    let Value::Map(legacy) = &relationships[2] else {
        panic!("expected relationship map")
    };
    assert!(!legacy.contains_key("mergeKeyFields"));
}

#[test]
fn merge_key_metadata_is_counted_for_schema_limits() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.edge_types.push(EdgeType {
        merge_key_fields: vec!["k".repeat(600_000)],
        description: None,
        id: EdgeTypeId(1),
        name: "EVENT".into(),
        signatures: vec![(TypeId(1), TypeId(1)); 2],
        columns: vec![],
    });
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("schema string limit"));
    c.edge_types[0].signatures.truncate(1);
    c.edge_types[0].merge_key_fields = vec![String::new(); 65536];
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("collection entry limit"));
}

#[test]
fn rejects_arguments_and_invalid_yields_and_rolls_back_staged_writes() {
    let (_dir, db) = open();
    store_catalog(&db, &catalog());
    for query in [
        "CALL db.schema({}) YIELD version RETURN version",
        "CALL db.schema() YIELD missing RETURN missing",
        "CALL db.schema() YIELD version AS x, nodes AS x RETURN x",
    ] {
        assert!(matches!(
            execute(&db, query, &BTreeMap::new()),
            Err(ExecError::Semantic(_))
        ));
    }
    let mut tx = QueryTransaction::begin(&db).unwrap();
    tx.run("CREATE (:Page {entity_id:'staged'})", &BTreeMap::new())
        .unwrap();
    assert!(tx
        .run(
            "CALL db.schema(1) YIELD version RETURN version",
            &BTreeMap::new()
        )
        .is_err());
    assert_eq!(tx.state(), QueryTransactionState::Failed);
    assert!(tx.commit().is_err());
    assert!(execute(&db, "MATCH (p:Page) RETURN p", &BTreeMap::new())
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn rejects_oversized_schema_and_dangling_endpoints() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.node_types = (0..4097).map(|i| node("", i)).collect();
    store_catalog(&db, &c);
    assert!(matches!(
        execute(&db, QUERY, &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    c = catalog();
    c.edge_types.push(EdgeType {
        merge_key_fields: vec![],
        description: None,
        id: EdgeTypeId(1),
        name: "LINK".into(),
        signatures: vec![(TypeId(1), TypeId(1)); 4097],
        columns: vec![],
    });
    store_catalog(&db, &c);
    assert!(matches!(
        execute(&db, QUERY, &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    c.edge_types[0].signatures = vec![(TypeId(1), TypeId(2))];
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("missing endpoint"));
    c = catalog();
    c.node_types[0].label = "x".repeat((1 << 20) + 1);
    store_catalog(&db, &c);
    assert!(matches!(
        execute(&db, QUERY, &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
    c = catalog();
    c.node_types[0].columns = (0..65535)
        .map(|i| Column {
            id: i,
            name: String::new(),
        })
        .collect();
    store_catalog(&db, &c);
    assert!(matches!(
        execute(&db, QUERY, &BTreeMap::new()),
        Err(ExecError::Semantic(_))
    ));
}

#[test]
fn enforces_allocation_and_execution_deadline() {
    let (_dir, db) = open();
    store_catalog(&db, &catalog());
    execute_with_limits(
        &db,
        "RETURN 1",
        &BTreeMap::new(),
        ExecutionLimits {
            memory_bytes: 8192,
            timeout: None,
        },
    )
    .unwrap();
    assert!(matches!(
        execute_with_limits(
            &db,
            QUERY,
            &BTreeMap::new(),
            ExecutionLimits {
                memory_bytes: 8192,
                timeout: None
            }
        ),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert!(matches!(
        execute_with_limits(
            &db,
            QUERY,
            &BTreeMap::new(),
            ExecutionLimits {
                memory_bytes: 1 << 20,
                timeout: Some(Duration::ZERO)
            }
        ),
        Err(ExecError::TimedOut)
    ));
}

#[test]
fn catalog_and_index_readiness_use_the_transaction_snapshot() {
    let (_dir, db) = open();
    let mut c = catalog();
    store_catalog(&db, &c);
    let mut old = QueryTransaction::begin(&db).unwrap();
    let before = old.run(QUERY, &BTreeMap::new()).unwrap();
    assert_eq!(before.rows[0][3], Out::Value(Value::Bool(false)));
    c.version += 1;
    c.node_types.push(node("New", 2));
    let mut update = db.begin();
    update.put(
        Cf::Meta,
        b"catalog".to_vec(),
        serde_json::to_vec(&c).unwrap(),
    );
    update.put(Cf::Meta, b"scalar-index-v2-ready".to_vec(), vec![2]);
    update.commit().unwrap();
    assert_eq!(old.run(QUERY, &BTreeMap::new()).unwrap(), before);
    let after = run(&db);
    assert_eq!(after[0], Out::Value(Value::Int(8)));
    assert_eq!(after[3], Out::Value(Value::Bool(true)));
    assert_ne!(after[1], before.rows[0][1]);
    old.rollback();
}

#[test]
fn caps_count_repeated_endpoints_and_relationship_properties() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.node_types[0].label = "x".repeat(200_000);
    c.edge_types.push(EdgeType {
        merge_key_fields: vec![],
        description: None,
        id: EdgeTypeId(1),
        name: "LINK".into(),
        signatures: vec![(TypeId(1), TypeId(1)); 3],
        columns: vec![],
    });
    store_catalog(&db, &c);
    let error = execute(&db, QUERY, &BTreeMap::new()).unwrap_err();
    assert!(matches!(error, ExecError::Semantic(ref s) if s.contains("string limit")));
    c.node_types[0].label = "Page".into();
    c.edge_types[0].signatures = vec![(TypeId(1), TypeId(1)); 7];
    c.edge_types[0].columns = (0..10_000)
        .map(|i| Column {
            id: i,
            name: String::new(),
        })
        .collect();
    store_catalog(&db, &c);
    let error = execute(&db, QUERY, &BTreeMap::new()).unwrap_err();
    assert!(matches!(error, ExecError::Semantic(ref s) if s.contains("entry limit")));
    c.edge_types[0].columns = vec![Column {
        id: 0,
        name: "p".repeat(200_000),
    }];
    store_catalog(&db, &c);
    let error = execute(&db, QUERY, &BTreeMap::new()).unwrap_err();
    assert!(matches!(error, ExecError::Semantic(ref s) if s.contains("string limit")));
}

#[test]
fn type_descriptions_survive_reopen_and_are_exposed_as_bounded_schema() {
    let (dir, db) = open();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version":"1", "nodes":[{"label":"Ticket", "description":"A customer request"}],
        "edges":[{"type":"RELATED", "from":{"label":"Ticket"}, "to":{"label":"Ticket"}, "description":"Related requests"}]
    })).unwrap();
    db.apply_ontology(&mapping).unwrap();
    drop(db);
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let values = run(&db);
    let Out::Value(Value::List(nodes)) = &values[1] else {
        panic!("expected nodes")
    };
    let Value::Map(node) = &nodes[0] else {
        panic!("expected node map")
    };
    assert_eq!(node.get("description"), Some(&string("A customer request")));
    let Out::Value(Value::List(edges)) = &values[2] else {
        panic!("expected edges")
    };
    let Value::Map(edge) = &edges[0] else {
        panic!("expected edge map")
    };
    assert_eq!(edge.get("description"), Some(&string("Related requests")));
}

#[test]
fn accumulated_node_and_relationship_descriptions_obey_schema_string_cap() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.node_types = (1..=257)
        .map(|id| {
            let mut ty = node(&format!("Type{id}"), id);
            ty.description = Some("x".repeat(4096));
            ty
        })
        .collect();
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("schema string limit"));
    c = catalog();
    c.edge_types = (1..=257)
        .map(|id| EdgeType {
            merge_key_fields: vec![],
            id: EdgeTypeId(id),
            name: format!("LINK{id}"),
            description: Some("x".repeat(4096)),
            signatures: vec![(TypeId(1), TypeId(1))],
            columns: vec![],
        })
        .collect();
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("schema string limit"));
}

#[test]
fn endpoint_lookup_preserves_sparse_ids_and_first_duplicate_in_catalog_order() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.node_types = vec![node("First", 1), node("Duplicate", 1), node("Last", 4095)];
    c.edge_types.push(EdgeType {
        merge_key_fields: vec![],
        description: None,
        id: EdgeTypeId(1),
        name: "LINK".into(),
        signatures: vec![(TypeId(4095), TypeId(1)), (TypeId(1), TypeId(4095))],
        columns: vec![],
    });
    store_catalog(&db, &c);
    assert_eq!(
        run(&db)[2],
        Out::Value(Value::List(
            [("Last", "First"), ("First", "Last")]
                .into_iter()
                .map(|(source, target)| map(&[
                    ("name", string("LINK")),
                    ("source", string(source)),
                    ("target", string(target)),
                    ("properties", strings(&[])),
                ]))
                .collect()
        ))
    );
    c.edge_types[0].signatures = vec![(TypeId(4095), TypeId(7))];
    store_catalog(&db, &c);
    assert!(execute(&db, QUERY, &BTreeMap::new())
        .unwrap_err()
        .to_string()
        .contains("missing endpoint"));
}

#[test]
fn dense_schema_preserves_every_relationship_without_scanning_instances() {
    let (_dir, db) = open();
    let mut c = catalog();
    c.node_types = (0..4096).map(|id| node(&format!("Type{id}"), id)).collect();
    c.edge_types.push(EdgeType {
        merge_key_fields: vec![],
        description: None,
        id: EdgeTypeId(1),
        name: "LINK".into(),
        signatures: (0..4096)
            .map(|id| (TypeId(id), TypeId(4095 - id)))
            .collect(),
        columns: vec![],
    });
    store_catalog(&db, &c);
    let output = run(&db);
    let Out::Value(Value::List(nodes)) = &output[1] else {
        panic!("expected nodes")
    };
    let Out::Value(Value::List(relationships)) = &output[2] else {
        panic!("expected relationships")
    };
    assert_eq!(nodes.len(), 4096);
    assert_eq!(relationships.len(), 4096);
    for (id, relationship) in relationships.iter().enumerate() {
        let Value::Map(fields) = relationship else {
            panic!("expected relationship map")
        };
        assert_eq!(fields["source"], string(&format!("Type{id}")));
        assert_eq!(fields["target"], string(&format!("Type{}", 4095 - id)));
    }
}
