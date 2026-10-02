//! Warm embedded index-vs-scan measurement; temporary synthetic data only.
//! cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 10000
use std::{collections::BTreeMap, time::Instant};

use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, merge_node},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10000".into())
        .parse()?;
    if !(1..=1_000_000).contains(&count) {
        return Err("node count must be between 1 and 1000000".into());
    }
    let dir = tempfile::tempdir()?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    let db = Database::open(dir.path(), &resources)?;
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Customer","properties":["email","name"],"indexed_fields":["email"]}],"edges":[]}"#,
    )?;
    let catalog = db.apply_ontology(&mapping)?;
    let started = Instant::now();
    for start in (0..count).step_by(500) {
        let mut txn = db.begin();
        for index in start..(start + 500).min(count) {
            let key = Value::Str(format!("customer-{index}@example.test"));
            merge_node(
                &mut txn,
                &catalog,
                "Customer",
                &format!("customer-{index}"),
                &BTreeMap::from([("email".into(), key.clone()), ("name".into(), key)]),
            )?;
        }
        txn.commit()?;
    }
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    check_invariants(&db)?;
    drop(db);
    let db = Database::open(dir.path(), &resources)?;
    let mut measurements = serde_json::Map::new();
    for (name, predicate) in [
        ("patternIndex", "MATCH (c:Customer {email:$key})"),
        ("whereIndex", "MATCH (c:Customer) WHERE c.email=$key"),
        ("whereScan", "MATCH (c:Customer) WHERE c.name=$key"),
    ] {
        let query = format!("{predicate} RETURN c.entity_id");
        let mut samples = Vec::new();
        for sample in 0..30 {
            let index = (sample * 7919) % count;
            let params = BTreeMap::from([(
                "key".into(),
                Value::Str(format!("customer-{index}@example.test")),
            )]);
            let started = Instant::now();
            let result = execute(&db, &query, &params)?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                result.rows,
                vec![vec![Out::Value(Value::Str(format!("customer-{index}")))]]
            );
            if sample >= 5 {
                samples.push(elapsed);
            }
        }
        samples.sort_by(f64::total_cmp);
        measurements.insert(
            name.into(),
            serde_json::json!({
                "samples": samples.len(), "p50Ms": samples[samples.len() / 2],
                "p95Ms": samples[(samples.len() - 1) * 95 / 100],
            }),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "nodes": count, "importMs": import_ms, "cacheMiB": 64, "memtableMiB": 16,
            "mode": "warm embedded, excludes database open, 5 warmups per query",
            "measurements": measurements,
        }))?
    );
    Ok(())
}
