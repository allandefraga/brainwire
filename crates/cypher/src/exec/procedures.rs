//! Read-only, transaction-local analytics and inspection procedures.
use super::*;
use brainwire_store::graph::{
    sparse_projection_with_visibility_and_control, ProjectionError, ProjectionOptions,
};
mod knowledge;
mod schema;

fn semantic(message: impl Into<String>) -> ExecError {
    ExecError::Semantic(message.into())
}

pub(super) fn fields(name: &str) -> Result<&'static [&'static str], ExecError> {
    match name {
        "db.schema" => Ok(&["version", "nodes", "relationships", "indexesReady"]),
        "db.commitState" => Ok(&["sequence", "tracked", "scope"]),
        "db.checkpoint" => Ok(&["name", "path", "sequence", "tracked"]),
        "db.setNodeAcl" | "db.setNodeAcls" => Ok(&["updated"]),
        "db.nodeAcls" => Ok(&[
            "label",
            "entityId",
            "allow",
            "deny",
            "connectorIds",
            "sourceTable",
            "sources",
        ]),
        "db.indexes" => Ok(&["label", "property", "state"]),
        "db.labels" => Ok(&["label"]),
        "db.relationshipTypes" => Ok(&["relationshipType"]),
        "db.propertyKeys" => Ok(&["propertyKey"]),
        "dbms.components" => Ok(&["name", "versions", "edition"]),
        "db.memory" => Ok(&[
            "poolLimitBytes",
            "poolReservedBytes",
            "poolPeakBytes",
            "poolRejections",
        ]),
        "graph.project" => Ok(&[
            "nodeCount",
            "relationshipCount",
            "scannedRelationshipCount",
            "nonZeroCount",
            "csrBytes",
        ]),
        "graph.degree" => Ok(&["node", "outDegree", "inDegree"]),
        "graph.wcc" | "graph.scc" => Ok(&["node", "component", "size"]),
        "graph.reachable" | "graph.shortestPath" => Ok(&["node", "depth"]),
        "graph.pagerank" => Ok(&["node", "score", "iterations", "converged", "residual"]),
        "knowledge.rank" => Ok(&["node", "score", "contributions"]),
        _ => Err(ExecError::Unsupported(format!("unknown procedure {name}"))),
    }
}

struct Options {
    projection: ProjectionOptions,
    iterations: usize,
    damping: f64,
    tolerance: f64,
    source: Option<(String, String)>,
    max_depth: usize,
    target: Option<(String, String)>,
}

