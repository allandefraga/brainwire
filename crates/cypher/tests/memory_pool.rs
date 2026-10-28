use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, ExecutionLimits, QueryMemoryPool, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn execution_deadline_stops_low_memory_fanout_and_releases_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(1 << 20));
    let limits = ExecutionLimits {
        memory_bytes: 1 << 20,
        timeout: Some(std::time::Duration::from_millis(1)),
    };
    let mut transaction =
        QueryTransaction::begin_with_pool(&db, limits, Arc::clone(&pool)).unwrap();
    let params = BTreeMap::from([("v".into(), Value::List((0..200).map(Value::Int).collect()))]);
    assert!(matches!(
        transaction.run(
            "UNWIND $v AS a UNWIND $v AS b UNWIND $v AS c UNWIND $v AS d RETURN count(*)",
            &params
        ),
        Err(ExecError::TimedOut)
    ));
    assert!(transaction.commit().is_err());
    assert_eq!(pool.usage().reserved_bytes, 0);
    let mut expired = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 1 << 20,
            timeout: Some(std::time::Duration::ZERO),
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert!(matches!(
        expired.run("RETURN 1", &BTreeMap::new()),
        Err(ExecError::TimedOut)
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn concurrent_transactions_share_capacity_and_release_on_all_terminal_paths() {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let first = Database::open(&dir.path().join("first"), &resources).unwrap();
    let second = Database::open(&dir.path().join("second"), &resources).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(24_000));
    let limits = ExecutionLimits {
        memory_bytes: 64_000,
        timeout: None,
    };
    let params = BTreeMap::from([("text".into(), Value::Str("x".repeat(3000)))]);
    let mut a = QueryTransaction::begin_with_pool(&first, limits, Arc::clone(&pool)).unwrap();
    a.run("RETURN $text", &params).unwrap();
    let held = pool.usage().reserved_bytes;
    assert!(held > 0);
    let mut b = QueryTransaction::begin_with_pool(&second, limits, Arc::clone(&pool)).unwrap();
    assert!(matches!(
        b.run("RETURN $text", &params),
        Err(ExecError::MemoryPoolExhausted { .. })
    ));
    assert!(b.commit().is_err());
    assert_eq!(pool.usage().reserved_bytes, held);
    assert_eq!(pool.usage().rejections, 1);
    assert!(pool.usage().peak_bytes <= 24_000);
    a.commit().unwrap();
    assert_eq!(pool.usage().reserved_bytes, 0);
    for explicit_rollback in [true, false] {
        let mut tx = QueryTransaction::begin_with_pool(&second, limits, Arc::clone(&pool)).unwrap();
        tx.run("RETURN $text", &params).unwrap();
        if explicit_rollback {
            tx.rollback();
        }
        drop(tx);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}

#[test]
fn pool_reservation_stays_bounded_across_runs_and_reports_live_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let pool = Arc::new(QueryMemoryPool::new(32_768));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 32_768,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    tx.run("RETURN 'hello'", &BTreeMap::new()).unwrap();
    let reserved = pool.usage().reserved_bytes;
    for _ in 0..100 {
        tx.run("RETURN 'hello'", &BTreeMap::new()).unwrap();
    }
    assert_eq!(pool.usage().reserved_bytes, reserved);
    let row = tx.run("CALL db.memory() YIELD poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections RETURN poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections",&BTreeMap::new()).unwrap();
    assert_eq!(row.rows.len(), 1);
    assert!(pool.usage().reserved_bytes > 0);
    tx.rollback();
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn result_lease_survives_commit_prepare_rollback_and_drop_without_double_charge() {
    for terminal in ["commit", "prepare", "rollback", "drop"] {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        if terminal == "prepare" {
            let mapping =
                serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                    .unwrap();
            db.apply_ontology(&mapping).unwrap();
        }
        let pool = Arc::new(QueryMemoryPool::new(64_000));
        let limits = ExecutionLimits {
            memory_bytes: 64_000,
            timeout: None,
        };
        let mut transaction =
            QueryTransaction::begin_with_pool(&db, limits, Arc::clone(&pool)).unwrap();
        if terminal == "prepare" {
            transaction = transaction.with_replicated_ids();
        }
        transaction
            .run(
                if terminal == "prepare" {
                    "CREATE (:Customer {entity_id:'lease'}) RETURN $text"
                } else {
                    "RETURN $text"
                },
                &BTreeMap::from([("text".into(), Value::Str("x".repeat(3000)))]),
            )
            .unwrap();
        let held = pool.usage().reserved_bytes;
        assert!(held > 0);
        let result = transaction.output_memory_lease();
        let in_flight = result.clone();
        assert_eq!(
            pool.usage().reserved_bytes,
            held,
            "cloning ownership must not double charge"
        );
        match terminal {
            "commit" => {
                transaction.commit().unwrap();
            }
            "prepare" => {
                assert!(transaction.prepare_replication().unwrap().is_some());
                assert_eq!(
                    transaction.state(),
                    brainwire_cypher::QueryTransactionState::Prepared
                );
            }
            "rollback" => {
                transaction.rollback();
            }
            "drop" => {}
            _ => unreachable!(),
        }
        drop(transaction);
        let retained = pool.usage().reserved_bytes;
        assert!(
            retained >= held,
            "{terminal} released live result accounting"
        );
        drop(result);
        assert_eq!(pool.usage().reserved_bytes, retained);
        drop(in_flight);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}
