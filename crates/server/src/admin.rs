//! Local database administration. These commands need exclusive RocksDB access.

use std::collections::BTreeMap;
use std::path::Path;

use brainwire_core::schema::{Catalog, GraphMapping};
use brainwire_tui::{
    GraphSummary, OntologyEdge, OntologyNode, OntologySummary, StorageFileSummary,
};

use crate::config;
use crate::databases::Databases;

fn load_config(explicit_config: Option<&Path>) -> Result<config::Config, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("config: {e}"))?;
    let implicit = config::find_file(&cwd);
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let loaded = config::load(explicit_config.or(implicit.as_deref()), &env)
        .map_err(|e| format!("config: {e}"))?;
    Ok(loaded.config)
}

fn open_registry(explicit_config: Option<&Path>) -> Result<Databases, String> {
    Databases::open_all(&load_config(explicit_config)?)
}

fn read_ontology(path: &Path) -> Result<GraphMapping, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read ontology {}: {e}", path.display()))?;
    let mapping: GraphMapping =
        serde_json::from_str(&text).map_err(|e| format!("invalid ontology: {e}"))?;
    Catalog::apply(&mapping, None).map_err(|e| format!("invalid ontology: {e}"))?;
    Ok(mapping)
}

pub fn list(config: Option<&Path>) -> Result<(), String> {
    for name in open_registry(config)?.names() {
        println!("{name}");
    }
    Ok(())
}

fn file_kind(name: &str) -> &'static str {
    match name {
        "CURRENT" => "pointer",
        "LOCK" => "lock",
        "LOG" => "diagnostic",
        _ if name.starts_with("MANIFEST-") => "manifest",
        _ if name.starts_with("OPTIONS-") => "options",
        _ if name.starts_with("LOG.old.") => "diagnostic",
        _ if name.ends_with(".sst") => "sst",
        _ if name.ends_with(".log") => "wal",
        _ => "other",
    }
}