fn integer(
    map: &BTreeMap<String, Value>,
    key: &str,
    default: usize,
    max: usize,
) -> Result<usize, ExecError> {
    match map.get(key) {
        None => Ok(default),
        Some(Value::Int(v)) if *v > 0 && (*v as u64) <= max as u64 => Ok(*v as usize),
        _ => Err(semantic(format!(
            "{key} must be an integer between 1 and {max}"
        ))),
    }
}
fn number(map: &BTreeMap<String, Value>, key: &str, default: f64) -> Result<f64, ExecError> {
    match map.get(key) {
        None => Ok(default),
        Some(Value::Float(v)) if v.is_finite() => Ok(*v),
        Some(Value::Int(v)) => Ok(*v as f64),
        _ => Err(semantic(format!("{key} must be a finite number"))),
    }
}
fn text(map: &BTreeMap<String, Value>, key: &str) -> Result<Option<String>, ExecError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Str(v)) if !v.is_empty() => Ok(Some(v.clone())),
        _ => Err(semantic(format!("{key} must be a non-empty string"))),
    }
}
fn options(name: &str, args: &[Expr], ctx: &Context<'_, '_>) -> Result<Options, ExecError> {
    if args.len() > 1 {
        return Err(semantic(
            "analytics takes zero arguments or one options map",
        ));
    }
    let map = match args.first() {
        None => BTreeMap::new(),
        Some(arg) => match eval::constant(arg, ctx)? {
            Value::Map(map) => map,
            _ => return Err(semantic("analytics options must be a map")),
        },
    };
    for key in map.keys() {
        let common = ["relationship", "maxNodes", "maxRelationships"];
        let weighted =
            ["graph.project", "graph.pagerank"].contains(&name) && key == "weightProperty";
        let reachable = ["graph.reachable", "graph.shortestPath"].contains(&name)
            && ["source", "sourceLabel", "maxDepth"].contains(&key.as_str());
        let pagerank = name == "graph.pagerank"
            && ["maxIterations", "dampingFactor", "tolerance"].contains(&key.as_str());
        let target =
            name == "graph.shortestPath" && ["target", "targetLabel"].contains(&key.as_str());
        if !common.contains(&key.as_str()) && !weighted && !pagerank && !reachable && !target {
            return Err(semantic(format!("unknown option {key} for {name}")));
        }
    }
    let relationship_type = text(&map, "relationship")?
        .map(|label| {
            ctx.catalog
                .edge_type(&label)
                .map(|ty| ty.id)
                .ok_or_else(|| semantic(format!("unknown relationship type {label}")))
        })
        .transpose()?;
    let damping = number(&map, "dampingFactor", 0.85)?;
    if !(0.0..=1.0).contains(&damping) {
        return Err(semantic("dampingFactor must be between 0 and 1"));
    }
    let tolerance = number(&map, "tolerance", 1e-8)?;
    if tolerance < 0.0 {
        return Err(semantic("tolerance must be non-negative"));
    }
    let source = if ["graph.reachable", "graph.shortestPath"].contains(&name) {
        let id = text(&map, "source")?.ok_or_else(|| semantic("source is required"))?;
        let label =
            text(&map, "sourceLabel")?.ok_or_else(|| semantic("sourceLabel is required"))?;
        if ctx.catalog.node_type(&label).is_none() {
            return Err(semantic(format!("unknown source label {label}")));
        }
        Some((id, label))
    } else {
        None
    };
    let target = if name == "graph.shortestPath" {
        let id = text(&map, "target")?.ok_or_else(|| semantic("target is required"))?;
        let label =
            text(&map, "targetLabel")?.ok_or_else(|| semantic("targetLabel is required"))?;
        if ctx.catalog.node_type(&label).is_none() {
            return Err(semantic(format!("unknown target label {label}")));
        }
        Some((id, label))
    } else {
        None
    };
    Ok(Options {
        projection: ProjectionOptions {
            relationship_type,
            weight_property: text(&map, "weightProperty")?,
            max_nodes: integer(&map, "maxNodes", 100_000, 1_000_000)?,
            max_relationships: integer(&map, "maxRelationships", 1_000_000, 10_000_000)?,
            max_bytes: ctx.budget.remaining(),
        },
        iterations: integer(&map, "maxIterations", 100, 1000)?,
        damping,
        tolerance,
        source,
        max_depth: integer(&map, "maxDepth", 10, 1000)?,
        target,
    })
}

pub(super) fn validate(
    name: &str,
    args: &[Expr],
    yields: &[YieldItem],
    ctx: &Context<'_, '_>,
) -> Result<(), ExecError> {
    let available = fields(name)?;
    if ctx.visibility.is_some()
        && name == "graph.project"
        && yields
            .iter()
            .any(|item| item.field == "scannedRelationshipCount")
    {
        return Err(ExecError::Forbidden(
            "physical relationship scan counts are unavailable with read visibility",
        ));
    }
    if name == "db.setNodeAcl" {
        acl_options(args, ctx)?;
    } else if name == "db.setNodeAcls" {
        acl_batch(args, ctx)?;
    } else if name == "db.checkpoint" {
        checkpoint_destination(args, ctx)?;
    } else if name == "db.nodeAcls" {
        acl_refs(args, ctx)?;
    } else if matches!(
        name,
        "db.indexes"
            | "db.memory"
            | "db.schema"
            | "db.commitState"
            | "db.labels"
            | "db.relationshipTypes"
            | "db.propertyKeys"
            | "dbms.components"
    ) {
        if !args.is_empty() {
            return Err(semantic(format!("{name} takes no arguments")));
        }
    } else if name == "knowledge.rank" {
        knowledge::options(args, ctx)?;
    } else {
        options(name, args, ctx)?;
    }
    let mut aliases = BTreeSet::new();
    for item in yields {
        if !available.contains(&item.field.as_str()) {
            return Err(semantic(format!(
                "unknown YIELD field {} for {name}",
                item.field
            )));
        }
        if !aliases.insert(&item.alias) {
            return Err(semantic(format!("duplicate YIELD alias {}", item.alias)));
        }
    }
    Ok(())
}

