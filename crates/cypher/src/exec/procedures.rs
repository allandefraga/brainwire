//! Read-only, transaction-local analytics and inspection procedures.
use super::*;
use brainwire_store::graph::{sparse_projection, ProjectionError, ProjectionOptions};
mod knowledge;
mod schema;

fn semantic(message: impl Into<String>) -> ExecError {
    ExecError::Semantic(message.into())
}

pub(super) fn fields(name: &str) -> Result<&'static [&'static str], ExecError> {
    match name {
        "db.schema" => Ok(&["version", "nodes", "relationships", "indexesReady"]),
        "db.indexes" => Ok(&["label", "property", "state"]),
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
        Some(arg) => match scalar(eval(arg, &Bindings::new(), ctx)?)? {
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
    if matches!(name, "db.indexes" | "db.memory" | "db.schema") {
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

fn projection_error(error: ProjectionError) -> ExecError {
    match error {
        ProjectionError::Store(error) => error.into(),
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
    Binding::Value(value)
}
fn int(value: usize) -> Binding {
    Binding::Value(Value::Int(value as i64))
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
    let projection = sparse_projection(ctx.txn, &opts.projection).map_err(projection_error)?;
    ctx.budget.charge(
        projection
            .nodes
            .len()
            .saturating_mul(128)
            .saturating_add(projection.relationship_count.saturating_mul(128)),
    )?;
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
                .map_err(|_| semantic("source node absent from projection"))?;
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
                    .map_err(|_| semantic("target node absent from projection"))?;
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
