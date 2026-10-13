use brainwire_core::ids::TypeId;
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    replicated_apply::{self, ApplyError, ApplyLimits},
    txn::Cf,
};
#[test]
fn staged_node_and_edge_counters_do_not_commit_until_apply_and_survive_promotion_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    let mut tx = db.begin().with_replicated_ids();
    let a = tx.next_node_id(TypeId(1)).unwrap();
    let b = tx.next_node_id(TypeId(1)).unwrap();
    let edge = tx.next_edge_id().unwrap();
    assert_eq!((a.seq(), b.seq()), (0, 1));
    assert_eq!(edge.0, 1);
    assert_eq!(db.commit_sequence(), None);
    assert!(db
        .begin()
        .scan_prefix(Cf::Meta, b"seq/")
        .unwrap()
        .is_empty());
    let batch = tx
        .prepare_replication(ApplyLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(db.commit_sequence(), None);
    replicated_apply::apply(&db, &batch, ApplyLimits::default()).unwrap();
    let mut next = db.begin().with_replicated_ids();
    assert_eq!(next.next_node_id(TypeId(1)).unwrap().seq(), 2);
    assert_eq!(next.next_edge_id().unwrap().0, 2);
    let second = next
        .prepare_replication(ApplyLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(second.index, 2);
    replicated_apply::apply(&db, &second, ApplyLimits::default()).unwrap();
    drop(db);
    let db = Database::open(dir.path(), &resources).unwrap();
    let mut promoted = db.begin().with_replicated_ids();
    assert_eq!(promoted.next_node_id(TypeId(1)).unwrap().seq(), 3);
    assert_eq!(promoted.next_edge_id().unwrap().0, 3);
    drop(promoted);
    assert!(db.next_node_id(TypeId(1)).unwrap().seq() >= 3);
    assert!(db.next_edge_id().unwrap().0 >= 3);
}
#[test]
fn apply_invalidates_preexisting_standalone_allocator_cache() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    assert_eq!(db.next_node_id(TypeId(1)).unwrap().seq(), 0);
    let mut staged = db.begin().with_replicated_ids();
    assert_eq!(staged.next_node_id(TypeId(1)).unwrap().seq(), 4);
    let batch = staged
        .prepare_replication(ApplyLimits::default())
        .unwrap()
        .unwrap();
    replicated_apply::apply(&db, &batch, ApplyLimits::default()).unwrap();
    assert!(db.next_node_id(TypeId(1)).unwrap().seq() >= 5);
}
#[test]
fn prepare_rejects_late_mode_reserved_metadata_bounds_and_local_commit() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    let mut plain = db.begin();
    plain.put(Cf::Note, b"plain".to_vec(), b"v".to_vec());
    assert!(plain
        .with_replicated_ids()
        .prepare_replication(ApplyLimits::default())
        .is_err());
    let mut local = db.begin().with_replicated_ids();
    local.put(Cf::Note, b"local".to_vec(), b"v".to_vec());
    assert!(local.commit().is_err());
    let mut reserved = db.begin().with_replicated_ids();
    reserved.put(Cf::Meta, b"replicated-apply/user".to_vec(), b"v".to_vec());
    assert!(matches!(
        reserved.prepare_replication(ApplyLimits::default()),
        Err(ApplyError::ReservedMetadata)
    ));
    let mut bounded = db.begin().with_replicated_ids();
    bounded.put(Cf::Note, b"bound".to_vec(), b"long value".to_vec());
    assert!(matches!(
        bounded.prepare_replication(ApplyLimits {
            max_value_bytes: 1,
            ..ApplyLimits::default()
        }),
        Err(ApplyError::Bounds(_))
    ));
    assert_eq!(db.commit_sequence(), None);
    assert!(db.begin().scan_prefix(Cf::Note, &[]).unwrap().is_empty());
}
#[test]
fn malformed_or_exhausted_staged_id_counters_fail_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &resources).unwrap();
    let mut key = b"seq/node/".to_vec();
    key.extend_from_slice(&1u16.to_be_bytes());
    for value in [vec![1], (1u64 << 42).to_be_bytes().to_vec()] {
        let mut seed = db.begin();
        seed.put(Cf::Meta, key.clone(), value);
        seed.commit().unwrap();
        let mut tx = db.begin().with_replicated_ids();
        assert!(tx.next_node_id(TypeId(1)).is_err());
        assert!(tx
            .prepare_replication(ApplyLimits::default())
            .unwrap()
            .is_none());
    }
}