/// Metadata-only, non-atomic inventory; never follows symlinks or reads file contents.
fn inventory(
    db: &brainwire_store::db::Database,
    limit: usize,
) -> Result<(Vec<StorageFileSummary>, bool), String> {
    let live: BTreeMap<_, _> = db
        .live_sst_files()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|f| {
            (
                Path::new(&f.name)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                f,
            )
        })
        .collect();
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for entry in
        std::fs::read_dir(db.directory()).map_err(|e| format!("cannot list database files: {e}"))?
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot inspect file metadata: {e}")),
        };
        if metadata.is_dir() {
            continue;
        }
        total += 1;
        if files.len() == limit && files.last_key_value().is_some_and(|(last, _)| &name > last) {
            continue;
        }
        let symlink = metadata.file_type().is_symlink();
        let table = if symlink { None } else { live.get(&name) };
        files.insert(
            name.clone(),
            StorageFileSummary {
                name: name.clone(),
                kind: if symlink { "symlink" } else { file_kind(&name) }.into(),
                size_bytes: metadata.len(),
                column_family: table.map(|f| f.column_family_name.clone()),
                level: table.map(|f| f.level),
                entries: table.map(|f| f.num_entries),
                deletions: table.map(|f| f.num_deletions),
                state: if symlink {
                    "not_followed"
                } else if table.is_some() {
                    "live_sst_metadata"
                } else {
                    "filesystem_metadata"
                }
                .into(),
            },
        );
        if files.len() > limit {
            files.pop_last();
        }
    }
    Ok((files.into_values().collect(), total > limit))
}
fn file_json(file: &StorageFileSummary) -> serde_json::Value {
    serde_json::json!({"name": file.name, "kind": file.kind, "size_bytes": file.size_bytes,
        "column_family": file.column_family, "level": file.level, "entries": file.entries,
        "deletions": file.deletions, "state": file.state})
}
pub fn files(name: &str, json: bool, limit: usize, config: Option<&Path>) -> Result<(), String> {
    if !(1..=10_000).contains(&limit) {
        return Err("file limit must be between 1 and 10000".into());
    }
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} not found"))?;
    let (files, truncated) = inventory(&db, limit)?;
    if json {
        println!(
            "{}",
            serde_json::json!({"database": name, "scope":"offline_metadata_nonatomic", "files": files.iter().map(file_json).collect::<Vec<_>>(), "truncated": truncated})
        );
    } else {
        for file in files {
            println!(
                "{} kind={} bytes={} cf={} level={}",
                safe_text(&file.name),
                file.kind,
                file.size_bytes,
                file.column_family.as_deref().unwrap_or("?"),
                file.level.map_or_else(|| "?".into(), |v| v.to_string())
            );
        }
        if truncated {
            println!("Inventory truncated; increase --limit to inspect more metadata.");
        }
    }
    Ok(())
}
fn safe_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn graph_summaries(
    config: Option<&Path>,
    estimated: bool,
) -> Result<Vec<GraphSummary>, String> {
    let config = load_config(config)?;
    let registry = Databases::open_all(&config)?;
    let mut graphs = Vec::new();
    for name in registry.names() {
        let db = registry
            .get(&name)
            .ok_or_else(|| format!("database {name:?} disappeared from registry"))?;
        let metrics = db
            .storage_metrics()
            .map_err(|e| format!("cannot inspect graph {name}: {e}"))?;
        let (nodes, edges) = if estimated {
            let count = |family: &str| {
                metrics
                    .get(family)
                    .and_then(|properties| properties.get("estimated_keys"))
                    .copied()
                    .flatten()
                    .ok_or_else(|| format!("RocksDB did not provide {family} key estimate"))
            };
            (count("node")?, count("out")?)
        } else {
            let counts = db
                .stats()
                .map_err(|e| format!("cannot count graph {name}: {e}"))?;
            (counts.nodes, counts.edges)
        };
        let catalog = db
            .catalog()
            .map_err(|e| format!("cannot read ontology of {name}: {e}"))?;
        let (files, files_truncated) = inventory(&db, 256)?;
        graphs.push(GraphSummary {
            ontology: catalog
                .as_ref()
                .map(ontology_summary)
                .transpose()?
                .unwrap_or_default(),
            name,
            nodes,
            edges,
            estimated,
            commit_sequence: db.commit_sequence(),
            ontology_version: catalog.as_ref().map(|c| c.version),
            node_types: catalog.as_ref().map_or(0, |c| c.node_types.len()),
            edge_types: catalog.as_ref().map_or(0, |c| c.edge_types.len()),
            node_labels: catalog.as_ref().map_or_else(Vec::new, |c| {
                c.node_types.iter().map(|ty| ty.label.clone()).collect()
            }),
            edge_labels: catalog.as_ref().map_or_else(Vec::new, |c| {
                c.edge_types.iter().map(|ty| ty.name.clone()).collect()
            }),
            index_declarations: catalog
                .as_ref()
                .map_or(0, |c| c.node_types.iter().map(|ty| ty.indexed.len()).sum()),
            timeline_streams: catalog.as_ref().map_or(0, |c| c.streams.len()),
            indexes_ready: db.scalar_indexes_ready().map_err(|e| e.to_string())?,
            indexed_fields: catalog.as_ref().map_or_else(Vec::new, |c| {
                c.node_types
                    .iter()
                    .flat_map(|ty| ty.indexed.iter().map(|f| format!("{}.{}", ty.label, f)))
                    .collect()
            }),
            storage: metrics
                .into_iter()
                .map(|(cf, props)| {
                    (
                        cf.to_string(),
                        props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                    )
                })
                .collect(),
            cache_budget: config.memory.cache.0,
            memtable_budget: config.memory.memtables.0,
            query_budget: config.memory.query.0,
            query_timeout_ms: config.server.query_timeout_ms,
            cache_accounted: 0,
            memtables_allocated: 0,
            directory: db.directory().display().to_string(),
            files,
            files_truncated,
        });
    }
    let (cache, memtables) = registry.memory_usage();
    for graph in &mut graphs {
        graph.cache_accounted = cache as u64;
        graph.memtables_allocated = memtables as u64;
    }
    Ok(graphs)
}

fn ontology_summary(catalog: &Catalog) -> Result<OntologySummary, String> {
    let nodes = catalog
        .node_types
        .iter()
        .map(|ty| OntologyNode {
            label: ty.label.clone(),
            properties: std::iter::once("entity_id".to_string())
                .chain(
                    ty.columns
                        .iter()
                        .map(|c| c.name.clone())
                        .filter(|name| name != "entity_id"),
                )
                .collect(),
            indexed_fields: ty.indexed.clone(),
        })
        .collect();
    let mut edges = Vec::new();
    for ty in &catalog.edge_types {
        for (from, to) in &ty.signatures {
            let label = |id| {
                catalog
                    .node_types
                    .iter()
                    .find(|n| n.id == id)
                    .map(|n| n.label.clone())
                    .ok_or_else(|| {
                        format!(
                            "ontology relation {} references an unknown node type",
                            ty.name
                        )
                    })
            };
            edges.push(OntologyEdge {
                name: ty.name.clone(),
                source: label(*from)?,
                target: label(*to)?,
                properties: ty.columns.iter().map(|c| c.name.clone()).collect(),
            });
        }
    }
    Ok(OntologySummary { nodes, edges })
}

