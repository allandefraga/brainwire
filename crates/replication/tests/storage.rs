use brainwire_replication::{
    Application, Command, LogStore, Reply, StateMachine, Storage, TypeConfig,
};
use openraft::storage::{RaftLogStorage, RaftLogStorageExt, RaftStateMachine};
use openraft::testing::{StoreBuilder, Suite};
use openraft::{
    Entry, EntryPayload, LogId, Membership, RaftLogReader, RaftSnapshotBuilder, StorageError,
    StoredMembership, Vote,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tempfile::TempDir;
#[derive(Default)]
struct App {
    calls: AtomicUsize,
    fail_after_effect: AtomicBool,
    ledger: Mutex<BTreeMap<String, Reply>>,
}
impl Application for App {
    fn apply(&self, command: &Command) -> Result<Reply, String> {
        let mut ledger = self.ledger.lock().unwrap();
        let reply = ledger
            .entry(command.request_id().into())
            .or_insert_with(|| {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Reply::Applied { revision: 1 }
            })
            .clone();
        if self.fail_after_effect.swap(false, Ordering::SeqCst) {
            return Err("injected failure after graph durable effect".into());
        }
        Ok(reply)
    }
}
fn entry(index: u64, command: Command) -> Entry<TypeConfig> {
    Entry {
        log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), index),
        payload: EntryPayload::Normal(command),
    }
}
fn command(id: &str) -> Command {
    Command::Write {
        request_id: id.into(),
        graph: "g".into(),
        expected_revision: 0,
        batch: vec![1, 2],
    }
}

#[tokio::test]
async fn append_rejects_invalid_commands_without_persisting_a_valid_prefix() {
    for invalid in [
        command(""),
        Command::Write {
            request_id: "oversized".into(),
            graph: "g".into(),
            expected_revision: 0,
            batch: vec![255; 600_000],
        },
    ] {
        assert!(invalid.validate().is_err());
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 8 * 1024 * 1024, app.clone()).unwrap();
        let mut logs = store.log_store();
        let first = entry(0, command("first"));
        logs.blocking_append([first.clone()]).await.unwrap();
        let bytes = store.log_bytes().unwrap();
        let second = entry(1, command("second"));
        assert!(logs
            .blocking_append([second.clone(), entry(2, invalid)])
            .await
            .is_err());
        assert_eq!(store.log_bytes().unwrap(), bytes);
        assert_eq!(
            logs.try_get_log_entries(..).await.unwrap(),
            vec![first.clone()]
        );
        assert_eq!(app.calls.load(Ordering::SeqCst), 0);
        drop(logs);
        drop(store);
        let store = Storage::open(dir.path(), 8 * 1024 * 1024, app).unwrap();
        assert_eq!(store.log_bytes().unwrap(), bytes);
        let mut logs = store.log_store();
        logs.blocking_append([second.clone()]).await.unwrap();
        assert_eq!(
            logs.try_get_log_entries(..).await.unwrap(),
            vec![first, second]
        );
    }
}

#[test]
fn recovery_rejects_invalid_retained_commands_without_rewriting_them() {
    for invalid in [
        command("bad space"),
        Command::Write {
            request_id: "oversized".into(),
            graph: "g".into(),
            expected_revision: 0,
            batch: vec![255; 600_000],
        },
    ] {
        assert!(invalid.validate().is_err());
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        drop(Storage::open(dir.path(), 8 * 1024 * 1024, app.clone()).unwrap());
        let encoded = serde_json::to_vec(&entry(0, invalid)).unwrap();
        assert!(encoded.len() < brainwire_replication::MAX_ENTRY_BYTES);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        let mut key = [b'L'; 9];
        key[1..].copy_from_slice(&0_u64.to_be_bytes());
        db.put(key, &encoded).unwrap();
        db.put(
            b"Mbytes",
            serde_json::to_vec(&(encoded.len() as u64)).unwrap(),
        )
        .unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        assert!(Storage::open(dir.path(), 8 * 1024 * 1024, app.clone()).is_err());
        assert_eq!(app.calls.load(Ordering::SeqCst), 0);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        assert_eq!(db.get(key).unwrap().unwrap(), encoded);
    }
}

#[tokio::test]
async fn vote_updates_cannot_regress_persisted_term_leader_or_commit() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    assert_eq!(logs.read_vote().await.unwrap(), None);
    let initial = Vote::new(3, 2);
    logs.save_vote(&initial).await.unwrap();
    logs.save_vote(&initial).await.unwrap();
    let committed = Vote::new_committed(3, 2);
    logs.save_vote(&committed).await.unwrap();
    for invalid in [Vote::new(2, 9), Vote::new(3, 1), initial] {
        assert!(logs.save_vote(&invalid).await.is_err());
        assert_eq!(logs.read_vote().await.unwrap(), Some(committed));
    }
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
    let mut logs = store.log_store();
    assert_eq!(logs.read_vote().await.unwrap(), Some(committed));
    // OpenRaft's configured leader ordering includes the node ID within a term.
    let higher_leader = Vote::new(3, 3);
    logs.save_vote(&higher_leader).await.unwrap();
    assert_eq!(logs.read_vote().await.unwrap(), Some(higher_leader));
    let next = Vote::new(4, 1);
    logs.save_vote(&next).await.unwrap();
    assert_eq!(logs.read_vote().await.unwrap(), Some(next));
}

