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
