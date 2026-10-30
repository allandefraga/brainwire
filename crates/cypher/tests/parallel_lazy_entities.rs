use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, ExecutionLimits, LazyRun, Out, QueryAccess, QueryMemoryPool, QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

fn setup() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item","properties":["number"]}],"edges":[]}"#,
    )
    .unwrap();
    database.apply_ontology(&mapping).unwrap();
    for start in (0..60_000).step_by(5_000) {
        execute(
            &database,
            &format!(
                "UNWIND range({start}, {}) AS i CREATE (:Item {{entity_id:'item-' + i, number:i}})",
                start + 4_999
            ),
            &BTreeMap::new(),
        )
        .unwrap();
    }
    (directory, database)
}

#[test]
fn large_entity_scan_can_stop_after_one_row_with_bounded_memory() {
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    let pool = Arc::new(QueryMemoryPool::new(4 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 4 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let mut fields = 0;
    let mut rows = 0;
    let result = transaction.run_lazy_with_access(
        "MATCH (n:Item) RETURN n",
        &BTreeMap::new(),
        QueryAccess {
            write: false,
            instance_admin: false,
        },
        &mut |columns| {
            assert_eq!(columns, &["n"]);
            fields += 1;
            true
        },
        &mut |row, _| {
            assert!(matches!(&row[0], Out::Node(_)));
            rows += 1;
            false
        },
    );
    assert!(
        matches!(result, Ok(LazyRun::Streamed)),
        "entity scan must stream rather than retain all handles"
    );
    assert_eq!(fields, 1);
    assert_eq!(rows, 1);
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert!(pool.usage().peak_bytes <= 4 << 20);
}

#[test]
fn parallel_entities_preserve_order_and_the_original_snapshot() {
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    let pool = Arc::new(QueryMemoryPool::new(8 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 8 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let mut numbers = Vec::new();
    let result = transaction
        .run_lazy_with_access(
            "MATCH (n:Item) RETURN n",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
            &mut |_| {
                execute(
                    &database,
                    "MATCH (n:Item {entity_id:'item-0'}) SET n.number=-1",
                    &BTreeMap::new(),
                )
                .unwrap();
                true
            },
            &mut |row, _| {
                let Out::Node(node) = &row[0] else {
                    panic!("node expected");
                };
                let Some(Value::Int(number)) = node.props.get("number") else {
                    panic!("number expected");
                };
                numbers.push(*number);
                true
            },
        )
        .unwrap();
    assert!(matches!(result, LazyRun::Streamed));
    assert_eq!(numbers, (0..60_000).collect::<Vec<_>>());
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn parallel_entity_acl_filters_ranges_and_hides_acl_properties() {
    use brainwire_core::acl::ReadVisibility;
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    for number in [20_000, 40_000, 59_999] {
        execute(
            &database,
            "CALL db.setNodeAcl({label:'Item',entityId:$id,allow:['reader']})",
            &BTreeMap::from([("id".into(), Value::Str(format!("item-{number}")))]),
        )
        .unwrap();
    }
    let pool = Arc::new(QueryMemoryPool::new(8 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 8 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap()
    .with_read_visibility(ReadVisibility {
        principals: vec!["reader".into()],
        ..Default::default()
    })
    .unwrap();
    let mut numbers = Vec::new();
    let result = transaction
        .run_lazy_with_access(
            "MATCH (n:Item) RETURN n",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
            &mut |_| true,
            &mut |row, _| {
                let Out::Node(node) = &row[0] else {
                    panic!("node expected");
                };
                assert!(node
                    .props
                    .keys()
                    .all(|key| !key.to_ascii_lowercase().starts_with("acl_")));
                numbers.push(node.props["number"].clone());
                true
            },
        )
        .unwrap();
    assert!(matches!(result, LazyRun::Streamed));
    assert_eq!(numbers, [20_000, 40_000, 59_999].map(Value::Int));
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn cancellation_stops_an_empty_parallel_scan_and_late_worker_errors_release_memory() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    let pool = Arc::new(QueryMemoryPool::new(4 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 4 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let on_fields = Arc::clone(&cancel);
    let result = transaction
        .run_lazy_leased_with_access(
            "MATCH (n:Item) WHERE n.number < 0 RETURN n",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
            &mut |_| {
                on_fields.store(true, Ordering::Release);
                true
            },
            cancel,
            &mut |_, _, _| panic!("cancelled empty scan emitted a row"),
        )
        .unwrap();
    assert!(matches!(result, LazyRun::Streamed));
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 4 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    transaction
        .run(
            "MATCH (n:Item {entity_id:'item-0'}) SET n.number=-1",
            &BTreeMap::new(),
        )
        .unwrap();
    let result = transaction.run_lazy_with_access(
        "MATCH (n:Item) RETURN n, 1 / (59999 - n.number)",
        &BTreeMap::new(),
        QueryAccess {
            write: false,
            instance_admin: false,
        },
        &mut |_| true,
        &mut |_, _| true,
    );
    assert!(result.is_err());
    assert_eq!(pool.usage().reserved_bytes, 0);
    let original = execute(
        &database,
        "MATCH (n:Item {entity_id:'item-0'}) RETURN n.number",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(original.rows, vec![vec![Out::Value(Value::Int(0))]]);
}

#[test]
fn parallel_output_credit_outlives_the_transaction_until_the_consumer_drops_it() {
    use std::sync::atomic::AtomicBool;
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    let pool = Arc::new(QueryMemoryPool::new(4 << 20));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 4 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let mut owned = None;
    let result = transaction
        .run_lazy_leased_with_access(
            "MATCH (n:Item) RETURN n",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
            &mut |_| true,
            Arc::new(AtomicBool::new(false)),
            &mut |row, _, lease| {
                owned = Some((row, lease));
                false
            },
        )
        .unwrap();
    assert!(matches!(result, LazyRun::Streamed));
    transaction.rollback();
    drop(transaction);
    assert!(
        pool.usage().reserved_bytes > 0,
        "live output needs its credit"
    );
    assert!(
        pool.usage().reserved_bytes < 1 << 20,
        "completed workers must release scratch and queued output"
    );
    drop(owned);
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn cancellation_reaches_parallel_workers_that_have_started_without_output() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};
    if std::thread::available_parallelism().map_or(1, |n| n.get()) < 2 {
        return;
    }
    let (_directory, database) = setup();
    let pool = Arc::new(QueryMemoryPool::new(4 << 20));
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let mut transaction = QueryTransaction::begin_with_pool(
        &database,
        ExecutionLimits {
            memory_bytes: 4 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let (header, started) = mpsc::channel();
    let (ready, observing) = mpsc::channel();
    std::thread::scope(|scope| {
        let observed_pool = Arc::clone(&pool);
        let request_cancel = Arc::clone(&cancel);
        let finished = Arc::clone(&done);
        let canceller = scope.spawn(move || {
            let baseline = started.recv_timeout(Duration::from_secs(5)).unwrap();
            ready.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut observed = false;
            while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
                if observed_pool.usage().reserved_bytes > baseline {
                    observed = true;
                    break;
                }
                std::thread::yield_now();
            }
            request_cancel.store(true, Ordering::Release);
            observed
        });
        let result = transaction.run_lazy_leased_with_access(
            "MATCH (n:Item) WHERE n.number < 0 RETURN n",
            &BTreeMap::new(),
            QueryAccess {
                write: false,
                instance_admin: false,
            },
            &mut |_| {
                header.send(pool.usage().reserved_bytes).unwrap();
                observing.recv_timeout(Duration::from_secs(5)).unwrap();
                true
            },
            Arc::clone(&cancel),
            &mut |_, _, _| panic!("empty scan emitted a row"),
        );
        done.store(true, Ordering::Release);
        assert!(matches!(result, Ok(LazyRun::Streamed)));
        assert!(
            canceller.join().unwrap(),
            "workers never acquired scan memory before cancellation"
        );
    });
    drop(transaction);
    assert_eq!(pool.usage().reserved_bytes, 0);
}
