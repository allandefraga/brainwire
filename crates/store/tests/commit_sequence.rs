use brainwire_core::ids::TypeId;
use brainwire_store::db::{Database, Resources, StoreConfig, StoreError, COLUMN_FAMILIES};
use brainwire_store::txn::{Cf, TxnError};

const MARKER: &[u8] = b"commit/sequence";

fn raw_marker(path: &std::path::Path, value: Option<&[u8]>) {
    let raw = rocksdb::DB::open_cf(&rocksdb::Options::default(), path, COLUMN_FAMILIES).unwrap();
    let cf = raw.cf_handle("meta").unwrap();
    match value {
        Some(v) => raw.put_cf(cf, MARKER, v).unwrap(),
        None => raw.delete_cf(cf, MARKER).unwrap(),
    }
}

fn write(db: &Database, key: &[u8]) -> u64 {
    let mut tx = db.begin();
    tx.put(Cf::Note, key.to_vec(), b"value".to_vec());
    tx.commit().unwrap()
}

#[test]
fn missing_marker_is_untracked_until_a_write_and_reopen_continues_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(db.commit_sequence(), None);
    assert_eq!(db.begin().snapshot_commit_sequence(), None);
    assert_eq!(db.begin().commit().unwrap(), 0);
    let mut pending = db.begin();
    pending.put(Cf::Note, b"pending".to_vec(), b"v".to_vec());
    drop(pending);
    assert_eq!(db.commit_sequence(), None);
    assert_eq!(write(&db, b"a"), 1);
    assert_eq!(db.commit_sequence(), Some(1));
    drop(db);
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(db.commit_sequence(), Some(1));
    assert_eq!(write(&db, b"b"), 2);
    assert_eq!(db.begin().commit().unwrap(), 2);
    drop(db);
    raw_marker(dir.path(), None);
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(db.commit_sequence(), None);
    assert_eq!(
        db.begin().get(Cf::Note, b"a").unwrap(),
        Some(b"value".to_vec())
    );
    assert_eq!(write(&db, b"legacy-upgrade"), 1);
}

#[test]
fn snapshot_is_captured_at_begin_and_does_not_follow_pending_or_later_commits() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    let initial = db.begin();
    write(&db, b"a");
    let mut snapshot = db.begin();
    snapshot.put(Cf::Note, b"pending".to_vec(), b"v".to_vec());
    write(&db, b"b");
    assert_eq!(initial.snapshot_commit_sequence(), None);
    assert_eq!(snapshot.snapshot_commit_sequence(), Some(1));
    assert_eq!(db.begin().snapshot_commit_sequence(), Some(2));
    assert_eq!(snapshot.commit().unwrap(), 3);
    assert_eq!(initial.snapshot_commit_sequence(), None);
}

#[test]
fn malformed_or_unknown_version_marker_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    drop(Database::open(dir.path(), &res).unwrap());
    for marker in [vec![], vec![1], vec![1; 8], vec![1; 10], vec![2; 9]] {
        raw_marker(dir.path(), Some(&marker));
        assert!(matches!(
            Database::open(dir.path(), &res),
            Err(StoreError::Corrupt(_))
        ));
    }
}

#[test]
fn overflow_and_reserved_writes_fail_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    write(&db, b"seed");
    for delete in [false, true] {
        let mut tx = db.begin();
        tx.put(Cf::Note, b"companion".to_vec(), b"v".to_vec());
        if delete {
            tx.delete(Cf::Meta, MARKER.to_vec());
        } else {
            tx.put(Cf::Meta, MARKER.to_vec(), vec![1; 9]);
        }
        assert!(matches!(tx.commit(), Err(TxnError::Store(_))));
        assert_eq!(db.commit_sequence(), Some(1));
        assert_eq!(db.begin().get(Cf::Note, b"companion").unwrap(), None);
    }
    drop(db);
    let mut marker = vec![1];
    marker.extend_from_slice(&u64::MAX.to_be_bytes());
    raw_marker(dir.path(), Some(&marker));
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(db.commit_sequence(), Some(u64::MAX));
    assert_eq!(db.begin().commit().unwrap(), u64::MAX);
    let mut tx = db.begin();
    tx.put(Cf::Note, b"overflow".to_vec(), b"v".to_vec());
    assert!(matches!(tx.commit(), Err(TxnError::Store(_))));
    assert_eq!(db.commit_sequence(), Some(u64::MAX));
    assert_eq!(db.begin().get(Cf::Note, b"overflow").unwrap(), None);
}

