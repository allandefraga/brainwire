use brainwire_core::value::Value;
use brainwire_cypher::{
    exec::IdentifiedCommitError, ExecError, ExecutionLimits, QueryMemoryPool, QueryTransaction,
    QueryTransactionState,
};
use brainwire_store::db::{request_ledger::LedgerLimits, Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

fn setup(identified: bool) -> (tempfile::TempDir, Resources, Database) {
    let directory = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let path = directory.path().join("graph");
    let database = if identified {
        Database::create_with_request_ledger(
            &path,
            &resources,
            LedgerLimits {
                max_entries: 100,
                max_bytes: 100_000,
            },
        )
        .unwrap()
    } else {
        Database::open(&path, &resources).unwrap()
    };
    let mapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    database.apply_ontology(&mapping).unwrap();
    (directory, resources, database)
}

#[test]
fn standalone_commit_guard_refuses_before_writing_and_releases_its_reservation() {
    for (identified, shared_limit) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_directory, _resources, database) = setup(identified);
        let pool = Arc::new(QueryMemoryPool::new(if shared_limit {
            128 * 1024
        } else {
            1 << 20
        }));
        let limits = ExecutionLimits {
            memory_bytes: if shared_limit { 1 << 20 } else { 128 * 1024 },
            timeout: None,
        };
        let mut transaction =
            QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool)).unwrap();
        if identified {
            transaction = transaction.with_identified_write().unwrap();
        }
        let ids = (0..100)
            .map(|index| Value::Str(format!("guard-{index}")))
            .collect();
        transaction
            .run(
                "UNWIND $ids AS id CREATE (:Customer {entity_id:id})",
                &BTreeMap::from([("ids".into(), Value::List(ids))]),
            )
            .unwrap();
        // Standalone RUN may durably reserve IDs; only completion is under test.
        let before_commit = database.commit_sequence();
        assert!(pool.usage().reserved_bytes > 0);
        let failure = if identified {
            match transaction.commit_identified("writer", "refused") {
                Err(IdentifiedCommitError::Execution(error)) => Some(error),
                other => panic!("expected pre-write execution refusal, got {other:?}"),
            }
        } else {
            transaction.commit().err()
        };
        assert!(
            matches!(
                (shared_limit, failure),
                (false, Some(ExecError::MemoryLimit { .. }))
                    | (true, Some(ExecError::MemoryPoolExhausted { .. }))
            ),
            "commit should reserve its materialization before writing"
        );
        assert_eq!(transaction.state(), QueryTransactionState::Failed);
        assert_eq!(pool.usage().reserved_bytes, 0);
        assert_eq!(database.commit_sequence(), before_commit);
        assert_eq!(database.stats().unwrap().nodes, 0);
        if identified {
            assert!(database
                .request_receipt("writer", "refused")
                .unwrap()
                .is_none());
        }

        let mut next =
            QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool)).unwrap();
        if identified {
            next = next.with_identified_write().unwrap();
        }
        next.run("CREATE (:Customer {entity_id:'healthy'})", &BTreeMap::new())
            .unwrap();
        if identified {
            let receipt = next.commit_identified("writer", "healthy").unwrap();
            assert_eq!(
                database.request_receipt("writer", "healthy").unwrap(),
                Some(receipt)
            );
        } else {
            next.commit().unwrap();
        }
        assert_eq!(next.state(), QueryTransactionState::Committed);
        assert_eq!(database.stats().unwrap().nodes, 1);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}

#[test]
fn read_only_completion_does_not_require_a_write_materialization_reservation() {
    let (_directory, _resources, database) = setup(false);
    let pool = Arc::new(QueryMemoryPool::new(64 * 1024));
    let headroom = Arc::new(std::sync::atomic::AtomicUsize::new(1 << 20));
    let observed = Arc::clone(&headroom);
    pool.set_process_headroom(move || observed.load(std::sync::atomic::Ordering::Relaxed));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 64 * 1024,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    transaction.run("RETURN 1", &BTreeMap::new()).unwrap();
    headroom.store(0, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(transaction.commit().unwrap(), None);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn refused_commit_preserves_accounting_owned_by_live_results() {
    let (_directory, _resources, database) = setup(false);
    let pool = Arc::new(QueryMemoryPool::new(1 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 128 * 1024,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    transaction
        .run(
            "UNWIND $ids AS id CREATE (:Customer {entity_id:id})",
            &BTreeMap::from([(
                "ids".into(),
                Value::List(
                    (0..100)
                        .map(|index| Value::Str(format!("held-{index}")))
                        .collect(),
                ),
            )]),
        )
        .unwrap();
    let output = transaction.run("RETURN 'held'", &BTreeMap::new()).unwrap();
    let result_owner = transaction.output_memory_lease();
    let reserved = pool.usage().reserved_bytes;
    assert!(reserved > 0);
    assert!(matches!(
        transaction.commit(),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(output.rows.len(), 1);
    assert_eq!(database.stats().unwrap().nodes, 0);
    assert_eq!(pool.usage().reserved_bytes, reserved);
    drop(transaction);
    drop(output);
    assert_eq!(pool.usage().reserved_bytes, reserved);
    drop(result_owner);
    assert_eq!(pool.usage().reserved_bytes, 0);
}