/// Only a plain name under the operator-configured root; never a caller path.
fn checkpoint_destination(
    args: &[Expr],
    ctx: &Context<'_, '_>,
) -> Result<(String, std::path::PathBuf), ExecError> {
    let root = ctx
        .checkpoint_dir
        .ok_or_else(|| semantic("db.checkpoint is disabled; configure server.checkpoint_dir"))?;
    if args.len() != 1 {
        return Err(semantic("db.checkpoint requires one checkpoint name"));
    }
    let Value::Str(name) = eval::constant(&args[0], ctx)? else {
        return Err(semantic("checkpoint name must be a string"));
    };
    let valid = (1..=128).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if !valid {
        return Err(semantic(
            "checkpoint name must be 1-128 ASCII letters, digits, '-', '_' or '.', not starting with '.'",
        ));
    }
    let path = root.join(&name);
    // The store also refuses existing paths; this reports it as a client error.
    if path.symlink_metadata().is_ok() {
        return Err(semantic(format!("checkpoint {name:?} already exists")));
    }
    Ok((name, path))
}

fn acl_options(
    args: &[Expr],
    ctx: &Context<'_, '_>,
) -> Result<(String, String, brainwire_core::acl::NodeAcl), ExecError> {
    if args.len() != 1 {
        return Err(semantic("db.setNodeAcl requires one options map"));
    }
    let Value::Map(map) = eval::constant(&args[0], ctx)? else {
        return Err(semantic("db.setNodeAcl options must be a map"));
    };
    acl_map(map, ctx)
}

/// Most entries one `db.setNodeAcls` call accepts; larger loads use several calls.
const MAX_ACL_BATCH: usize = 10_000;

fn acl_batch(
    args: &[Expr],
    ctx: &Context<'_, '_>,
) -> Result<Vec<(String, String, brainwire_core::acl::NodeAcl)>, ExecError> {
    if args.len() != 1 {
        return Err(semantic("db.setNodeAcls requires one list of options maps"));
    }
    let Value::List(entries) = eval::constant(&args[0], ctx)? else {
        return Err(semantic("db.setNodeAcls requires a list of options maps"));
    };
    if entries.len() > MAX_ACL_BATCH {
        return Err(semantic(format!(
            "db.setNodeAcls accepts at most {MAX_ACL_BATCH} entries"
        )));
    }
    entries
        .into_iter()
        .map(|entry| match entry {
            Value::Map(map) => acl_map(map, ctx),
            _ => Err(semantic("db.setNodeAcls entries must be maps")),
        })
        .collect()
}