#[test]
fn disjoint_concurrent_writers_commit_but_explicit_marker_dependencies_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let db = &db;
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut tx = db.begin();
                    tx.put(Cf::Note, vec![i], vec![i]);
                    barrier.wait();
                    tx.commit().unwrap()
                })
            })
            .collect();
        let mut seqs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        seqs.sort();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    });
    let mut point = db.begin();
    point.get_for_update(Cf::Meta, MARKER).unwrap();
    let mut prefix = db.begin();
    prefix.scan_prefix_for_update(Cf::Meta, b"commit/").unwrap();
    write(&db, b"later");
    assert!(matches!(point.commit(), Err(TxnError::Conflict)));
    assert!(matches!(prefix.commit(), Err(TxnError::Conflict)));
    assert_eq!(db.commit_sequence(), Some(5));
}

#[test]
fn allocator_reservation_is_committed_even_when_outer_transaction_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(dir.path(), &res).unwrap();
    let mut outer = db.begin();
    db.next_node_id(TypeId(1)).unwrap();
    assert_eq!(outer.snapshot_commit_sequence(), None);
    assert_eq!(db.commit_sequence(), Some(1));
    outer.put(Cf::Note, b"pending".to_vec(), b"v".to_vec());
    drop(outer);
    assert_eq!(db.commit_sequence(), Some(1));
    drop(db);
    let db = Database::open(dir.path(), &res).unwrap();
    assert_eq!(db.commit_sequence(), Some(1));
    assert_eq!(db.begin().get(Cf::Note, b"pending").unwrap(), None);
}

#[test]
fn checkpoint_retains_its_captured_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(&dir.path().join("live"), &res).unwrap();
    write(&db, b"a");
    let mut pending = db.begin();
    pending.put(Cf::Note, b"pending".to_vec(), b"v".to_vec());
    let captured = db
        .checkpoint_with_sequence(&dir.path().join("checkpoint"))
        .unwrap();
    assert_eq!(captured, Some(1));
    write(&db, b"b");
    let checkpoint = Database::open(&dir.path().join("checkpoint"), &res).unwrap();
    assert_eq!(checkpoint.commit_sequence(), captured);
    assert_eq!(
        checkpoint.begin().get(Cf::Note, b"a").unwrap(),
        Some(b"value".to_vec())
    );
    assert_eq!(checkpoint.begin().get(Cf::Note, b"b").unwrap(), None);
    assert_eq!(write(&checkpoint, b"c"), 2);
    assert_eq!(db.commit_sequence(), Some(2));
}