#[tokio::test]
async fn append_rejects_same_term_leader_regression_without_partial_write() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    let mut first = entry(0, command("first"));
    first.log_id.leader_id = openraft::CommittedLeaderId::new(2, 3);
    logs.blocking_append([first.clone()]).await.unwrap();
    let bytes = store.log_bytes().unwrap();
    let mut second = entry(1, command("second"));
    second.log_id.leader_id = openraft::CommittedLeaderId::new(2, 4);
    let mut regressed = entry(2, command("regressed"));
    regressed.log_id.leader_id = openraft::CommittedLeaderId::new(2, 2);
    for invalid in [
        vec![Entry {
            log_id: LogId::new(regressed.log_id.leader_id, 1),
            ..regressed.clone()
        }],
        vec![second.clone(), regressed],
    ] {
        assert!(logs.blocking_append(invalid).await.is_err());
        assert_eq!(store.log_bytes().unwrap(), bytes);
        assert_eq!(
            logs.try_get_log_entries(..).await.unwrap(),
            vec![first.clone()]
        );
    }
    logs.blocking_append([second.clone()]).await.unwrap();
    logs.blocking_append([first.clone()]).await.unwrap();
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
    assert_eq!(
        store.log_store().try_get_log_entries(..).await.unwrap(),
        vec![first, second]
    );
}

#[tokio::test]
async fn recovery_rejects_same_term_leader_regression() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    drop(store);
    let db = rocksdb::DB::open_default(dir.path()).unwrap();
    let mut bytes = 0_u64;
    for (index, node_id) in [(0, 3), (1, 2)] {
        let mut value = entry(index, command("corrupt"));
        value.log_id.leader_id = openraft::CommittedLeaderId::new(2, node_id);
        let encoded = serde_json::to_vec(&value).unwrap();
        bytes += encoded.len() as u64;
        let mut key = [b'L'; 9];
        key[1..].copy_from_slice(&index.to_be_bytes());
        db.put(key, encoded).unwrap();
    }
    db.put(b"Mbytes", serde_json::to_vec(&bytes).unwrap())
        .unwrap();
    db.flush_wal(true).unwrap();
    drop(db);
    assert!(Storage::open(dir.path(), 1024 * 1024, app).is_err());
}

