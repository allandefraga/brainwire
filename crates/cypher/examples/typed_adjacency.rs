//! Reproducible mixed-type hub workload on a fresh, synthetic database.
//! Build once, then run unchanged data with the before/after executable.
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute_with_limits, ExecutionLimits, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{check_invariants, merge_edge, merge_node},
};
use std::{collections::BTreeMap, path::Path, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [mode, path, rest @ ..] = args.as_slice() else {
        return Err("usage: typed_adjacency build PATH [neighbors] | run PATH".into());
    };
    if !matches!(mode.as_str(), "build" | "run") {
        return Err("mode must be build or run".into());
    }
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    if mode == "build" {
        let neighbors: usize = rest.first().map_or(Ok(10_000), |n| n.parse())?;
        if rest.len() > 1 || !(10..=100_000).contains(&neighbors) {
            return Err("neighbors must be between 10 and 100000".into());
        }
        if Path::new(path).try_exists()? {
            return Err("refusing to overwrite an existing path".into());
        }
        let db = Database::open(Path::new(path), &resources)?;
        let edges: Vec<_> = (0..10)
            .map(|i| format!("NOISE_{i}"))
            .chain(["WANTED".into(), "ABSENT".into()])
            .map(|name| serde_json::json!({"type":name,"from":{"label":"Item"},"to":{"label":"Item"}}))
            .collect();
        let mapping: GraphMapping = serde_json::from_value(serde_json::json!({
            "version":"1","nodes":[{"label":"Item"}],"edges":edges,
        }))?;
        let catalog = db.apply_ontology(&mapping)?;
        let empty = BTreeMap::new();
        let mut tx = db.begin();
        let hub = merge_node(&mut tx, &catalog, "Item", "hub", &empty)?;
        tx.commit()?;
        let started = Instant::now();
        for start in (0..neighbors).step_by(100) {
            let mut tx = db.begin();
            for i in start..(start + 100).min(neighbors) {
                let other = merge_node(&mut tx, &catalog, "Item", &format!("item-{i}"), &empty)?;
                for kind in 0..10 {
                    let kind = format!("NOISE_{kind}");
                    merge_edge(&mut tx, &catalog, hub, &kind, other, &empty)?;
                    merge_edge(&mut tx, &catalog, other, &kind, hub, &empty)?;
                }
                if i < 10 {
                    merge_edge(&mut tx, &catalog, hub, "WANTED", other, &empty)?;
                    merge_edge(&mut tx, &catalog, other, "WANTED", hub, &empty)?;
                }
            }
            tx.commit()?;
        }
        check_invariants(&db)?;
        println!(
            "{}",
            serde_json::json!({"nodes":neighbors+1,"relationships":neighbors*20+20,"buildMs":started.elapsed().as_secs_f64()*1000.0})
        );
        return Ok(());
    }
    if !rest.is_empty() || !Path::new(path).join("CURRENT").try_exists()? {
        return Err("run requires an existing fixture and no extra arguments".into());
    }
    let db = Database::open(Path::new(path), &resources)?;
    let stats = db.stats()?;
    let limits = ExecutionLimits {
        memory_bytes: 64 << 20,
        timeout: None,
    };
    let mut results = serde_json::Map::new();
    for (name, query, expected) in [
        (
            "outgoing",
            "MATCH (h:Item {entity_id:'hub'})-[:WANTED]->(n:Item) RETURN count(*)",
            10,
        ),
        (
            "incoming",
            "MATCH (h:Item {entity_id:'hub'})<-[:WANTED]-(n:Item) RETURN count(*)",
            10,
        ),
        (
            "absent",
            "MATCH (h:Item {entity_id:'hub'})-[:ABSENT]->(n:Item) RETURN count(*)",
            0,
        ),
        (
            "aggregateLimitOne",
            "MATCH (h:Item {entity_id:'hub'})-[:WANTED]->(n:Item) RETURN count(*) LIMIT 1",
            10,
        ),
        (
            "projectionLimit",
            "MATCH (h:Item {entity_id:'hub'})-[:WANTED]->(n:Item) RETURN 1 LIMIT 1",
            1,
        ),
        (
            "denseType",
            "MATCH (h:Item {entity_id:'hub'})-[:NOISE_0]->(n:Item) RETURN count(*)",
            (stats.nodes - 1) as i64,
        ),
        (
            "pointLookup",
            "MATCH (h:Item {entity_id:'hub'}) RETURN count(*)",
            1,
        ),
    ] {
        let mut samples = Vec::new();
        for i in 0..35 {
            let started = Instant::now();
            let result = execute_with_limits(&db, query, &BTreeMap::new(), limits)?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(result.rows, vec![vec![Out::Value(Value::Int(expected))]]);
            if i >= 5 {
                samples.push(elapsed);
            }
        }
        let raw = samples.clone();
        samples.sort_by(f64::total_cmp);
        results.insert(name.into(), serde_json::json!({"p50Ms":samples[(samples.len()-1)/2],"p95Ms":samples[(samples.len()-1)*95/100],"samplesMs":raw}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
            "nodes":stats.nodes,"relationships":stats.edges,"hubEdgesPerDirection":(stats.edges/2),
            "selectedEdges":10,"cacheMiB":64,"memtableMiB":16,
            "mode":"warm embedded, 5 warmups and 30 measured samples; excludes open and import; no network",
            "measurements":results,
        }))?
    );
    Ok(())
}
