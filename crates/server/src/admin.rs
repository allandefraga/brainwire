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