#[tokio::test]
async fn committed_marker_rejects_missing_mismatched_and_regressing_logs_before_persistence() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    logs.save_committed(None).await.unwrap();
    let first = entry(0, command("first"));
    let second = entry(1, command("second"));
    assert!(logs.save_committed(Some(first.log_id)).await.is_err());
    assert_eq!(store.committed().unwrap(), None);
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    logs.blocking_append([first.clone(), second.clone()])
        .await
        .unwrap();
    let mut mismatched = second.log_id;
    mismatched.leader_id.term += 1;
    assert!(logs.save_committed(Some(mismatched)).await.is_err());
    assert_eq!(store.committed().unwrap(), None);
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    logs.save_committed(Some(first.log_id)).await.unwrap();
    logs.save_committed(Some(first.log_id)).await.unwrap();
    logs.save_committed(Some(second.log_id)).await.unwrap();
    for invalid in [
        None,
        Some(first.log_id),
        Some(mismatched),
        Some(entry(2, command("missing")).log_id),
    ] {
        assert!(logs.save_committed(invalid).await.is_err());
        assert_eq!(store.committed().unwrap(), Some(second.log_id));
    }
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
    assert_eq!(store.committed().unwrap(), Some(second.log_id));
    assert!(store.log_store().truncate(second.log_id).await.is_err());
}
#[tokio::test]
async fn durable_vote_log_committed_membership_dedupe_and_conflicting_retry() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    let mut sm = store.state_machine();
    let membership = Entry {
        log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 0),
        payload: EntryPayload::Membership(Membership::new(
            vec![[1, 2, 3].into_iter().collect()],
            None,
        )),
    };
    let first = entry(1, command("same-id"));
    let duplicate = entry(2, command("same-id"));
    let mut changed = command("same-id");
    if let Command::Write { batch, .. } = &mut changed {
        batch.push(3);
    }
    let conflict = entry(3, changed);
    logs.save_vote(&Vote::new_committed(2, 1)).await.unwrap();
    logs.blocking_append([
        membership.clone(),
        first.clone(),
        duplicate.clone(),
        conflict.clone(),
    ])
    .await
    .unwrap();
    logs.save_committed(Some(conflict.log_id)).await.unwrap();
    let replies = sm
        .apply([membership, first, duplicate, conflict.clone()])
        .await
        .unwrap();
    assert_eq!(
        replies,
        [
            Reply::Initialized,
            Reply::Applied { revision: 1 },
            Reply::Applied { revision: 1 },
            Reply::Conflict
        ]
    );
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    let bytes = store.log_bytes().unwrap();
    drop(sm);
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    assert_eq!(
        logs.read_vote().await.unwrap(),
        Some(Vote::new_committed(2, 1))
    );
    assert_eq!(store.committed().unwrap(), Some(conflict.log_id));
    assert_eq!(store.last_applied().unwrap(), Some(conflict.log_id));
    assert_eq!(
        store.lookup("same-id").unwrap(),
        Some(Reply::Applied { revision: 1 })
    );
    assert_eq!(store.lookup("absent").unwrap(), None);
    assert_eq!(store.log_bytes().unwrap(), bytes);
    assert_eq!(logs.try_get_log_entries(0..4).await.unwrap().len(), 4);
    assert_eq!(
        store
            .membership()
            .unwrap()
            .membership()
            .voter_ids()
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(logs.truncate(conflict.log_id).await.is_err());
}
#[tokio::test]
async fn invalid_apply_batch_has_no_graph_effects_and_valid_log_replays_after_reopen() {
    for corruption in [
        "origin",
        "gap",
        "duplicate",
        "leader",
        "leader-id",
        "command",
    ] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        let retained = vec![entry(0, command("first")), entry(1, command("second"))];
        store
            .log_store()
            .blocking_append(retained.clone())
            .await
            .unwrap();
        let mut invalid = retained.clone();
        match corruption {
            "origin" => invalid = vec![retained[1].clone()],
            "gap" => invalid[1].log_id.index = 2,
            "duplicate" => invalid[1].log_id.index = 0,
            "leader" => invalid[1].log_id.leader_id = openraft::CommittedLeaderId::new(0, 1),
            "leader-id" => invalid[1].log_id.leader_id = openraft::CommittedLeaderId::new(1, 0),
            "command" => {
                invalid[1].payload = EntryPayload::Normal(command(""));
            }
            _ => unreachable!(),
        }
        let mut sm = store.state_machine();
        assert!(sm.apply(invalid).await.is_err(), "{corruption}");
        assert_eq!(app.calls.load(Ordering::SeqCst), 0, "{corruption}");
        assert_eq!(store.last_applied().unwrap(), None);
        assert_eq!(store.lookup("first").unwrap(), None);
        // An invalid batch still stops this runtime, but leaves durable replay intact.
        assert!(sm.apply(retained.clone()).await.is_err());
        drop(sm);
        drop(store);
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        assert_eq!(
            store.state_machine().apply(retained).await.unwrap().len(),
            2
        );
        assert_eq!(app.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn apply_preflight_checks_the_previously_applied_boundary() {
    for corruption in ["duplicate", "gap", "leader", "leader-id"] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        let first = entry(0, command("first"));
        let second = entry(1, command("second"));
        store
            .log_store()
            .blocking_append([first.clone(), second.clone()])
            .await
            .unwrap();
        store.state_machine().apply([first.clone()]).await.unwrap();
        let mut invalid = second.clone();
        match corruption {
            "duplicate" => invalid.log_id.index = 0,
            "gap" => invalid.log_id.index = 2,
            "leader" => invalid.log_id.leader_id = openraft::CommittedLeaderId::new(0, 1),
            "leader-id" => invalid.log_id.leader_id = openraft::CommittedLeaderId::new(1, 0),
            _ => unreachable!(),
        }
        assert!(store.state_machine().apply([invalid]).await.is_err());
        assert_eq!(app.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.last_applied().unwrap(), Some(first.log_id));
        assert_eq!(store.lookup("second").unwrap(), None);
        drop(store);
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        store.state_machine().apply([second]).await.unwrap();
        assert_eq!(app.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn crash_window_replay_uses_application_ledger_and_fatal_runtime_stops() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    app.fail_after_effect.store(true, Ordering::SeqCst);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    let mut sm = store.state_machine();
    let e = entry(0, command("replay"));
    logs.blocking_append([e.clone()]).await.unwrap();
    logs.save_committed(Some(e.log_id)).await.unwrap();
    assert!(sm.apply([e.clone()]).await.is_err());
    assert_eq!(store.last_applied().unwrap(), None);
    assert_eq!(store.lookup("replay").unwrap(), None);
    assert!(sm.apply([e.clone()]).await.is_err());
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    drop(logs);
    drop(sm);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    assert_eq!(
        store.state_machine().apply([e]).await.unwrap(),
        vec![Reply::Applied { revision: 1 }]
    );
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    assert!(store.lookup("replay").unwrap().is_some());
}
#[tokio::test]
async fn cap_holes_and_unsupported_snapshots_fail_without_mutation() {
    let dir = TempDir::new().unwrap();
    let e = entry(0, command("one"));
    let size = serde_json::to_vec(&e).unwrap().len() as u64;
    let store = Storage::open(dir.path(), size, Arc::new(App::default())).unwrap();
    let mut logs = store.log_store();
    let mut sm = store.state_machine();
    logs.blocking_append([e.clone()]).await.unwrap();
    assert!(logs
        .blocking_append([entry(1, command("two"))])
        .await
        .is_err());
    assert_eq!(store.log_bytes().unwrap(), size);
    assert!(logs
        .blocking_append([entry(2, command("hole"))])
        .await
        .is_err());
    assert_eq!(logs.try_get_log_entries(..).await.unwrap(), vec![e.clone()]);
    assert!(logs.purge(e.log_id).await.is_err());
    assert!(sm.build_snapshot().await.is_err());
    assert!(sm.begin_receiving_snapshot().await.is_err());
    assert!(sm.get_current_snapshot().await.unwrap().is_none());
    logs.truncate(e.log_id).await.unwrap();
    assert_eq!(store.log_bytes().unwrap(), 0);
    assert!(logs.try_get_log_entries(..).await.unwrap().is_empty());
}
struct Builder;
impl StoreBuilder<TypeConfig, LogStore, StateMachine, TempDir> for Builder {
    async fn build(&self) -> Result<(TempDir, LogStore, StateMachine), StorageError<u64>> {
        let dir = TempDir::new().unwrap();
        let store = Storage::open(dir.path(), 1024 * 1024, Arc::new(App::default()))?;
        Ok((dir, store.log_store(), store.state_machine()))
    }
}
type Official = Suite<TypeConfig, LogStore, StateMachine, Builder, TempDir>;
#[tokio::test]
async fn official_supported_storage_conformance_cases() {
    // The full suite requires purging and snapshots, deliberately unsupported here.
    let (_g, l, s) = Builder.build().await.unwrap();
    Official::save_vote(l, s).await.unwrap();
    let (_g, l, s) = Builder.build().await.unwrap();
    Official::get_log_entries(l, s).await.unwrap();
    let (_g, l, s) = Builder.build().await.unwrap();
    Official::limited_get_log_entries(l, s).await.unwrap();
    let (_g, l, s) = Builder.build().await.unwrap();
    Official::apply_single(l, s).await.unwrap();
    let (_g, l, s) = Builder.build().await.unwrap();
    Official::apply_multiple(l, s).await.unwrap();
}
#[test]
fn command_validation_and_digest_include_graph_variant_and_request_identity() {
    assert!(command("").validate().is_err());
    assert!(command("bad space").validate().is_err());
    assert!(command("id:ok-1").validate().is_ok());
    assert_ne!(
        command("a").digest().unwrap(),
        command("b").digest().unwrap()
    );
    let c = Command::CreateGraph {
        request_id: "create".into(),
        graph: "new".into(),
        schema: b"{}".to_vec(),
    };
    assert!(c.validate().is_ok());
    assert_eq!(c.request_id(), "create");
    let large = Command::Write {
        request_id: "huge".into(),
        graph: "g".into(),
        expected_revision: 0,
        batch: vec![255; 1024 * 1024],
    };
    assert!(large.validate().is_err());
}

#[tokio::test]
async fn replication_reader_bounds_serialized_batches_and_handles_max_exclusive() {
    let dir = TempDir::new().unwrap();
    let store = Storage::open(dir.path(), 8 * 1024 * 1024, Arc::new(App::default())).unwrap();
    let mut logs = store.log_store();
    let entries = (0..3)
        .map(|i| {
            entry(
                i,
                Command::Write {
                    request_id: format!("large-{i}"),
                    graph: "g".into(),
                    expected_revision: 0,
                    batch: vec![255; 400_000],
                },
            )
        })
        .collect::<Vec<_>>();
    logs.blocking_append(entries).await.unwrap();
    let limited = logs.limited_get_log_entries(0, 3).await.unwrap();
    assert_eq!(limited.len(), 2);
    assert!(
        limited
            .iter()
            .map(|e| serde_json::to_vec(e).unwrap().len())
            .sum::<usize>()
            <= brainwire_replication::MAX_ENTRY_BYTES
    );
    assert_eq!(logs.try_get_log_entries(0..3).await.unwrap().len(), 3);
    assert!(logs
        .try_get_log_entries((
            std::ops::Bound::Excluded(u64::MAX),
            std::ops::Bound::Unbounded
        ))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        logs.get_log_state()
            .await
            .unwrap()
            .last_log_id
            .unwrap()
            .index,
        2
    );
}
#[tokio::test]
async fn corrupt_durable_accounting_is_not_silently_reset_on_reopen() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    store
        .log_store()
        .blocking_append([entry(0, command("one"))])
        .await
        .unwrap();
    drop(store);
    let db = rocksdb::DB::open_default(dir.path()).unwrap();
    db.put(b"Mbytes", b"0").unwrap();
    db.flush_wal(true).unwrap();
    drop(db);
    assert!(Storage::open(dir.path(), 1024 * 1024, app).is_err());
}

struct FixedReply(Reply);
#[test]
fn recovery_rejects_malformed_vote_without_rewriting_it() {
    for bytes in [b"{".as_slice(), b"null".as_slice(), b"true".as_slice()] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        drop(Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap());
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        db.put(b"Mvote", bytes).unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        assert!(Storage::open(dir.path(), 1024 * 1024, app).is_err());
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        assert_eq!(db.get(b"Mvote").unwrap().unwrap(), bytes);
    }
}

impl Application for FixedReply {
    fn apply(&self, _: &Command) -> Result<Reply, String> {
        Ok(self.0.clone())
    }
}
#[tokio::test]
async fn registry_initialization_proof_is_durable_only_for_new_successful_registry_commands() {
    for registry_command in [
        Command::Bootstrap {
            request_id: "bootstrap".into(),
            graphs: BTreeMap::new(),
        },
        Command::CreateGraph {
            request_id: "create".into(),
            graph: "g".into(),
            schema: b"{}".to_vec(),
        },
        command("write"),
    ] {
        for reply in [Reply::Initialized, Reply::Conflict] {
            let dir = TempDir::new().unwrap();
            let app = Arc::new(FixedReply(reply.clone()));
            let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
            assert!(!store.registry_initialized().unwrap());
            let e = entry(0, registry_command.clone());
            store
                .log_store()
                .blocking_append([e.clone()])
                .await
                .unwrap();
            assert!(!store.registry_initialized().unwrap());
            assert_eq!(
                store.state_machine().apply([e.clone()]).await.unwrap(),
                vec![reply.clone()]
            );
            let expected =
                reply == Reply::Initialized && !matches!(registry_command, Command::Write { .. });
            assert_eq!(store.registry_initialized().unwrap(), expected);
            drop(store);
            let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
            assert_eq!(store.registry_initialized().unwrap(), expected);
            assert_eq!(store.last_applied().unwrap(), Some(e.log_id));
            assert_eq!(
                store.lookup(registry_command.request_id()).unwrap(),
                Some(reply.clone())
            );
            let duplicate = entry(1, registry_command.clone());
            store
                .log_store()
                .blocking_append([duplicate.clone()])
                .await
                .unwrap();
            assert_eq!(
                store.state_machine().apply([duplicate]).await.unwrap(),
                vec![reply]
            );
            assert_eq!(store.registry_initialized().unwrap(), expected);
            if matches!(registry_command, Command::Write { .. }) {
                let divergent = entry(
                    2,
                    Command::CreateGraph {
                        request_id: registry_command.request_id().into(),
                        graph: "g".into(),
                        schema: b"{}".to_vec(),
                    },
                );
                store
                    .log_store()
                    .blocking_append([divergent.clone()])
                    .await
                    .unwrap();
                assert_eq!(
                    store.state_machine().apply([divergent]).await.unwrap(),
                    vec![Reply::Conflict]
                );
                assert!(!store.registry_initialized().unwrap());
            }
        }
    }
}

#[tokio::test]
async fn retained_log_capacity_is_atomic_at_boundary_and_can_increase_on_reopen() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let first = entry(0, command("capacity-first"));
    let second = entry(1, command("capacity-second"));
    let third = entry(2, command("capacity-third"));
    let bytes = |entry: &Entry<TypeConfig>| serde_json::to_vec(entry).unwrap().len() as u64;
    let first_bytes = bytes(&first);
    let cap = first_bytes + bytes(&second);
    let store = Storage::open(dir.path(), cap, app.clone()).unwrap();
    assert_eq!(store.max_log_bytes(), cap);
    let mut logs = store.log_store();
    logs.blocking_append([first.clone()]).await.unwrap();
    logs.save_committed(Some(first.log_id)).await.unwrap();
    store.state_machine().apply([first.clone()]).await.unwrap();

    // The second entry fits, but the entire append must fail when the third exceeds capacity.
    assert!(logs
        .blocking_append([second.clone(), third.clone()])
        .await
        .is_err());
    assert_eq!(
        logs.try_get_log_entries(..).await.unwrap(),
        vec![first.clone()]
    );
    assert_eq!(store.log_bytes().unwrap(), first_bytes);
    assert_eq!(store.committed().unwrap(), Some(first.log_id));
    assert_eq!(store.last_applied().unwrap(), Some(first.log_id));
    assert_eq!(
        store.lookup("capacity-first").unwrap(),
        Some(Reply::Applied { revision: 1 })
    );
    assert_eq!(store.lookup("capacity-second").unwrap(), None);
    assert_eq!(store.lookup("capacity-third").unwrap(), None);

    logs.blocking_append([second.clone()]).await.unwrap();
    logs.save_committed(Some(second.log_id)).await.unwrap();
    store.state_machine().apply([second.clone()]).await.unwrap();
    assert_eq!(store.log_bytes().unwrap(), cap);
    logs.blocking_append([first.clone(), second.clone()])
        .await
        .unwrap();
    assert_eq!(store.log_bytes().unwrap(), cap);
    assert_eq!(store.committed().unwrap(), Some(second.log_id));
    assert_eq!(store.last_applied().unwrap(), Some(second.log_id));
    assert_eq!(app.calls.load(Ordering::SeqCst), 2);
    drop(logs);
    drop(store);

    let store = Storage::open(dir.path(), cap, app.clone()).unwrap();
    assert_eq!(store.log_bytes().unwrap(), cap);
    assert_eq!(
        store.log_store().try_get_log_entries(..).await.unwrap(),
        vec![first.clone(), second.clone()]
    );
    drop(store);
    assert!(Storage::open(dir.path(), cap - 1, app.clone()).is_err());

    let higher_cap = cap + bytes(&third);
    let store = Storage::open(dir.path(), higher_cap, app).unwrap();
    assert_eq!(store.max_log_bytes(), higher_cap);
    store
        .log_store()
        .blocking_append([third.clone()])
        .await
        .unwrap();
    assert_eq!(store.log_bytes().unwrap(), higher_cap);
    assert_eq!(
        store.log_store().try_get_log_entries(..).await.unwrap(),
        vec![first, second.clone(), third]
    );
    assert_eq!(store.committed().unwrap(), Some(second.log_id));
    assert_eq!(store.last_applied().unwrap(), Some(second.log_id));
}

