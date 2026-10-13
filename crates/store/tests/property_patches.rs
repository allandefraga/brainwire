use brainwire_core::{codec::decode_row, schema::GraphMapping, value::Value};
use brainwire_store::{
    db::{Database, Resources, StoreConfig, StoreError},
    graph::{check_invariants, merge_edge, merge_node},
    txn::Cf,
};
use std::collections::BTreeMap;

#[test]
fn edge_partial_patch_preserves_omitted_fields_removes_null_and_reopens_with_one_mirror() {
    let folder = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(folder.path(), &resources).unwrap();
    let schema: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Page"}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"},"properties":["weight"]}]}"#,
    ).unwrap();
    let catalog = db.apply_ontology(&schema).unwrap();
    let mut tx = db.begin();
    let a = merge_node(&mut tx, &catalog, "Page", "a", &BTreeMap::new()).unwrap();
    let b = merge_node(&mut tx, &catalog, "Page", "b", &BTreeMap::new()).unwrap();
    merge_edge(
        &mut tx,
        &catalog,
        a,
        "LINK",
        b,
        &BTreeMap::from([
            ("weight".into(), Value::Int(2)),
            ("keep".into(), Value::Str("unchanged".into())),
            ("remove_me".into(), Value::Str("old".into())),
        ]),
    )
    .unwrap();
    tx.commit().unwrap();
    let patch = BTreeMap::from([
        ("weight".into(), Value::Null),
        ("remove_me".into(), Value::Null),
        (
            "new_list".into(),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ),
    ]);
    let mut pending = db.begin();
    merge_edge(&mut pending, &catalog, a, "LINK", b, &patch).unwrap();
    drop(pending);
    let weight_column = catalog
        .edge_type("LINK")
        .unwrap()
        .columns
        .iter()
        .find(|column| column.name == "weight")
        .unwrap()
        .id;
    let read = |db: &Database| {
        let tx = db.begin();
        let mut found = Vec::new();
        tx.visit_prefix::<StoreError>(Cf::Out, &[], |_, bytes| {
            found.push(decode_row(bytes).unwrap());
            Ok(())
        })
        .unwrap();
        assert_eq!(found.len(), 1);
        found.pop().unwrap()
    };
    assert_eq!(read(&db).columns.get(&weight_column), Some(&Value::Int(2)));
    let mut tx = db.begin();
    merge_edge(&mut tx, &catalog, a, "LINK", b, &patch).unwrap();
    tx.commit().unwrap();
    drop(db);
    let db = Database::open(folder.path(), &resources).unwrap();
    let row = read(&db);
    assert!(!row.columns.contains_key(&weight_column));
    assert!(!row.extra.contains_key("remove_me"));
    assert_eq!(row.extra.get("keep"), Some(&Value::Str("unchanged".into())));
    assert_eq!(
        row.extra.get("new_list"),
        Some(&Value::List(vec![Value::Int(1), Value::Int(2)]))
    );
    assert_eq!(db.stats().unwrap().edges, 1);
    assert_eq!(db.begin().scan_prefix(Cf::In, &[]).unwrap().len(), 1);
    check_invariants(&db).unwrap();
}
