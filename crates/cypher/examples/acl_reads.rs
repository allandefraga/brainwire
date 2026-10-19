//! ACL-governed reads on an isolated synthetic graph; setup is outside timings.
//! cargo run --release --locked --offline -p brainwire-cypher --example acl_reads -- 100000
use std::{collections::BTreeMap, sync::Arc, time::Instant};

use brainwire_core::{acl::NodeAcl, schema::GraphMapping, value::Value};
use brainwire_cypher::{ExecutionLimits, Out, QueryMemoryPool, QueryTransaction, ReadVisibility};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{merge_node, set_node_acl},
};

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
    let alice = NodeAcl {
        allow: vec!["alice".into()],
        ..NodeAcl::default()
    };
    let bob = NodeAcl {
        allow: vec!["bob".into()],
        ..NodeAcl::default()
    };
    let started = Instant::now();
    for first in (0..nodes).step_by(500) {
        let mut transaction = database.begin();
        for index in first..(first + 500).min(nodes) {
            let id = merge_node(
                &mut transaction,
                &catalog,
                "Customer",
                &format!("customer-{index}"),
                &BTreeMap::from([
                    ("segment".into(), Value::Int((index % 7) as i64)),
                    ("ordinal".into(), Value::Int(index as i64)),
                ]),
            )?;
            set_node_acl(
                &mut transaction,
                id,
                if index % 2 == 0 { &alice } else { &bob },
            )?;
        }
        transaction.commit()?;
    }
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    drop(database);
    let database = Database::open(directory.path(), &resources)?;
    let limits = ExecutionLimits {
        memory_bytes: 256 << 20,
        timeout: None,
    };
    let visibility = ReadVisibility {
        principals: vec!["alice".into()],
        ..ReadVisibility::default()
    };
    let parameters = BTreeMap::new();
    let mut measurements = serde_json::Map::new();
    for (name, query, expected) in [
        (
            "indexedEqualityCount",
            "MATCH (c:Customer) WHERE c.segment=0 RETURN count(*)",
            nodes.div_ceil(14),
        ),
        (
            "labelCount",
            "MATCH (c:Customer) RETURN count(*)",
            nodes.div_ceil(2),
        ),
        (
            "visiblePropertyCount",
            "MATCH (c:Customer) RETURN count(c.ordinal)",
            nodes.div_ceil(2),
        ),
    ] {
        let pool = Arc::new(QueryMemoryPool::new(limits.memory_bytes));
        let mut samples = Vec::new();
        for sample in 0..8 {
            let started = Instant::now();
            let mut transaction =
                QueryTransaction::begin_with_pool(&database, limits, Arc::clone(&pool))?
                    .with_read_visibility(visibility.clone())?;
            let output = transaction.run(query, &parameters)?;
            transaction.commit()?;
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                output.rows,
                vec![vec![Out::Value(Value::Int(expected as i64))]],
                "{name}: only Alice's nodes may be counted"
            );
            assert_eq!(pool.usage().reserved_bytes, 0);
            if sample > 0 {
                samples.push(elapsed_ms);
            }
        }
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        measurements.insert(
            name.into(),
            serde_json::json!({
                "query": query,
                "expectedVisibleCount": expected,
                "samplesMs": samples,
                "p50Ms": sorted[sorted.len() / 2],
                "maxMs": sorted[sorted.len() - 1],
                "peakReservationBytes": pool.usage().peak_bytes,
            }),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "nodes": nodes,
            "principal": "alice",
            "acl": "alternating alice/bob, every node has private ACL metadata",
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "cacheMiB": 64,
            "memtableMiB": 4,
            "queryBudgetBytes": limits.memory_bytes,
            "importMs": import_ms,
            "warmupsPerQuery": 1,
            "samplesPerQuery": 7,
            "scope": "warm embedded; excludes setup and database reopen; includes transaction begin, policy validation, query, commit; query accounting is not RSS",
            "measurements": measurements,
        }))?
    );
    Ok(())
}