pub fn overview(json: bool, estimated: bool, config: Option<&Path>) -> Result<(), String> {
    let graphs = graph_summaries(config, estimated)?;
    if json {
        let graphs: Vec<_> = graphs
            .iter()
            .map(|g| {
                serde_json::json!({
                    "name": g.name,
                    "nodes": g.nodes,
                    "edges": g.edges,
                    "count_kind": if g.estimated { "rocksdb_estimate" } else { "exact" },
                    "commit_state": {"sequence": g.commit_sequence.unwrap_or(0).to_string(), "tracked": g.commit_sequence.is_some(), "scope": "store_latest"},
                    "ontology_version": g.ontology_version,
                    "node_types": g.node_types,
                    "edge_types": g.edge_types,
                    "node_labels": g.node_labels,
                    "edge_labels": g.edge_labels,
                    "index_declarations": g.index_declarations,
                    "timeline_streams": g.timeline_streams,
                    "secondary_index_scope": "node",
                    "secondary_index_status": if g.index_declarations == 0 { "none" } else if g.indexes_ready { "scalar_equality_ready" } else { "declared_only" },
                    "indexed_fields": g.indexed_fields,
                    "storage": g.storage,
                    "files": g.files.iter().map(file_json).collect::<Vec<_>>(),
                    "files_truncated": g.files_truncated,
                    "file_inventory_scope": "offline_metadata_nonatomic",
                    "memory_scope": "shared_offline_inspection_process",
                    "cache_budget_bytes": g.cache_budget,
                    "memtable_budget_bytes": g.memtable_budget,
                    "query_budget_bytes": g.query_budget,
                    "query_timeout_ms": g.query_timeout_ms,
                    "query_timeout_scope": "cooperative_per_run",
                    "query_budget_enforced": false,
                    "query_allocation_guard_enforced": true,
                    "query_allocation_guard_scope": "conservative_per_run",
                    "server_query_pool_enforced": true,
                    "server_query_pool_limit_bytes": g.query_budget,
                    "server_query_pool_usage": null,
                    "server_query_pool_usage_scope": "not_observed_offline",
                    "cache_accounted_bytes": g.cache_accounted,
                    "memtables_allocated_bytes": g.memtables_allocated,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "graphs": graphs }));
    } else {
        println!(
            "Brainwire graphs ({})",
            if estimated {
                "RocksDB key estimates; quick"
            } else {
                "exact counts; full scan"
            }
        );
        for graph in graphs {
            let ontology = graph
                .ontology_version
                .map_or_else(|| "none".to_string(), |v| format!("v{v}"));
            println!(
                "{}: nodes={} edges={} ontology={ontology} node_types={} edge_types={}",
                graph.name, graph.nodes, graph.edges, graph.node_types, graph.edge_types
            );
        }
    }
    Ok(())
}

pub fn stats(name: &str, json: bool, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    let counts = db.stats().map_err(|e| format!("cannot count graph: {e}"))?;
    if json {
        println!(
            "{}",
            serde_json::json!({"database":name,"nodes":counts.nodes,"edges":counts.edges})
        );
    } else {
        println!(
            "database={name} nodes={} edges={}",
            counts.nodes, counts.edges
        );
    }
    Ok(())
}

pub fn schema(name: &str, json: bool, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    let catalog = db
        .catalog()
        .map_err(|e| format!("cannot read catalog: {e}"))?;
    let declared = catalog
        .as_ref()
        .is_some_and(|catalog| catalog.node_types.iter().any(|ty| !ty.indexed.is_empty()));
    let index_status = if !declared {
        "none"
    } else if db
        .scalar_indexes_ready()
        .map_err(|e| format!("cannot read index state: {e}"))?
    {
        "scalar_equality_ready"
    } else {
        "declared_only"
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "database": name,
                "catalog": catalog,
                "secondary_index_scope": "node",
                "secondary_index_status": index_status,
                "temporal_history_status": "not_implemented",
                "maintenance_status": "not_implemented",
                "checkpoint_status": "offline_native"
            })
        );
    } else if let Some(catalog) = catalog {
        println!("database={name} catalog_version={}", catalog.version);
        for node in &catalog.node_types {
            println!("node {} indexed_declared={:?}", node.label, node.indexed);
        }
        for edge in &catalog.edge_types {
            println!("edge {}", edge.name);
        }
        for stream in &catalog.streams {
            println!(
                "timeline {} timestamp={}",
                stream.event_type, stream.timestamp_field
            );
        }
        println!("node secondary indexes: {index_status}");
        println!("temporal history and maintenance jobs: not implemented");
    } else {
        println!("database={name} has no ontology catalog");
    }
    Ok(())
}