fn acl_map(
    map: BTreeMap<String, Value>,
    ctx: &Context<'_, '_>,
) -> Result<(String, String, brainwire_core::acl::NodeAcl), ExecError> {
    if map.keys().any(|key| {
        ![
            "label",
            "entityId",
            "allow",
            "deny",
            "connectorIds",
            "sourceTable",
            "sources",
        ]
        .contains(&key.as_str())
    }) {
        return Err(semantic("unknown db.setNodeAcl option"));
    }
    let label = text(&map, "label")?.ok_or_else(|| semantic("label is required"))?;
    if ctx.catalog.node_type(&label).is_none() {
        return Err(semantic("unknown node label"));
    }
    let entity_id = text(&map, "entityId")?.ok_or_else(|| semantic("entityId is required"))?;
    if entity_id.len() > 1024 {
        return Err(semantic("entityId exceeds 1024 bytes"));
    }
    // Cover escaped JSON plus list/string metadata clones before validating/serializing it.
    ctx.budget
        .charge(budget::map_bytes(&map).saturating_mul(6))?;
    let list = |name: &str| -> Result<Vec<String>, ExecError> {
        match map.get(name) {
            None => Ok(Vec::new()),
            Some(Value::List(values)) => values
                .iter()
                .map(|value| match value {
                    Value::Str(value) => Ok(value.clone()),
                    _ => Err(semantic(format!("{name} requires a list of strings"))),
                })
                .collect(),
            _ => Err(semantic(format!("{name} requires a list of strings"))),
        }
    };
    let acl = brainwire_core::acl::NodeAcl {
        allow: list("allow")?,
        deny: list("deny")?,
        connector_ids: list("connectorIds")?,
        source_table: match map.get("sourceTable") {
            None | Some(Value::Null) => None,
            _ => text(&map, "sourceTable")?,
        },
        sources: list("sources")?,
    };
    acl.validate()
        .map_err(|error| semantic(error.to_string()))?;
    Ok((label, entity_id, acl))
}

/// `db.nodeAcls` input: a list of `{label, entityId}` maps, at most
/// [`MAX_ACL_BATCH`], each naming a declared label.
fn acl_refs(args: &[Expr], ctx: &Context<'_, '_>) -> Result<Vec<(String, String)>, ExecError> {
    if args.len() != 1 {
        return Err(semantic(
            "db.nodeAcls requires one list of {label, entityId} maps",
        ));
    }
    let Value::List(entries) = eval::constant(&args[0], ctx)? else {
        return Err(semantic(
            "db.nodeAcls requires a list of {label, entityId} maps",
        ));
    };
    if entries.len() > MAX_ACL_BATCH {
        return Err(semantic(format!(
            "db.nodeAcls accepts at most {MAX_ACL_BATCH} entries"
        )));
    }
    entries
        .into_iter()
        .map(|entry| {
            let Value::Map(map) = entry else {
                return Err(semantic("db.nodeAcls entries must be maps"));
            };
            if map.keys().any(|key| key != "label" && key != "entityId") {
                return Err(semantic("unknown db.nodeAcls option"));
            }
            let label = text(&map, "label")?.ok_or_else(|| semantic("label is required"))?;
            if ctx.catalog.node_type(&label).is_none() {
                return Err(semantic("unknown node label"));
            }
            let entity_id =
                text(&map, "entityId")?.ok_or_else(|| semantic("entityId is required"))?;
            Ok((label, entity_id))
        })
        .collect()
}

/// The private ACL of each named node that exists (an unstamped node answers with
/// empty lists). Trusted administrators only: a read-visibility transaction never
/// sees this namespace.
fn node_acls(
    args: &[Expr],
    ctx: &Context<'_, '_>,
    bind: &dyn Fn(Vec<Binding>) -> Result<Bindings, ExecError>,
) -> Result<Vec<Bindings>, ExecError> {
    if ctx.visibility.is_some() {
        return Err(ExecError::Forbidden(
            "node ACL metadata is unavailable with read visibility",
        ));
    }
    let strings = |values: &[String]| {
        value(Value::List(
            values.iter().map(|v| Value::Str(v.clone())).collect(),
        ))
    };
    let mut rows = Vec::new();
    for (label, entity_id) in acl_refs(args, ctx)? {
        ctx.budget.check_deadline()?;
        let ty = ctx.catalog.node_type(&label).expect("validated label");
        let Some(id) = brainwire_store::graph::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))?
            .first()
            .copied()
        else {
            continue;
        };
        let acl = brainwire_store::graph::get_node_acl(ctx.txn, id)?.unwrap_or_default();
        rows.push(bind(vec![
            value(Value::Str(label)),
            value(Value::Str(entity_id)),
            strings(&acl.allow),
            strings(&acl.deny),
            strings(&acl.connector_ids),
            value(acl.source_table.map_or(Value::Null, Value::Str)),
            strings(&acl.sources),
        ])?);
    }
    Ok(rows)
}