#[tokio::test]
async fn recovery_rejects_membership_marker_divergence_from_applied_history() {
    for corruption in [
        "missing",
        "changed",
        "stale",
        "future",
        "wrong-entry",
        "no-applied",
        "no-log-id",
    ] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        let membership = |index, ids: &[u64]| Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), index),
            payload: EntryPayload::Membership(Membership::new(
                vec![ids.iter().copied().collect()],
                None,
            )),
        };
        let first = membership(0, &[1, 2, 3]);
        let current = membership(2, &[1, 2, 4]);
        let pending = membership(4, &[1, 2, 5]);
        let applied = vec![
            first.clone(),
            entry(1, command("first")),
            current.clone(),
            entry(3, command("second")),
        ];
        let mut retained = applied.clone();
        retained.push(pending.clone());
        store.log_store().blocking_append(retained).await.unwrap();
        store.state_machine().apply(applied).await.unwrap();
        drop(store);

        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        match corruption {
            "missing" => db.delete(b"Mmembership").unwrap(),
            "no-applied" => db.delete(b"Mapplied").unwrap(),
            _ => {
                let (log_id, source) = match corruption {
                    "changed" => (current.log_id, &first),
                    "stale" => (first.log_id, &first),
                    "future" => (pending.log_id, &pending),
                    "no-log-id" => (current.log_id, &current),
                    "wrong-entry" => (
                        LogId::new(openraft::CommittedLeaderId::new(1, 1), 3),
                        &current,
                    ),
                    _ => unreachable!(),
                };
                let EntryPayload::Membership(configuration) = &source.payload else {
                    unreachable!()
                };
                let value = StoredMembership::new(
                    (corruption != "no-log-id").then_some(log_id),
                    configuration.clone(),
                );
                db.put(b"Mmembership", serde_json::to_vec(&value).unwrap())
                    .unwrap();
            }
        }
        db.flush_wal(true).unwrap();
        drop(db);
        assert!(
            Storage::open(dir.path(), 1024 * 1024, app).is_err(),
            "accepted {corruption} membership marker"
        );
    }
}

