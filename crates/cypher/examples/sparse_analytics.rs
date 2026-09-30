//! Reproducible persisted-graph workload; no network or existing database used.
//! cargo run --locked --offline -p brainwire-cypher --example sparse_analytics -- 100000
use brainwire_core::schema::GraphMapping;
use brainwire_cypher::{execute, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{merge_edge, merge_node, sparse_projection, ProjectionOptions},
};
use std::{collections::BTreeMap, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let edges: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "100000".into())
        .parse()?;
    if !(100..=1_000_000).contains(&edges) || !edges.is_multiple_of(10) {
        return Err("edge count must be a multiple of 10 between 100 and 1000000".into());
    }
    let node_count = edges / 10;
    let dir = tempfile::tempdir()?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    let db = Database::open(dir.path(), &resources)?;
    let mapping: GraphMapping = serde_json::from_str(
        r#"{"version":"1","nodes":[{"label":"Page"}],"edges":[{"type":"LINK","from":{"label":"Page"},"to":{"label":"Page"}}]}"#,
    )?;
    let catalog = db.apply_ontology(&mapping)?;
    let empty = BTreeMap::new();
    let mut nodes = Vec::with_capacity(node_count);
    let started = Instant::now();
    for start in (0..node_count).step_by(1000) {
        let mut tx = db.begin();
        for i in start..(start + 1000).min(node_count) {
            nodes.push(merge_node(
                &mut tx,
                &catalog,
                "Page",
                &format!("synthetic-{i}"),
                &empty,
            )?);
        }
        tx.commit()?;
    }
    let node_import_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    for start in (0..node_count).step_by(100) {
        let mut tx = db.begin();
        for i in start..(start + 100).min(node_count) {
            for hop in 1..=10 {
                // Nine ring offsets plus one shared hub: bounded out-degree,
                // skewed in-degree and a nonuniform PageRank result.
                let target = if hop == 10 && i + 9 < node_count {
                    0
                } else {
                    (i + hop) % node_count
                };
                merge_edge(&mut tx, &catalog, nodes[i], "LINK", nodes[target], &empty)?;
            }
        }
        tx.commit()?;
        if start.is_multiple_of(10_000) {
            eprintln!("imported sources {start}/{node_count}");
        }
    }
    let edge_import_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected_edges = edges;
    assert_eq!(db.stats()?.edges, expected_edges as u64);
    drop(db);
    let started = Instant::now();
    let db = Database::open(dir.path(), &resources)?;
    let reopen_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let projection = sparse_projection(&db.begin(), &ProjectionOptions::default())?;
    let projection_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(projection.nodes.len(), node_count);
    assert_eq!(projection.relationship_count, expected_edges);
    let csr_bytes = projection.matrix.allocated_bytes();
    let started = Instant::now();
    let ranks = projection.matrix.pagerank_until(100, 0.85, 1e-8)?;
    let pagerank_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mass: f64 = ranks.scores.iter().sum();
    assert!((mass - 1.0).abs() < 1e-8);
    let iterations = ranks.iterations;
    let converged = ranks.converged;
    drop(projection);
    drop(ranks);
    let started = Instant::now();
    let query = "CALL graph.pagerank({relationship:'LINK', maxIterations:100, tolerance:0.00000001}) YIELD node, score RETURN node.entity_id AS id, score LIMIT 5";
    let result = execute(&db, query, &empty)?;
    let cypher_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(result.rows.len(), 5);
    let Out::Value(brainwire_core::value::Value::Str(top)) = &result.rows[0][0] else {
        panic!("expected id")
    };
    assert_eq!(top, "synthetic-0");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "nodes": node_count, "attemptedRelationships": edges, "relationships": expected_edges,
            "nodeImportMs": node_import_ms, "edgeImportMs": edge_import_ms, "reopenMs": reopen_ms,
            "projectionMs": projection_ms, "pagerankMs": pagerank_ms, "cypherTop5Ms": cypher_ms,
            "csrBytes": csr_bytes, "rankMass": mass, "iterations": iterations, "converged": converged,
            "topEntity": top, "cacheBytes": 64 << 20, "memtableBytes": 16 << 20,
            "note": "local synthetic workload; CSR bytes exclude construction buffers and process RSS"
        }))?
    );
    Ok(())
}
