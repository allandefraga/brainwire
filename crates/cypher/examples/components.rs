//! Reproducible WCC/SCC correctness and end-to-end Cypher timings.
//! cargo run --release --locked --offline -p brainwire-cypher --example components -- 100000
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute_with_limits, ExecutionLimits, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{merge_edge, merge_node},
};
use std::{collections::BTreeMap, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let count: usize = args.next().unwrap_or_else(|| "100000".into()).parse()?;
    let fanout: usize = args.next().unwrap_or_else(|| "1".into()).parse()?;
    if args.next().is_some()
        || !(200..=100_000).contains(&count)
        || !count.is_multiple_of(200)
        || !(1..=10).contains(&fanout)
    {
        return Err(
            "expected node count divisible by 200 (200..100000), optionally fanout (1..10)".into(),
        );
    }
    let directory = tempfile::tempdir()?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    let db = Database::open(directory.path(), &resources)?;
    let schema: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Page"}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}}]}"#,
    )?;
    let catalog = db.apply_ontology(&schema)?;
    let empty = BTreeMap::new();
    let mut nodes = Vec::with_capacity(count);
    let started = Instant::now();
    for start in (0..count).step_by(1000) {
        let mut tx = db.begin();
        for index in start..(start + 1000).min(count) {
            nodes.push(merge_node(
                &mut tx,
                &catalog,
                "Page",
                &format!("page-{index}"),
                &empty,
            )?);
        }
        tx.commit()?;
    }
    // Independent groups of 100: alternate directed rings and directed chains.
    // Every group is one WCC; rings are one SCC, chains have 100 singleton SCCs.
    for group in 0..count / 100 {
        let mut tx = db.begin();
        let start = group * 100;
        for offset in 0..100 {
            for hop in 1..=fanout {
                if group.is_multiple_of(2) || offset + hop < 100 {
                    merge_edge(
                        &mut tx,
                        &catalog,
                        nodes[start + offset],
                        "LINK",
                        nodes[start + (offset + hop) % 100],
                        &empty,
                    )?;
                }
            }
        }
        tx.commit()?;
    }
    let import_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(db.stats()?.nodes, count as u64);
    let relationships = count / 200 * (200 * fanout - fanout * (fanout + 1) / 2);
    assert_eq!(db.stats()?.edges, relationships as u64);
    drop(db);
    let db = Database::open(directory.path(), &resources)?;
    let limits = ExecutionLimits {
        memory_bytes: 256 << 20,
        timeout: Some(std::time::Duration::from_secs(30)),
    };
    let mut reports = Vec::new();
    for (procedure, expected) in [("wcc", count / 100), ("scc", count / 200 * 101)] {
        let query = format!("CALL graph.{procedure}({{relationship:'LINK'}}) YIELD node,component,size WHERE node.entity_id = component.entity_id RETURN count(*) AS components");
        let mut samples = Vec::new();
        for iteration in 0..6 {
            let started = Instant::now();
            let result = execute_with_limits(&db, &query, &empty, limits)?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                result.rows,
                vec![vec![Out::Value(Value::Int(expected as i64))]]
            );
            if iteration > 0 {
                samples.push(elapsed);
            }
        }
        samples.sort_by(f64::total_cmp);
        let sample = execute_with_limits(&db, &format!("CALL graph.{procedure}({{relationship:'LINK'}}) YIELD node,component,size RETURN node.entity_id,component.entity_id,size LIMIT 1"), &empty, limits)?;
        assert_eq!(
            sample.rows,
            vec![vec![
                Out::Value(Value::Str("page-0".into())),
                Out::Value(Value::Str("page-0".into())),
                Out::Value(Value::Int(100))
            ]]
        );
        reports.push(serde_json::json!({"procedure":procedure,"expectedComponents":expected,"samplesMs":samples,"medianMs":samples[2],"maxMs":samples[4]}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
            "nodes":count,"relationships":relationships,"fanout":fanout,"importMs":import_ms,
            "queryMemoryLimitBytes":256 << 20,"results":reports,
            "note":"local synthetic rings/chains; timings include RocksDB projection, algorithm and eager Cypher bindings; five warm samples, not production percentiles or RSS"
        }))?
    );
    Ok(())
}