type AclStamp = (NodeId, brainwire_core::acl::NodeAcl);

/// Resolve every entry before any stamp, so a missing node or invalid entry
/// fails the whole call.
pub(super) fn prepare_node_acls(
    args: &[Expr],
    yields: &[YieldItem],
    ctx: &Context<'_, '_>,
) -> Result<(Vec<AclStamp>, Vec<Bindings>), ExecError> {
    let mut stamps = Vec::new();
    for (label, entity_id, acl) in acl_batch(args, ctx)? {
        let ty = ctx.catalog.node_type(&label).expect("validated label");
        let id = brainwire_store::graph::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))?
            .first()
            .copied()
            .ok_or_else(|| semantic(format!("node not found: {label} {entity_id:?}")))?;
        stamps.push((id, acl));
    }
    let row: Bindings = yields
        .iter()
        .map(|item| (item.alias.clone(), int(stamps.len())))
        .collect();
    ctx.budget.bindings(&row)?;
    Ok((stamps, vec![row]))
}

pub(super) fn prepare_node_acl(
    args: &[Expr],
    yields: &[YieldItem],
    ctx: &Context<'_, '_>,
) -> Result<(NodeId, brainwire_core::acl::NodeAcl, Vec<Bindings>), ExecError> {
    let (label, entity_id, acl) = acl_options(args, ctx)?;
    let ty = ctx.catalog.node_type(&label).expect("validated label");
    let id = brainwire_store::graph::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))?
        .first()
        .copied()
        .ok_or_else(|| semantic("node not found"))?;
    let row: Bindings = yields
        .iter()
        .map(|item| (item.alias.clone(), Binding::Bool(true)))
        .collect();
    ctx.budget.bindings(&row)?;
    Ok((id, acl, vec![row]))
}

fn projection_error(error: ProjectionError) -> ExecError {
    match error {
        ProjectionError::Store(error) => error.into(),
        ProjectionError::Interrupted => ExecError::TimedOut,
        ProjectionError::MemoryLimit { limit, attempted } => {
            ExecError::MemoryLimit { limit, attempted }
        }
        ProjectionError::MissingNode => {
            StoreError::Corrupt("projection found an edge with a missing endpoint".into()).into()
        }
        error => semantic(error.to_string()),
    }
}
fn value(value: Value) -> Binding {
    Binding::from(value)
}
fn int(value: usize) -> Binding {
    Binding::Int(value as i64)
}