#[tokio::test]
async fn recovery_preserves_applied_membership_and_ignores_pending_membership() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    assert_eq!(store.membership().unwrap(), StoredMembership::default());
    let configuration = Membership::new(vec![[1, 2, 3].into_iter().collect()], None);
    let first = Entry {
        log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 0),
        payload: EntryPayload::Membership(configuration.clone()),
    };
    let pending = Entry {
        log_id: LogId::new(openraft::CommittedLeaderId::new(2, 2), 2),
        payload: EntryPayload::Membership(Membership::new(
            vec![[1, 2, 4].into_iter().collect()],
            None,
        )),
    };
    let applied = vec![first.clone(), entry(1, command("one"))];
    let mut retained = applied.clone();
    retained.push(pending);
    store.log_store().blocking_append(retained).await.unwrap();
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    assert_eq!(store.membership().unwrap(), StoredMembership::default());
    store.state_machine().apply(applied).await.unwrap();
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
    assert_eq!(
        store.membership().unwrap(),
        StoredMembership::new(Some(first.log_id), configuration)
    );
}

#[tokio::test]
async fn recovery_rejects_missing_retained_log_prefix_even_with_matching_accounting() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let entries = vec![entry(0, command("first")), entry(1, command("second"))];
    store
        .log_store()
        .blocking_append(entries.clone())
        .await
        .unwrap();
    drop(store);
    let db = rocksdb::DB::open_default(dir.path()).unwrap();
    let mut key = [b'L'; 9];
    key[1..].copy_from_slice(&0_u64.to_be_bytes());
    db.delete(key).unwrap();
    let remaining_bytes = serde_json::to_vec(&entries[1]).unwrap().len() as u64;
    db.put(b"Mbytes", serde_json::to_vec(&remaining_bytes).unwrap())
        .unwrap();
    db.flush_wal(true).unwrap();
    drop(db);
    assert!(Storage::open(dir.path(), 1024 * 1024, app).is_err());
}

