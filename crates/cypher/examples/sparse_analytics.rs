//! Reproducible graph workload; no network or existing database used.
//! cargo run --locked --offline -p brainwire-cypher --example sparse_analytics -- 100000
//! Append `--database data/staging-100k` to retain a fresh generated database.
use brainwire_core::schema::GraphMapping;
use brainwire_core::value::Value;
use brainwire_cypher::{execute_with_limits, ExecutionLimits, Out};
use brainwire_store::{
    db::{Database, Resources, StoreConfig},
    graph::{merge_edge, merge_node, sparse_projection, ProjectionOptions},
};
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

fn latency_summary(samples: &mut [f64]) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    let percentile = |p: usize| samples[(samples.len() - 1) * p / 100];
    serde_json::json!({
        "samples": samples.len(),
        "meanMs": samples.iter().sum::<f64>() / samples.len() as f64,
        "p50Ms": percentile(50),
        "p95Ms": percentile(95),
        "p99Ms": percentile(99),
    })
}

fn measure_local_query(
    db: &Database,
    query: &str,
    node_count: usize,
    expected_rows: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut samples = Vec::with_capacity(1000);
    for i in 0..1100 {
        let id = (i * 7919) % node_count;
        let params = BTreeMap::from([("id".into(), Value::Str(format!("synthetic-{id}")))]);
        let started = Instant::now();
        let result = execute_with_limits(
            db,
            query,
            &params,
            ExecutionLimits {
                memory_bytes: 256 << 20,
                timeout: None,
            },
        )?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(result.rows.len(), expected_rows);
        if i >= 100 {
            samples.push(elapsed_ms);
        }
    }
    Ok(latency_summary(&mut samples))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let edges: usize = args.next().unwrap_or_else(|| "100000".into()).parse()?;
    if !(100..=1_000_000).contains(&edges) || !edges.is_multiple_of(10) {
        return Err("edge count must be a multiple of 10 between 100 and 1000000".into());
    }
    let persisted = match args.next().as_deref() {
        Some("--database") => Some(PathBuf::from(args.next().ok_or("missing database path")?)),
        None => None,
        _ => return Err("expected --database PATH after edge count".into()),
    };
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }
    if let Some(path) = &persisted {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(format!("refusing to overwrite {}", path.display()).into()),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            Err(_) => {}
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
    }
    let node_count = edges / 10;
    let tempdir = if persisted.is_none() {
        Some(tempfile::tempdir()?)
    } else {
        None
    };
    let path = persisted.unwrap_or_else(|| tempdir.as_ref().unwrap().path().to_path_buf());
    let resources = Resources::new(StoreConfig {
        cache_bytes: 64 << 20,
        memtable_bytes: 16 << 20,
        id_block: 1024,
        conflict_history_keys: 50_000,
    });
    let db = Database::open(&path, &resources)?;
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
    let db = Database::open(&path, &resources)?;
    let reopen_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let projection = sparse_projection(
        &db.begin(),
        &ProjectionOptions {
            max_bytes: 256 << 20,
            ..ProjectionOptions::default()
        },
    )?;
    let projection_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(projection.nodes.len(), node_count);
    assert_eq!(projection.relationship_count, expected_edges);
    let csr_bytes = projection.matrix.allocated_bytes();
    let started = Instant::now();
    let ranks = projection.matrix.pagerank_until(100, 0.85, 1e-8)?;
    let pagerank_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    let depths = projection.matrix.breadth_first(0, 10)?;
    let bfs_ms = started.elapsed().as_secs_f64() * 1000.0;
    let reachable_nodes = depths.iter().flatten().count();
    assert_eq!(depths[0], Some(0));
    assert_eq!(depths[1], Some(1));
    let started = Instant::now();
    let shortest_path = projection
        .matrix
        .shortest_path(node_count - 1, 0, 10)?
        .unwrap();
    let shortest_path_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(shortest_path, vec![node_count - 1, 0]);
    let mass: f64 = ranks.scores.iter().sum();
    assert!((mass - 1.0).abs() < 1e-8);
    let iterations = ranks.iterations;
    let converged = ranks.converged;
    drop(projection);
    drop(ranks);
    let started = Instant::now();
    let query = "CALL graph.pagerank({relationship:'LINK', maxIterations:100, tolerance:0.00000001}) YIELD node, score RETURN node.entity_id AS id, score LIMIT 5";
    let result = execute_with_limits(
        &db,
        query,
        &empty,
        ExecutionLimits {
            memory_bytes: 256 << 20,
            timeout: None,
        },
    )?;
    let cypher_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(result.rows.len(), 5);
    let Out::Value(brainwire_core::value::Value::Str(top)) = &result.rows[0][0] else {
        panic!("expected id")
    };
    assert_eq!(top, "synthetic-0");
    let started = Instant::now();
    let path_result = execute_with_limits(&db,
        "CALL graph.shortestPath({source:$source,sourceLabel:'Page',target:'synthetic-0',targetLabel:'Page',relationship:'LINK'}) YIELD node,depth RETURN node.entity_id,depth",
        &BTreeMap::from([("source".into(),Value::Str(format!("synthetic-{}",node_count-1)))]),
        ExecutionLimits { memory_bytes: 256 << 20 , timeout: None })?;
    let cypher_path_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(path_result.rows.len(), 2);
    assert_eq!(
        path_result.rows[1],
        vec![
            Out::Value(Value::Str("synthetic-0".into())),
            Out::Value(Value::Int(1))
        ]
    );
    let point_lookup = measure_local_query(
        &db,
        "MATCH (p:Page {entity_id:$id}) RETURN p.entity_id",
        node_count,
        1,
    )?;
    let one_hop = measure_local_query(
        &db,
        "MATCH (p:Page {entity_id:$id})-[:LINK]->(q:Page) RETURN q.entity_id",
        node_count,
        10,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "nodes": node_count, "attemptedRelationships": edges, "relationships": expected_edges,
            "nodeImportMs": node_import_ms, "edgeImportMs": edge_import_ms, "reopenMs": reopen_ms,
            "projectionMs": projection_ms, "pagerankMs": pagerank_ms, "cypherTop5Ms": cypher_ms,
            "bfsDepth10Ms": bfs_ms, "shortestPathMs": shortest_path_ms, "cypherShortestPathMs": cypher_path_ms, "allocationGuardBytes": 256 << 20, "reachableNodes": reachable_nodes,
            "csrBytes": csr_bytes, "rankMass": mass, "iterations": iterations, "converged": converged,
            "pointLookup": point_lookup, "oneHopTenNeighbors": one_hop,
            "databasePath": if tempdir.is_some() { None } else { Some(path.display().to_string()) },
            "topEntity": top, "cacheBytes": 64 << 20, "memtableBytes": 16 << 20,
            "note": "local synthetic workload; CSR bytes exclude construction buffers and process RSS"
        }))?
    );
    Ok(())
}