#[cfg(unix)]
#[test]
fn acknowledged_sequences_and_indexed_graph_writes_survive_sigkill() {
    use brainwire_core::value::Value;
    use brainwire_store::graph::{
        check_invariants, find_by_entity_id, find_by_indexed_property, out_edges,
    };
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    struct Writer(Option<Child>);
    impl Drop for Writer {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let mut recovered = 0;
    let mut last_ack = 0;
    for round in 0..3 {
        let first = round * 40;
        let mut writer = Writer(Some(
            Command::new(env!("CARGO_BIN_EXE_crash_writer"))
                .arg(dir.path())
                .arg(first.to_string())
                .arg("--sequence-ack")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        let stdout = writer.0.as_mut().unwrap().stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        for i in first..first + 40 {
            let line = receiver
                .recv_timeout(Duration::from_secs(30))
                .unwrap()
                .unwrap();
            let numbers: Vec<u64> = line
                .split_whitespace()
                .map(|v| v.parse().unwrap())
                .collect();
            assert_eq!(numbers.len(), 2);
            assert_eq!(numbers[0], i);
            assert!(numbers[1] > last_ack);
            assert!(numbers[1] > recovered);
            last_ack = numbers[1];
        }
        let child = writer.0.as_mut().unwrap();
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(9));
        writer.0.take();
        reader.join().unwrap();
        let db = Database::open(dir.path(), &res).unwrap();
        recovered = db.commit_sequence().unwrap();
        assert!(recovered >= last_ack);
        check_invariants(&db).unwrap();
        let catalog = db.catalog().unwrap().unwrap();
        let customer = catalog.node_type("Customer").unwrap();
        let tx = db.begin();
        for i in 0..first + 40 {
            let ids = find_by_entity_id(&tx, &format!("c:{i}"), Some(customer.id)).unwrap();
            assert_eq!(ids.len(), 1);
            assert_eq!(out_edges(&tx, ids[0]).unwrap().len(), 1);
            let indexed = find_by_indexed_property(
                &tx,
                customer,
                "segment",
                &Value::Str(format!("segment {}", i % 3)),
            )
            .unwrap()
            .unwrap();
            assert!(indexed.contains(&ids[0]));
        }
    }
}

#[test]
fn ontology_commits_are_tracked_and_sequences_are_per_store() {
    use brainwire_core::schema::GraphMapping;
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let first = Database::open(&dir.path().join("first"), &res).unwrap();
    let second = Database::open(&dir.path().join("second"), &res).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item","properties":[]}],"edges":[]}"#,
    )
    .unwrap();
    first.apply_ontology(&mapping).unwrap();
    assert_eq!(first.commit_sequence(), Some(1));
    assert_eq!(second.commit_sequence(), None);
    assert_eq!(write(&second, b"a"), 1);
    assert_eq!(write(&first, b"a"), 2);
    assert_eq!(second.commit_sequence(), Some(1));
}

#[test]
fn legacy_checkpoint_receipt_is_untracked_and_does_not_create_a_marker() {
    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let source = dir.path().join("legacy");
    drop(Database::open(&source, &res).unwrap());
    {
        let raw =
            rocksdb::DB::open_cf(&rocksdb::Options::default(), &source, COLUMN_FAMILIES).unwrap();
        raw.put_cf(raw.cf_handle("note").unwrap(), b"legacy-data", b"preserved")
            .unwrap();
    }
    let db = Database::open(&source, &res).unwrap();
    let destination = dir.path().join("checkpoint");
    assert_eq!(db.checkpoint_with_sequence(&destination).unwrap(), None);
    assert_eq!(db.commit_sequence(), None);
    let checkpoint = Database::open(&destination, &res).unwrap();
    assert_eq!(checkpoint.commit_sequence(), None);
    assert_eq!(
        checkpoint.begin().get(Cf::Note, b"legacy-data").unwrap(),
        Some(b"preserved".to_vec())
    );
}

#[test]
fn checkpoint_receipts_match_numbered_data_under_writer_contention() {
    use std::sync::mpsc::{self, TryRecvError};
    use std::sync::Barrier;

    let dir = tempfile::tempdir().unwrap();
    let res = Resources::new(StoreConfig::for_tests());
    let db = Database::open(&dir.path().join("live"), &res).unwrap();
    let mut seed = db.begin();
    seed.put(Cf::Note, b"number".to_vec(), 1u64.to_be_bytes().to_vec());
    assert_eq!(seed.commit().unwrap(), 1);
    let start = Barrier::new(2);
    std::thread::scope(|scope| {
        // The writer stays alive until every capture is checked. Disconnecting
        // the channel also stops it if an assertion in the reader panics.
        let (stop, stopped) = mpsc::channel::<()>();
        let db = &db;
        let start = &start;
        let writer = scope.spawn(move || {
            start.wait();
            let mut number = 2u64;
            while matches!(stopped.try_recv(), Err(TryRecvError::Empty)) {
                let mut tx = db.begin();
                tx.put(Cf::Note, b"number".to_vec(), number.to_be_bytes().to_vec());
                // Internal marker persistence must not consume a second commit.
                assert_eq!(tx.commit().unwrap(), number);
                number += 1;
            }
        });
        start.wait();
        for capture in 0..8 {
            let destination = dir.path().join(format!("checkpoint-{capture}"));
            let receipt = db.checkpoint_with_sequence(&destination).unwrap().unwrap();
            let checkpoint = Database::open(&destination, &res).unwrap();
            assert_eq!(checkpoint.commit_sequence(), Some(receipt));
            let read_only = checkpoint.begin();
            assert_eq!(read_only.snapshot_commit_sequence(), Some(receipt));
            let mut marker = vec![1];
            marker.extend_from_slice(&receipt.to_be_bytes());
            assert_eq!(read_only.get(Cf::Meta, MARKER).unwrap(), Some(marker));
            assert_eq!(
                read_only.get(Cf::Note, b"number").unwrap(),
                Some(receipt.to_be_bytes().to_vec()),
            );
            assert_eq!(read_only.commit().unwrap(), receipt);
            assert_eq!(checkpoint.commit_sequence(), Some(receipt));
        }
        stop.send(()).unwrap();
        writer.join().unwrap();
    });
}