#[tokio::test]
async fn recovery_rejects_term_regression_but_accepts_new_leader_terms() {
    for last_term in [1, 2, 3] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        let mut entries = vec![entry(0, command("first")), entry(1, command("second"))];
        entries[0].log_id = LogId::new(openraft::CommittedLeaderId::new(2, 1), 0);
        entries[1].log_id = LogId::new(
            openraft::CommittedLeaderId::new(last_term, if last_term > 2 { 2 } else { 1 }),
            1,
        );
        // Inject persisted corruption, bypassing the append-time invariant checks.
        drop(store);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        let mut bytes = 0_u64;
        for entry in entries {
            let value = serde_json::to_vec(&entry).unwrap();
            bytes += value.len() as u64;
            let mut key = [b'L'; 9];
            key[1..].copy_from_slice(&entry.log_id.index.to_be_bytes());
            db.put(key, value).unwrap();
        }
        db.put(b"Mbytes", serde_json::to_vec(&bytes).unwrap())
            .unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        let reopened = Storage::open(dir.path(), 1024 * 1024, app);
        assert_eq!(reopened.is_ok(), last_term >= 2, "last term {last_term}");
    }
}

struct DurableApp {
    db: rocksdb::DB,
    fail_after_commit: bool,
}
impl Application for DurableApp {
    fn apply(&self, command: &Command) -> Result<Reply, String> {
        let key = command.request_id().as_bytes();
        if let Some(bytes) = self.db.get(key).map_err(|e| e.to_string())? {
            return serde_json::from_slice(&bytes).map_err(|e| e.to_string());
        }
        let effects = self
            .db
            .get(b"\0effects")
            .map_err(|e| e.to_string())?
            .map(|bytes| u64::from_be_bytes(bytes.try_into().unwrap()))
            .unwrap_or(0)
            + 1;
        let reply = Reply::Applied { revision: effects };
        let mut batch = rocksdb::WriteBatch::default();
        batch.put(key, serde_json::to_vec(&reply).unwrap());
        batch.put(b"\0effects", effects.to_be_bytes());
        let mut options = rocksdb::WriteOptions::default();
        options.set_sync(true);
        self.db
            .write_opt(batch, &options)
            .map_err(|e| e.to_string())?;
        if self.fail_after_commit {
            return Err("injected interruption after application WAL commit".into());
        }
        Ok(reply)
    }
}

#[tokio::test]
async fn durable_application_reopen_reconciles_crash_before_consensus_apply_marker() {
    let dir = TempDir::new().unwrap();
    let application_path = dir.path().join("application");
    let consensus_path = dir.path().join("consensus");
    let command = command("durable-replay");
    let committed = entry(0, command.clone());
    {
        let app = Arc::new(DurableApp {
            db: rocksdb::DB::open_default(&application_path).unwrap(),
            fail_after_commit: true,
        });
        let store = Storage::open(&consensus_path, 1024 * 1024, app.clone()).unwrap();
        let mut logs = store.log_store();
        logs.blocking_append([committed.clone()]).await.unwrap();
        logs.save_committed(Some(committed.log_id)).await.unwrap();
        assert!(store
            .state_machine()
            .apply([committed.clone()])
            .await
            .is_err());
        assert_eq!(store.last_applied().unwrap(), None);
        assert_eq!(store.lookup(command.request_id()).unwrap(), None);
        assert!(app.db.get(command.request_id()).unwrap().is_some());
    }
    // Both independent RocksDB handles are now recreated from durable state.
    let app = Arc::new(DurableApp {
        db: rocksdb::DB::open_default(&application_path).unwrap(),
        fail_after_commit: false,
    });
    let store = Storage::open(&consensus_path, 1024 * 1024, app.clone()).unwrap();
    assert_eq!(store.committed().unwrap(), Some(committed.log_id));
    assert_eq!(store.last_applied().unwrap(), None);
    assert_eq!(
        store
            .state_machine()
            .apply([committed.clone()])
            .await
            .unwrap(),
        vec![Reply::Applied { revision: 1 }]
    );
    assert_eq!(store.last_applied().unwrap(), Some(committed.log_id));
    assert_eq!(
        store.lookup(command.request_id()).unwrap(),
        Some(Reply::Applied { revision: 1 })
    );
    let duplicate = entry(1, command);
    store
        .log_store()
        .blocking_append([duplicate.clone()])
        .await
        .unwrap();
    assert_eq!(
        store.state_machine().apply([duplicate]).await.unwrap(),
        vec![Reply::Applied { revision: 1 }]
    );
    assert_eq!(app.db.iterator(rocksdb::IteratorMode::Start).count(), 2);
    assert_eq!(
        app.db.get(b"\0effects").unwrap().unwrap(),
        1_u64.to_be_bytes()
    );
}

