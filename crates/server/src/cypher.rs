//! Embedded, exclusive local-store execution. JSON output is {fields, rows};
//! scalars/lists/maps are ordinary JSON, nodes are {type:"node", id, label,
//! properties}, including virtual entity_id. The executor is eager; the loaded
//! query-memory setting is not enforced. Ontology application precedes the query
//! in its own transaction. An output/I/O error can occur after a write committed.
use brainwire_core::{
    schema::{Catalog, GraphMapping},
    value::Value,
};
use brainwire_cypher::{ExecError, Out};
use brainwire_store::db::{Database, Resources, StoreConfig};
use serde_json::{json, Value as Json};
use std::{collections::BTreeMap, io::Write, path::Path};

pub fn run(
    database: &Path,
    query: &str,
    ontology: Option<&Path>,
    params: &str,
    config: Option<&Path>,
) -> Result<(), String> {
    let params = parse_params(params)?;
    let ontology: Option<GraphMapping> = ontology
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("ontology: {}: {e}", path.display()))?;
            let mapping = serde_json::from_str(&text).map_err(|e| format!("ontology: {e}"))?;
            // New stores can validate the catalog before any filesystem mutation.
            // Existing additive mappings must validate against their stored catalog.
            if !database.join("CURRENT").is_file() {
                Catalog::apply(&mapping, None).map_err(|e| format!("ontology: {e}"))?;
            }
            Ok::<_, String>(mapping)
        })
        .transpose()?;
    let cwd = std::env::current_dir().map_err(|e| format!("config: {e}"))?;
    let implicit = crate::config::find_file(&cwd);
    let env = std::env::vars().collect();
    let loaded = crate::config::load(config.or(implicit.as_deref()), &env)
        .map_err(|e| format!("config: {e}"))?;
    let memory = loaded.config.memory;
    let resources = Resources::new(StoreConfig {
        cache_bytes: usize::try_from(memory.cache.0)
            .map_err(|_| "config: cache exceeds platform address size")?,
        memtable_bytes: usize::try_from(memory.memtables.0)
            .map_err(|_| "config: memtables exceed platform address size")?,
        id_block: 1024,
        conflict_history_keys: 1_000_000,
    });
    let db = Database::open(database, &resources).map_err(|e| format!("storage: cannot open {}: {e}. This command needs exclusive local access; close any process using this database before retrying.", database.display()))?;
    if let Some(mapping) = ontology {
        db.apply_ontology(&mapping)
            .map_err(|e| format!("ontology: {e}"))?;
    }
    let output = brainwire_cypher::execute(&db, query, &params)
        .map_err(|e| format!("{}: {e}", category(&e)))?;
    let rows = output
        .rows
        .iter()
        .map(|row| row.iter().map(out_json).collect::<Result<Vec<_>, _>>())
        .collect::<Result<Vec<_>, _>>()?;
    let text = serde_json::to_string_pretty(&json!({"fields": output.fields, "rows": rows}))
        .map_err(|e| format!("output: {e}"))?;
    writeln!(std::io::stdout().lock(), "{text}").map_err(|e| format!("output: {e}"))
}
fn category(e: &ExecError) -> &'static str {
    match e {
        ExecError::Syntax(_) => "syntax",
        ExecError::Unsupported(_) => "unsupported",
        ExecError::Semantic(_) => "semantic",
        ExecError::ParameterMissing(_) => "parameter",
        ExecError::ConstraintViolation(_) => "constraint",
        ExecError::Conflict => "conflict",
        ExecError::Graph(_) => "storage",
    }
}
fn parse_params(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let json: Json = serde_json::from_str(text).map_err(|e| format!("parameters: {e}"))?;
    let Json::Object(map) = json else {
        return Err("parameters: --params must be a JSON object".into());
    };
    map.into_iter()
        .map(|(k, v)| from_json(v).map(|v| (k, v)))
        .collect()
}
fn from_json(v: Json) -> Result<Value, String> {
    Ok(match v {
        Json::Null => Value::Null,
        Json::Bool(v) => Value::Bool(v),
        Json::String(v) => Value::Str(v),
        Json::Number(v) => {
            let text = v.to_string();
            if !text.contains(['.', 'e', 'E']) {
                Value::Int(
                    text.parse()
                        .map_err(|_| "parameters: integer outside signed i64 range")?,
                )
            } else {
                let n: f64 = text.parse().map_err(|_| "parameters: invalid float")?;
                if !n.is_finite() {
                    return Err("parameters: nonfinite float".into());
                }
                Value::Float(n)
            }
        }
        Json::Array(v) => Value::List(v.into_iter().map(from_json).collect::<Result<_, _>>()?),
        Json::Object(v) => Value::Map(
            v.into_iter()
                .map(|(k, v)| from_json(v).map(|v| (k, v)))
                .collect::<Result<_, _>>()?,
        ),
    })
}
fn value_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::Null => Json::Null,
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Str(v) => json!(v),
        Value::Float(v) => Json::Number(
            serde_json::Number::from_f64(*v)
                .ok_or("output: nonfinite stored float cannot be represented as JSON")?,
        ),
        Value::List(v) => Json::Array(v.iter().map(value_json).collect::<Result<_, _>>()?),
        Value::Map(v) => Json::Object(
            v.iter()
                .map(|(k, v)| value_json(v).map(|v| (k.clone(), v)))
                .collect::<Result<_, _>>()?,
        ),
    })
}
fn out_json(v: &Out) -> Result<Json, String> {
    match v {
        Out::Value(v) => value_json(v),
        Out::Node(n) => Ok(
            json!({"type": "node", "id": n.id.0, "label": n.label, "properties": value_json(&Value::Map(n.props.clone()))?}),
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonfinite_output_is_never_null() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(value_json(&Value::List(vec![Value::Float(v)]))
                .unwrap_err()
                .contains("nonfinite"));
        }
    }
}
