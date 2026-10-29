use std::collections::BTreeMap;

use brainwire_core::{acl::ReadVisibility, schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out, QueryAccess, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph,
    txn::Cf,
};

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
        "version": "1",
        "nodes": [{"label":"Doc","properties":["title"]}],
        "edges": [{"type":"LINK","from":{"label":"Doc"},"to":{"label":"Doc"},
                   "properties":["entity_id","weight"]}]
    }))
    .unwrap();
    db.apply_ontology(&mapping).unwrap();
    run(&db, "CREATE (a:Doc {entity_id:'a',title:'A'}),(b:Doc {entity_id:'b',title:'B'}) CREATE (a)-[:LINK {entity_id:'user-edge-id',weight:2}]->(b)");
    (dir, db)
}

fn run(db: &Database, query: &str) -> Vec<Vec<Out>> {
    execute(db, query, &BTreeMap::new()).unwrap().rows
}

fn value(value: Value) -> Out {
    Out::Value(value)
}
fn text(text: &str) -> Value {
    Value::Str(text.into())
}

#[test]
fn node_property_functions_hide_virtual_identity_but_identity_access_and_node_values_keep_it() {
    let (_dir, db) = setup();
    let rows = run(&db, "MATCH (n:Doc {entity_id:'a'}) RETURN keys(n),properties(n),n.entity_id,n,n{.entity_id, .*},n{.*}");
    assert_eq!(rows[0][0], value(Value::List(vec![text("title")])));
    assert_eq!(
        rows[0][1],
        value(Value::Map(BTreeMap::from([("title".into(), text("A"))])))
    );
    assert_eq!(rows[0][2], value(text("a")));
    let Out::Node(node) = &rows[0][3] else {
        panic!("expected node")
    };
    assert_eq!(node.props.get("entity_id"), Some(&text("a")));
    assert_eq!(
        rows[0][4],
        value(Value::Map(BTreeMap::from([
            ("entity_id".into(), text("a")),
            ("title".into(), text("A"))
        ])))
    );
    assert_eq!(rows[0][5], rows[0][1]);
    run(&db, "MERGE (n:Doc {entity_id:'a'}) SET n.title='Updated'");
    assert_eq!(
        run(
            &db,
            "MATCH (n:Doc {entity_id:'a'}) RETURN properties(n),n.entity_id"
        ),
        vec![vec![
            value(Value::Map(BTreeMap::from([(
                "title".into(),
                text("Updated")
            )]))),
            value(text("a"))
        ]]
    );
}

#[test]
fn reserved_relationship_acl_metadata_is_hidden_from_every_read_projection() {
    let (_dir, db) = setup();
    for id in ["a", "b"] {
        run(&db,&format!("CALL db.setNodeAcl({{label:'Doc',entityId:'{id}',allow:['alice'],deny:[]}}) YIELD updated RETURN updated"));
    }
    let catalog = db.catalog().unwrap().unwrap();
    let mut txn = db.begin();
    let a =
        graph::find_by_entity_id(&txn, "a", Some(catalog.node_type("Doc").unwrap().id)).unwrap()[0];
    let edge = graph::out_edges(&txn, a).unwrap()[0];
    let mut key = a.0.to_be_bytes().to_vec();
    key.extend_from_slice(&edge.edge_type.0.to_be_bytes());
    key.extend_from_slice(&edge.other.0.to_be_bytes());
    key.extend_from_slice(&edge.id.0.to_be_bytes());
    let mut row =
        brainwire_core::codec::decode_row(&txn.get(Cf::Out, &key).unwrap().unwrap()).unwrap();
    row.extra.insert("ACL_Secret".into(), text("private-edge"));
    row.extra.insert("acl_allow".into(), text("private-list"));
    txn.put(Cf::Out, key, brainwire_core::codec::encode_row(&row));
    txn.commit().unwrap();
    let query="MATCH p = (a:Doc {entity_id:'a'})-[r:LINK]->(b:Doc) RETURN r.ACL_Secret,r.acl_allow,keys(r),properties(r),r{.*},r,p,[r],r.entity_id";
    for restricted in [false, true] {
        let rows = if restricted {
            let mut tx = QueryTransaction::begin(&db)
                .unwrap()
                .with_read_visibility(ReadVisibility {
                    principals: vec!["alice".into()],
                    ..Default::default()
                })
                .unwrap();
            let output = tx
                .run_with_access(
                    query,
                    &BTreeMap::new(),
                    QueryAccess {
                        write: false,
                        instance_admin: false,
                    },
                )
                .unwrap();
            tx.commit().unwrap();
            output.rows
        } else {
            run(&db, query)
        };
        let row = &rows[0];
        assert_eq!(row[0], value(Value::Null));
        assert_eq!(row[1], value(Value::Null));
        assert_eq!(
            row[2],
            value(Value::List(vec![text("entity_id"), text("weight")]))
        );
        let props = BTreeMap::from([
            ("entity_id".into(), text("user-edge-id")),
            ("weight".into(), Value::Int(2)),
        ]);
        assert_eq!(row[3], value(Value::Map(props.clone())));
        assert_eq!(row[4], value(Value::Map(props.clone())));
        let Out::Relationship(edge) = &row[5] else {
            panic!("expected relationship")
        };
        assert_eq!(edge.props, props);
        let Out::Path(path) = &row[6] else {
            panic!("expected path")
        };
        assert_eq!(path.rels[0].props, props);
        assert_eq!(row[7], Out::List(vec![row[5].clone()]));
        assert_eq!(row[8], value(text("user-edge-id")));
    }
}

