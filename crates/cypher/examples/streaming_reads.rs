//! Bounded-read and count measurements on an isolated synthetic graph.
//! cargo run --locked --offline -p brainwire-cypher --example streaming_reads -- 100000
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{ExecError, ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::merge_node,
};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nodes: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "100000".into())
        .parse()?;
    if !(100..=1_000_000).contains(&nodes) {
        return Err("nodes must be between 100 and 1000000".into());
    }
    let directory = tempfile::tempdir()?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 4 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    let database = Database::open(directory.path(), &resources)?;
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["segment","ordinal"],"indexed_fields":["segment"]}]}"#,
    )?;
    let catalog = database.apply_ontology(&mapping)?;
    let started = Instant::now();
    for first in (0..nodes).step_by(500) {
        let mut transaction = database.begin();
        for index in first..(first + 500).min(nodes) {
            merge_node(
                &mut transaction,
                &catalog,
                "Customer",
                &format!("customer-{index}"),
                &BTreeMap::from([
                    ("segment".into(), Value::Int((index % 7) as i64)),
                    ("ordinal".into(), Value::Int(index as i64)),
                ]),
            )?;
        }
        transaction.commit()?;
    }
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(database);
    let database = Database::open(directory.path(), &resources)?;
    let parameters = BTreeMap::from([
        ("last".into(), Value::Int(nodes as i64 - 1)),
        ("v".into(), Value::List((0..100).map(Value::Int).collect())),
    ]);
    let limits = ExecutionLimits {
        memory_bytes: 64 << 10,
        timeout: None,
    };
    let mut measurements = serde_json::Map::new();
    for (name, query, expected) in [
        (
            "labelLimit",
            "MATCH (c:Customer) RETURN c.entity_id LIMIT 1",
            vec![vec![Out::Value(Value::Str("customer-0".into()))]],
        ),
        (
            "indexLimit",
            "MATCH (c:Customer) WHERE c.segment=1 RETURN c.entity_id LIMIT 1",
            vec![vec![Out::Value(Value::Str("customer-1".into()))]],
        ),
        (
            "labelCount",
            "MATCH (c:Customer) RETURN count(*)",
            vec![vec![Out::Value(Value::Int(nodes as i64))]],
        ),
        (
            "indexCount",
            "MATCH (c:Customer) WHERE c.segment=0 RETURN count(*)",
            vec![vec![Out::Value(Value::Int(nodes.div_ceil(7) as i64))]],
        ),
        (
            "lateScanLimit",
            "MATCH (c:Customer) WHERE c.ordinal=$last RETURN c.entity_id LIMIT 1",
            vec![vec![Out::Value(Value::Str(format!(
                "customer-{}",
                nodes - 1
            )))]],
        ),
        (
            "cartesianLimit",
            "MATCH (c:Customer), (d:Customer) RETURN c.entity_id,d.entity_id LIMIT 1",
            vec![vec![
                Out::Value(Value::Str("customer-0".into())),
                Out::Value(Value::Str("customer-0".into())),
            ]],
        ),
        (
            "unwindLimit",
            "UNWIND $v AS a UNWIND $v AS b UNWIND $v AS c RETURN a,b,c LIMIT 1",
            vec![vec![Out::Value(Value::Int(0)); 3]],
        ),
    ] {
        let pool = Arc::new(QueryMemoryPool::new(limits.memory_bytes));
        let mut samples = vec![];
        for sample in 0..6 {
            let started = Instant::now();
            let mut transaction =
                QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool))?;
            let output = transaction.run(query, &parameters)?;
            transaction.commit()?;
            let millis = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(output.rows, expected, "{name}");
            assert_eq!(pool.usage().reserved_bytes, 0);
            if sample > 0 {
                samples.push(millis);
            }
        }
        samples.sort_by(f64::total_cmp);
        measurements.insert(name.into(),serde_json::json!({"samples":samples.len(),"p50Ms":samples[samples.len()/2],"maxMs":samples[samples.len()-1],"peakReservationBytes":pool.usage().peak_bytes}));
    }
    let pool = Arc::new(QueryMemoryPool::new(limits.memory_bytes));
    let mut transaction = QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool))?;
    assert!(matches!(
        transaction.run("MATCH (c:Customer) RETURN c", &parameters),
        Err(ExecError::MemoryLimit { .. })
    ));
    assert_eq!(pool.usage().reserved_bytes, 0);
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"nodes":nodes,"queryBudgetBytes":limits.memory_bytes,"cacheMiB":64,"memtableMiB":4,"importMs":import_ms,"measurements":measurements,"unboundedResult":"memory_limit_and_reservation_release_verified","scope":"warm embedded debug, one warmup and five samples; query accounting is not RSS"})
        )?
    );
    Ok(())
}
