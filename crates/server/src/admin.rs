//! Local database administration. These commands need exclusive RocksDB access.

use std::collections::BTreeMap;
use std::path::Path;

use brainwire_core::schema::{Catalog, GraphMapping};

use crate::config;
use crate::databases::Databases;

fn open_registry(explicit_config: Option<&Path>) -> Result<Databases, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("config: {e}"))?;
    let implicit = config::find_file(&cwd);
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let loaded = config::load(explicit_config.or(implicit.as_deref()), &env)
        .map_err(|e| format!("config: {e}"))?;
    Databases::open_all(&loaded.config)
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
    if json {
        println!(
            "{}",
            serde_json::json!({
                "database": name,
                "catalog": catalog,
                "secondary_index_status": "declared_only",
                "temporal_history_status": "not_implemented",
                "maintenance_status": "not_implemented"
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
        println!("secondary indexes: declared only; property scans are used");
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