#[test]
fn maps_and_relationship_user_properties_keep_entity_id_and_null_stays_null() {
    let (_dir, db) = setup();
    assert_eq!(run(&db,"RETURN keys({entity_id:'map-id',title:'M'}),properties({entity_id:'map-id'}),keys(null),properties(null)"), vec![vec![
        value(Value::List(vec![text("entity_id"),text("title")])),
        value(Value::Map(BTreeMap::from([("entity_id".into(),text("map-id"))]))),
        value(Value::Null),value(Value::Null)
    ]]);
    let rows=run(&db,"MATCH (:Doc {entity_id:'a'})-[r:LINK]->(:Doc {entity_id:'b'}) RETURN keys(r),properties(r),r.entity_id,r");
    assert_eq!(
        rows[0][0],
        value(Value::List(vec![text("entity_id"), text("weight")]))
    );
    assert_eq!(
        rows[0][1],
        value(Value::Map(BTreeMap::from([
            ("entity_id".into(), text("user-edge-id")),
            ("weight".into(), Value::Int(2))
        ])))
    );
    assert_eq!(rows[0][2], value(text("user-edge-id")));
    let Out::Relationship(edge) = &rows[0][3] else {
        panic!("expected relationship")
    };
    assert_eq!(edge.props.get("entity_id"), Some(&text("user-edge-id")));
}

#[test]
fn reserved_node_acl_metadata_stays_hidden_in_property_functions_with_visibility() {
    let (_dir, db) = setup();
    run(&db,"CALL db.setNodeAcl({label:'Doc',entityId:'a',allow:['alice'],deny:[]}) YIELD updated RETURN updated");
    run(&db,"CALL db.setNodeAcl({label:'Doc',entityId:'b',allow:['bob'],deny:[]}) YIELD updated RETURN updated");
    let mut txn = db.begin();
    let catalog = db.catalog().unwrap().unwrap();
    let id =
        graph::find_by_entity_id(&txn, "a", Some(catalog.node_type("Doc").unwrap().id)).unwrap()[0];
    let mut record = graph::get_node(&txn, id).unwrap().unwrap();
    record
        .row
        .extra
        .insert("ACL_Secret".into(), text("not-a-property"));
    record.row.extra.insert("acl_allow".into(), text("private"));
    let mut encoded = (record.entity_id.len() as u32).to_be_bytes().to_vec();
    encoded.extend_from_slice(record.entity_id.as_bytes());
    encoded.extend_from_slice(&brainwire_core::codec::encode_row(&record.row));
    txn.put(Cf::Node, id.0.to_be_bytes().to_vec(), encoded);
    txn.commit().unwrap();
    let query="MATCH (n:Doc) RETURN n.entity_id,keys(n),properties(n),n.ACL_Secret,n.acl_allow ORDER BY n.entity_id";
    let expected = vec![
        value(text("a")),
        value(Value::List(vec![text("title")])),
        value(Value::Map(BTreeMap::from([("title".into(), text("A"))]))),
        value(Value::Null),
        value(Value::Null),
    ];
    assert_eq!(run(&db, query)[0], expected);
    let mut transaction = QueryTransaction::begin(&db)
        .unwrap()
        .with_read_visibility(ReadVisibility {
            principals: vec!["alice".into()],
            ..Default::default()
        })
        .unwrap();
    let output = transaction
        .run_with_access(
            query,
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
        )
        .unwrap();
    transaction.commit().unwrap();
    assert_eq!(output.rows, vec![expected]);
}