pub(super) fn run(
    name: &str,
    args: &[Expr],
    yields: &[YieldItem],
    ctx: &Context<'_, '_>,
) -> Result<Vec<Bindings>, ExecError> {
    let available = fields(name)?;
    let bind = |values: Vec<Binding>| -> Result<Bindings, ExecError> {
        let row = yields
            .iter()
            .map(|item| {
                let index = available
                    .iter()
                    .position(|field| *field == item.field)
                    .expect("validated field");
                (item.alias.clone(), values[index].clone())
            })
            .collect();
        ctx.budget.bindings(&row)?;
        Ok(row)
    };
    if name == "db.schema" {
        return Ok(vec![bind(schema::values(ctx, yields.len())?)?]);
    }
    if name == "dbms.components" {
        return Ok(vec![bind(vec![
            value(Value::Str("Brainwire".into())),
            value(Value::List(vec![Value::Str(
                env!("CARGO_PKG_VERSION").into(),
            )])),
            value(Value::Str("community".into())),
        ])?]);
    }
    if matches!(
        name,
        "db.labels" | "db.relationshipTypes" | "db.propertyKeys"
    ) {
        let mut names = BTreeSet::new();
        match name {
            "db.labels" => names.extend(ctx.catalog.node_types.iter().map(|t| t.label.clone())),
            "db.relationshipTypes" => {
                names.extend(ctx.catalog.edge_types.iter().map(|t| t.name.clone()))
            }
            _ => {
                names.insert("entity_id".to_string());
                for ty in &ctx.catalog.node_types {
                    names.extend(ty.columns.iter().map(|c| c.name.clone()));
                }
                for ty in &ctx.catalog.edge_types {
                    names.extend(ty.columns.iter().map(|c| c.name.clone()));
                }
                names.retain(|n| !graph::acl_property(n));
            }
        }
        return names
            .into_iter()
            .map(|n| bind(vec![value(Value::Str(n))]))
            .collect();
    }
    if name == "db.nodeAcls" {
        return node_acls(args, ctx, &bind);
    }
    if name == "db.checkpoint" {
        let (name, path) = checkpoint_destination(args, ctx)?;
        let sequence = ctx.txn.database().checkpoint_with_sequence(&path)?;
        return Ok(vec![bind(vec![
            value(Value::Str(name)),
            value(Value::Str(path.display().to_string())),
            value(Value::Str(sequence.unwrap_or(0).to_string())),
            value(Value::Bool(sequence.is_some())),
        ])?]);
    }
    if name == "db.commitState" {
        let sequence = ctx.txn.snapshot_commit_sequence();
        return Ok(vec![bind(vec![
            value(Value::Str(sequence.unwrap_or(0).to_string())),
            value(Value::Bool(sequence.is_some())),
            value(Value::Str("store_snapshot".into())),
        ])?]);
    }
    if name == "db.memory" {
        let values = match ctx.budget.pool_usage() {
            Some(usage) => [
                usage.limit_bytes,
                usage.reserved_bytes,
                usage.peak_bytes,
                usage.rejections,
            ]
            .into_iter()
            .map(|n| {
                i64::try_from(n)
                    .map(|n| value(Value::Int(n)))
                    .map_err(|_| semantic("memory counter exceeds Cypher integer capacity"))
            })
            .collect::<Result<Vec<_>, _>>()?,
            None => vec![value(Value::Null); 4],
        };
        return Ok(vec![bind(values)?]);
    }
    if name == "db.indexes" {
        let ready = brainwire_store::graph::scalar_indexes_ready(ctx.txn)?;
        let state = if ready {
            "scalar_equality_ready"
        } else {
            "declared_only"
        };
        return ctx
            .catalog
            .node_types
            .iter()
            .flat_map(|ty| {
                ty.indexed.iter().map(|field| {
                    bind(vec![
                        value(Value::Str(ty.label.clone())),
                        value(Value::Str(field.clone())),
                        value(Value::Str(state.into())),
                    ])
                })
            })
            .collect();
    }
    if name == "knowledge.rank" {
        return knowledge::run(args, ctx, bind);
    }
    let opts = options(name, args, ctx)?;
    // Preserve the exact execution/pool error across the store callback boundary.
    let mut budget_error = None;
    let projected = sparse_projection_with_visibility_and_control(
        ctx.txn,
        &opts.projection,
        ctx.visibility,
        |bytes| {
            ctx.budget.charge(bytes).map_err(|error| {
                budget_error = Some(error);
                ProjectionError::Interrupted
            })
        },
    );
    if let Some(error) = budget_error {
        return Err(error);
    }
    let projection = projected.map_err(projection_error)?;
    match name {
        "graph.project" => Ok(vec![bind(vec![
            int(projection.nodes.len()),
            int(projection.relationship_count),
            int(projection.scanned_relationship_count),
            int(projection.matrix.non_zero_count()),
            int(projection.matrix.allocated_bytes()),
        ])?]),
        "graph.degree" => projection
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                bind(vec![
                    Binding::Node(*node),
                    value(Value::Int(projection.out_degrees[index] as i64)),
                    value(Value::Int(projection.in_degrees[index] as i64)),
                ])
            })
            .collect(),
        "graph.wcc" | "graph.scc" => {
            let bytes = projection
                .nodes
                .len()
                .checked_mul(128)
                .and_then(|bytes| {
                    projection
                        .matrix
                        .non_zero_count()
                        .checked_mul(16)
                        .and_then(|edges| bytes.checked_add(edges))
                })
                .ok_or(ExecError::MemoryLimit {
                    limit: ctx.budget.remaining(),
                    attempted: usize::MAX,
                })?;
            // Covers component/DFS/transpose buffers before core allocation, excluding CSR.
            ctx.budget.charge(bytes)?;
            let interrupted = || ctx.budget.check_deadline().is_err();
            let result = if name == "graph.wcc" {
                projection.matrix.weak_components_with_check(interrupted)
            } else {
                projection.matrix.strong_components_with_check(interrupted)
            }
            .map_err(|error| match error {
                brainwire_core::sparse::SparseMatrixError::Interrupted => ExecError::TimedOut,
                error => semantic(error.to_string()),
            })?;
            result
                .components
                .iter()
                .enumerate()
                .map(|(index, representative)| {
                    bind(vec![
                        Binding::Node(projection.nodes[index]),
                        Binding::Node(projection.nodes[*representative]),
                        int(result.sizes[*representative]),
                    ])
                })
                .collect()
        }
        "graph.reachable" | "graph.shortestPath" => {
            let (entity_id, label) = opts.source.as_ref().expect("validated source");
            let ty = ctx.catalog.node_type(label).expect("validated label");
            let ids = brainwire_store::graph::find_by_entity_id(ctx.txn, entity_id, Some(ty.id))?;
            let node = ids
                .first()
                .ok_or_else(|| semantic("source node not found"))?;
            let source = projection
                .nodes
                .binary_search(node)
                .map_err(|_| semantic("source node not found"))?;
            if let Some((entity_id, label)) = &opts.target {
                let ty = ctx
                    .catalog
                    .node_type(label)
                    .expect("validated target label");
                let target =
                    brainwire_store::graph::find_by_entity_id(ctx.txn, entity_id, Some(ty.id))?
                        .first()
                        .copied()
                        .ok_or_else(|| semantic("target node not found"))?;
                let target = projection
                    .nodes
                    .binary_search(&target)
                    .map_err(|_| semantic("target node not found"))?;
                let path = projection
                    .matrix
                    .shortest_path(source, target, opts.max_depth)
                    .map_err(|e| semantic(e.to_string()))?;
                return path
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .map(|(depth, index)| {
                        bind(vec![Binding::Node(projection.nodes[index]), int(depth)])
                    })
                    .collect();
            }
            let depths = projection
                .matrix
                .breadth_first(source, opts.max_depth)
                .map_err(|e| semantic(e.to_string()))?;
            projection
                .nodes
                .iter()
                .zip(depths)
                .filter_map(|(node, depth)| {
                    depth.map(|depth| bind(vec![Binding::Node(*node), int(depth)]))
                })
                .collect()
        }
        "graph.pagerank" => {
            let result = projection
                .matrix
                .pagerank_until(opts.iterations, opts.damping, opts.tolerance)
                .map_err(|e| semantic(e.to_string()))?;
            let mut scores: Vec<_> = projection.nodes.into_iter().zip(result.scores).collect();
            scores.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then(a.cmp(b)));
            scores
                .into_iter()
                .map(|(node, score)| {
                    bind(vec![
                        Binding::Node(node),
                        value(Value::Float(score)),
                        int(result.iterations),
                        value(Value::Bool(result.converged)),
                        value(Value::Float(result.residual)),
                    ])
                })
                .collect()
        }
        _ => unreachable!("validated procedure"),
    }
}

/// Catalog changes made while the server runs (see `brainwire_store::catalog`).
pub(super) fn admin(
    _txn: &mut Txn<'_>,
    _env: &super::run::Env<'_>,
    name: &str,
    _args: &[Expr],
) -> Result<(), ExecError> {
    Err(ExecError::Unsupported(format!("procedure {name}")))
}
