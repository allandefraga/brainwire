use brainwire_store::db::{Database, Resources, StoreConfig, COLUMN_FAMILIES};

#[test]
fn relationship_index_format_is_explicit_and_blocks_older_writers() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("graph");
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(&path, &resources).unwrap();
    assert!(!db.has_relationship_index_format());
    assert!(!db
        .storage_metrics()
        .unwrap()
        .contains_key("relationship_index_v1"));
    drop(db);
    drop(rocksdb::DB::open_cf(&rocksdb::Options::default(), &path, COLUMN_FAMILIES).unwrap());

    let mut db = Database::open(&path, &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    assert!(db.has_relationship_index_format());
    let metrics = db.storage_metrics().unwrap();
    assert_eq!(metrics["relationship_index_v1"]["estimated_keys"], Some(0));
    assert!(metrics["relationship_index_v1"]["memtable_bytes"].is_some());
    drop(db);
    assert!(rocksdb::DB::open_cf(&rocksdb::Options::default(), &path, COLUMN_FAMILIES).is_err());

    let reopened = Database::open(&path, &resources).unwrap();
    assert!(reopened.has_relationship_index_format());
}

#[test]
fn relationship_index_format_and_replication_are_mutually_exclusive() {
    use brainwire_store::replicated_apply::{self, ApplyError, ApplyLimits, Batch, Mutation};

    let temp = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let batch = Batch {
        version: 1,
        index: 1,
        mutations: vec![Mutation {
            cf: "meta".into(),
            key: b"test".to_vec(),
            value: Some(vec![1]),
        }],
    };
    let mut replica = Database::open(&temp.path().join("replica"), &resources).unwrap();
    replicated_apply::apply(&replica, &batch, ApplyLimits::default()).unwrap();
    assert!(replica
        .enable_relationship_index_format(&resources)
        .is_err());
    assert!(!replica.has_relationship_index_format());

    let mut indexed = Database::open(&temp.path().join("indexed"), &resources).unwrap();
    indexed
        .enable_relationship_index_format(&resources)
        .unwrap();
    assert!(matches!(
        replicated_apply::apply(&indexed, &batch, ApplyLimits::default()),
        Err(ApplyError::Invalid(
            "relationship indexes require standalone storage"
        ))
    ));
    assert!(matches!(
        indexed.begin().prepare_replication(ApplyLimits::default()),
        Err(ApplyError::Invalid(
            "relationship indexes require standalone storage"
        ))
    ));
    assert!(replicated_apply::applied_state(&indexed).unwrap().is_none());
}