pub fn metrics(name: &str, json: bool, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    let families = db
        .storage_metrics()
        .map_err(|e| format!("cannot read RocksDB metrics: {e}"))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "database": name,
                "kind": "rocksdb_estimates",
                "column_families": families
            })
        );
    } else {
        println!("database={name} RocksDB estimates (not total process memory)");
        for (family, metrics) in families {
            println!("{family}: {metrics:?}");
        }
    }
    Ok(())
}

pub fn create(name: &str, ontology: Option<&Path>, config: Option<&Path>) -> Result<(), String> {
    let mapping = ontology.map(read_ontology).transpose()?;
    let registry = open_registry(config)?;
    let db = registry.create(name)?;
    if let Some(mapping) = mapping {
        db.apply_ontology(&mapping)
            .map_err(|e| format!("cannot apply ontology: {e}"))?;
    }
    println!("database {name} ready");
    Ok(())
}

pub fn apply_ontology(name: &str, file: &Path, config: Option<&Path>) -> Result<(), String> {
    let mapping = read_ontology(file)?;
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    db.apply_ontology(&mapping)
        .map_err(|e| format!("cannot apply ontology: {e}"))?;
    println!("ontology applied to {name}");
    Ok(())
}

pub fn commit_state(name: &str, json: bool, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    let sequence = db.commit_sequence();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "database": name, "sequence": sequence.unwrap_or(0).to_string(),
                "tracked": sequence.is_some(), "scope": "store_latest"
            })
        );
    } else {
        println!(
            "database={name} commit_sequence={} tracked={} scope=store_latest",
            sequence.unwrap_or(0),
            sequence.is_some()
        );
    }
    Ok(())
}

pub fn checkpoint(name: &str, destination: &Path, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    let sequence = db
        .checkpoint_with_sequence(destination)
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"database":name,"status":"checkpoint_created","destination":destination,"scope":"single_graph_committed_native_checkpoint", "commit_sequence":sequence.unwrap_or(0).to_string(), "commit_sequence_tracked":sequence.is_some()})
    );
    Ok(())
}

/// Deliberate offline audit, never part of a cheap metrics refresh.
pub fn check(name: &str, json: bool, config: Option<&Path>) -> Result<(), String> {
    let registry = open_registry(config)?;
    let db = registry
        .get(name)
        .ok_or_else(|| format!("database {name:?} does not exist"))?;
    brainwire_store::graph::check_invariants(&db)?;
    let ready = db.scalar_indexes_ready().map_err(|e| e.to_string())?;
    let index_status = if ready {
        "checked_active_declarations"
    } else {
        "skipped_not_ready"
    };
    if json {
        println!(
            "{}",
            serde_json::json!({"database": name, "status": "ok", "scalar_indexes": index_status})
        );
    } else {
        println!("database={name} consistency=ok scalar_indexes={index_status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ontology_preserves_every_signature_and_rejects_dangling_types() {
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        let mut catalog = Catalog::apply(&mapping, None).unwrap();
        let summary = ontology_summary(&catalog).unwrap();
        let signatures: Vec<_> = summary
            .edges
            .iter()
            .map(|e| (e.name.as_str(), e.source.as_str(), e.target.as_str()))
            .collect();
        assert_eq!(
            signatures,
            vec![
                ("WORKS_AT", "Customer", "Company"),
                ("IN_SEGMENT", "Company", "Segment"),
                ("IN_SEGMENT", "Customer", "Segment"),
            ]
        );
        assert_eq!(summary.edges[0].properties, ["role"]);
        // The catalog also indexes entity-resolution fields and sorts declarations.
        assert_eq!(
            summary.nodes[0].indexed_fields,
            ["email", "phone", "segment"]
        );
        assert_eq!(
            summary.nodes[0]
                .properties
                .iter()
                .filter(|p| *p == "entity_id")
                .count(),
            1
        );
        catalog.node_types.retain(|n| n.label != "Company");
        assert!(ontology_summary(&catalog)
            .unwrap_err()
            .contains("unknown node type"));
    }
}
