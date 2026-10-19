//! Warm Cypher read diagnostic for a 100k-edge hub.
//! cargo run --release --locked --offline -p brainwire-cypher --example streaming_edges -- 100000
use std::{collections::BTreeMap, sync::Arc, time::Instant};

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{ExecutionLimits, Out, QueryMemoryPool, QueryTransaction};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, merge_edge, merge_node},
};

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let edges: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "100000".into())
        .parse()?;
    if !(100..=200_000).contains(&edges) {
        return Err("edge count must be between 100 and 200000".into());
    }
    let directory = tempfile::tempdir()?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: 128 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 100_000,
    });
    let database = Database::open(directory.path(), &resources)?;
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Item","properties":["key"],"indexed_fields":["key"]}],"edges":[{"type":"LINK","from":{"label":"Item"},"to":{"label":"Item"}}]}"#,
    )?;
    let catalog = database.apply_ontology(&mapping)?;
    let hub = {
        let mut txn = database.begin();
        let hub = merge_node(
            &mut txn,
            &catalog,
            "Item",
            "hub",
            &BTreeMap::from([("key".into(), Value::Str("hub".into()))]),
        )?;
        txn.commit()?;
        hub
    };
    let started = Instant::now();
    for first in (0..edges).step_by(500) {
        let mut txn = database.begin();
        for index in first..(first + 500).min(edges) {
            let id = merge_node(
                &mut txn,
                &catalog,
                "Item",
                &format!("peer-{index}"),
                &BTreeMap::from([("key".into(), Value::Str(format!("peer-{index}")))]),
            )?;
            merge_edge(&mut txn, &catalog, hub, "LINK", id, &BTreeMap::new())?;
        }
        txn.commit()?;
    }
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(database);
    let database = Database::open(directory.path(), &resources)?;
    check_invariants(&database)?;

    let limits = ExecutionLimits {
        memory_bytes: 128 << 20,
        timeout: None,
    };
    let parameters = BTreeMap::new();
    let mut results = serde_json::Map::new();
    for (name, query, expected_rows) in [
        (
            "hubLimitOne",
            "MATCH (h:Item)-[:LINK]->(n:Item) WHERE h.key='hub' RETURN n.key LIMIT 1",
            1,
        ),
        (
            "hubCount",
            "MATCH (h:Item)-[:LINK]->(n:Item) WHERE h.key='hub' RETURN count(n.key)",
            1,
        ),
    ] {
        let pool = Arc::new(QueryMemoryPool::new(limits.memory_bytes));
        let mut samples = Vec::new();
        let mut peak = 0;
        for sample in 0..8 {
            let started = Instant::now();
            let mut txn = QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool))?;
            let output = txn.run(query, &parameters)?;
            txn.commit()?;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(output.rows.len(), expected_rows, "{name}");
            match name {
                "hubCount" => {
                    assert_eq!(output.rows[0], vec![Out::Value(Value::Int(edges as i64))]);
                }
                "hubLimitOne" => match &output.rows[0][0] {
                    Out::Value(Value::Str(key)) => assert!(key
                        .strip_prefix("peer-")
                        .and_then(|suffix| suffix.parse::<usize>().ok())
                        .is_some_and(|index| index < edges)),
                    _ => return Err("LIMIT query returned an unexpected value".into()),
                },
                _ => unreachable!("only declared benchmark queries are measured"),
            }
            peak = pool.usage().peak_bytes.max(peak);
            assert_eq!(pool.usage().reserved_bytes, 0);
            if sample > 0 {
                samples.push(elapsed_ms);
            }
        }
        let mut ordered = samples.clone();
        ordered.sort_by(f64::total_cmp);
        results.insert(
            name.into(),
            serde_json::json!({
                "query": query,
                "samplesMs": samples,
                "p50Ms": median(&samples),
                "maxMs": ordered[ordered.len() - 1],
                "peakReservationBytes": peak,
            }),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "edges": edges,
            "nodes": edges + 1,
            "hubOutDegree": edges,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "cacheMiB": 128,
            "memtableMiB": 16,
            "queryBudgetBytes": limits.memory_bytes,
            "importMs": import_ms,
            "warmupsPerQuery": 1,
            "samplesPerQuery": 7,
            "scope": "warm embedded Cypher reads; includes transaction begin, index lookup, relationship traversal and commit; query reservation is not RSS",
            "measurements": results,
        }))?
    );
    Ok(())
}