#[cfg(unix)]
struct CrashAfterApplicationCommit {
    application: DurableApp,
    ready: std::path::PathBuf,
}
#[cfg(unix)]
impl Application for CrashAfterApplicationCommit {
    fn apply(&self, command: &Command) -> Result<Reply, String> {
        use std::io::Write;
        self.application.apply(command)?;
        let mut signal = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.ready.with_extension("pending"))
            .map_err(|e| e.to_string())?;
        signal
            .write_all(b"application-committed")
            .map_err(|e| e.to_string())?;
        signal.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(self.ready.with_extension("pending"), &self.ready)
            .map_err(|e| e.to_string())?;
        loop {
            std::thread::park();
        }
    }
}

#[cfg(unix)]
#[test]
fn crash_recovery_child() {
    let Some(root) = std::env::var_os("BRAINWIRE_RECOVERY_CHILD_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let app = Arc::new(CrashAfterApplicationCommit {
        application: DurableApp {
            db: rocksdb::DB::open_default(root.join("application")).unwrap(),
            fail_after_commit: false,
        },
        ready: root.join("ready"),
    });
    let store = Storage::open(root.join("consensus"), 1024 * 1024, app).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let committed = entry(0, command("process-crash"));
        let mut logs = store.log_store();
        logs.blocking_append([committed.clone()]).await.unwrap();
        logs.save_committed(Some(committed.log_id)).await.unwrap();
        store.state_machine().apply([committed]).await.unwrap();
    });
    panic!("child must block after application commit");
}

#[cfg(unix)]
#[tokio::test]
async fn process_kill_after_application_commit_recovers_once_from_independent_stores() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = TempDir::new().unwrap();
    let mut child = ChildGuard(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "crash_recovery_child", "--nocapture"])
            .env("BRAINWIRE_RECOVERY_CHILD_ROOT", root.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let ready = root.path().join("ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before durable signal"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "child did not reach durable signal"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(std::fs::read(&ready).unwrap(), b"application-committed");
    child.0.kill().unwrap();
    let status = child.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
    let app = Arc::new(DurableApp {
        db: rocksdb::DB::open_default(root.path().join("application")).unwrap(),
        fail_after_commit: false,
    });
    let store = Storage::open(root.path().join("consensus"), 1024 * 1024, app.clone()).unwrap();
    let committed = entry(0, command("process-crash"));
    assert_eq!(store.committed().unwrap(), Some(committed.log_id));
    assert_eq!(store.last_applied().unwrap(), None);
    assert_eq!(store.lookup("process-crash").unwrap(), None);
    assert_eq!(
        app.db.get(b"\0effects").unwrap().unwrap(),
        1_u64.to_be_bytes()
    );
    assert_eq!(
        store
            .state_machine()
            .apply([committed.clone()])
            .await
            .unwrap(),
        vec![Reply::Applied { revision: 1 }]
    );
    assert_eq!(store.last_applied().unwrap(), Some(committed.log_id));
    assert_eq!(
        store.lookup("process-crash").unwrap(),
        Some(Reply::Applied { revision: 1 })
    );
    assert_eq!(
        app.db.get(b"\0effects").unwrap().unwrap(),
        1_u64.to_be_bytes()
    );
}

#[tokio::test]
async fn append_rejects_unrecoverable_origin_and_term_regression_atomically() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let mut logs = store.log_store();
    assert!(logs
        .blocking_append([entry(1, command("missing-origin"))])
        .await
        .is_err());
    assert_eq!(store.log_bytes().unwrap(), 0);
    assert!(logs.try_get_log_entries(..).await.unwrap().is_empty());

    let mut first = entry(0, command("first"));
    first.log_id.leader_id.term = 2;
    logs.blocking_append([first.clone()]).await.unwrap();
    logs.save_committed(Some(first.log_id)).await.unwrap();
    store.state_machine().apply([first.clone()]).await.unwrap();
    let bytes = store.log_bytes().unwrap();
    let mut second = entry(1, command("second"));
    second.log_id.leader_id.term = 3;
    let mut third = entry(2, command("third"));
    third.log_id.leader_id.term = 2;
    for invalid in [
        vec![entry(1, command("old-term"))],
        vec![second.clone(), third],
        vec![first.clone(), entry(1, command("old-term"))],
    ] {
        assert!(logs.blocking_append(invalid).await.is_err());
        assert_eq!(store.log_bytes().unwrap(), bytes);
        assert_eq!(store.last_applied().unwrap(), Some(first.log_id));
        assert_eq!(store.committed().unwrap(), Some(first.log_id));
        assert_eq!(store.lookup("second").unwrap(), None);
        assert_eq!(
            logs.try_get_log_entries(..).await.unwrap(),
            vec![first.clone()]
        );
    }
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    assert_eq!(store.log_bytes().unwrap(), bytes);
    assert_eq!(store.last_applied().unwrap(), Some(first.log_id));
    assert_eq!(store.committed().unwrap(), Some(first.log_id));
    let mut logs = store.log_store();
    assert_eq!(
        logs.try_get_log_entries(..).await.unwrap(),
        vec![first.clone()]
    );
    logs.blocking_append([first.clone(), second.clone()])
        .await
        .unwrap();
    // An earlier-term identical prefix may be replayed after a higher-term suffix.
    logs.blocking_append([first.clone()]).await.unwrap();
    drop(logs);
    drop(store);
    let store = Storage::open(dir.path(), 1024 * 1024, app).unwrap();
    assert_eq!(
        store.log_store().try_get_log_entries(..).await.unwrap(),
        vec![first, second]
    );
}

#[tokio::test]
async fn recovery_rejects_malformed_request_ledger_without_application_effects() {
    let valid = serde_json::to_vec(&serde_json::json!({
        "digest": vec![0_u8; 32], "reply": "Conflict"
    }))
    .unwrap();
    let mut cases = vec![
        (b"Drequest".to_vec(), b"{".to_vec()),
        (b"Drequest".to_vec(), b"null".to_vec()),
        (
            b"Drequest".to_vec(),
            b"{\"digest\":[],\"reply\":\"Conflict\"}".to_vec(),
        ),
        (b"Drequest".to_vec(), vec![b' '; 1025]),
    ];
    for key in [
        b"D".to_vec(),
        b"Dbad space".to_vec(),
        b"D\xff".to_vec(),
        [b"D".as_slice(), &[b'a'; 129]].concat(),
    ] {
        cases.push((key, valid.clone()));
    }
    for (key, encoded) in cases {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        let retained = entry(0, command("valid"));
        store
            .log_store()
            .blocking_append([retained.clone()])
            .await
            .unwrap();
        store.state_machine().apply([retained]).await.unwrap();
        drop(store);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        db.put(&key, &encoded).unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        assert!(Storage::open(dir.path(), 1024 * 1024, app.clone()).is_err());
        assert_eq!(app.calls.load(Ordering::SeqCst), 1);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        assert_eq!(db.get(&key).unwrap().unwrap(), encoded);
    }
}

#[test]
fn recovery_rejects_malformed_registry_marker_without_rewriting_it() {
    for encoded in [b"null".as_slice(), b"1".as_slice(), b"{".as_slice()] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        drop(Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap());
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        db.put(b"Mregistry_initialized", encoded).unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        assert!(Storage::open(dir.path(), 1024 * 1024, app.clone()).is_err());
        assert_eq!(app.calls.load(Ordering::SeqCst), 0);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        assert_eq!(db.get(b"Mregistry_initialized").unwrap().unwrap(), encoded);
    }
}

#[tokio::test]
async fn recovery_keeps_valid_legacy_request_ledger_and_missing_registry_marker() {
    let dir = TempDir::new().unwrap();
    let app = Arc::new(App::default());
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    let first = entry(0, command("legacy:request_1"));
    store
        .log_store()
        .blocking_append([first.clone()])
        .await
        .unwrap();
    store.state_machine().apply([first.clone()]).await.unwrap();
    drop(store);
    let db = rocksdb::DB::open_default(dir.path()).unwrap();
    assert!(db.get(b"Mregistry_initialized").unwrap().is_none());
    let original = db.get(b"Dlegacy:request_1").unwrap().unwrap();
    drop(db);
    let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
    assert!(!store.registry_initialized().unwrap());
    assert_eq!(
        store.lookup("legacy:request_1").unwrap(),
        Some(Reply::Applied { revision: 1 })
    );
    let duplicate = entry(1, command("legacy:request_1"));
    let mut conflict = command("legacy:request_1");
    if let Command::Write {
        expected_revision, ..
    } = &mut conflict
    {
        *expected_revision = 99;
    }
    let conflict = entry(2, conflict);
    store
        .log_store()
        .blocking_append([duplicate.clone(), conflict.clone()])
        .await
        .unwrap();
    assert_eq!(
        store
            .state_machine()
            .apply([duplicate, conflict])
            .await
            .unwrap(),
        vec![Reply::Applied { revision: 1 }, Reply::Conflict]
    );
    assert_eq!(app.calls.load(Ordering::SeqCst), 1);
    drop(store);
    let db = rocksdb::DB::open_default(dir.path()).unwrap();
    assert_eq!(db.get(b"Dlegacy:request_1").unwrap().unwrap(), original);
    assert!(db.get(b"Mregistry_initialized").unwrap().is_none());
}

#[test]
fn recovery_accepts_every_current_cached_reply_encoding_without_a_registry_marker() {
    for reply in [
        Reply::Initialized,
        Reply::Conflict,
        Reply::Applied { revision: u64::MAX },
    ] {
        let dir = TempDir::new().unwrap();
        let app = Arc::new(App::default());
        drop(Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap());
        // This is the existing persisted Cached layout, with maximal numeric widths.
        let encoded = serde_json::to_vec(&serde_json::json!({
            "digest": vec![255_u8; 32], "reply": reply
        }))
        .unwrap();
        let id = "a".repeat(128);
        let key = format!("D{id}");
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        db.put(&key, &encoded).unwrap();
        db.flush_wal(true).unwrap();
        drop(db);
        let store = Storage::open(dir.path(), 1024 * 1024, app.clone()).unwrap();
        assert!(!store.registry_initialized().unwrap());
        assert_eq!(store.lookup(&id).unwrap(), Some(reply));
        assert_eq!(app.calls.load(Ordering::SeqCst), 0);
        drop(store);
        let db = rocksdb::DB::open_default(dir.path()).unwrap();
        assert_eq!(db.get(&key).unwrap().unwrap(), encoded);
    }
}
