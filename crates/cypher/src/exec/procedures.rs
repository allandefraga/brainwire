//! Read-only, transaction-local sparse analytics procedures.
use super::*;
use brainwire_store::graph::{sparse_projection, ProjectionError, ProjectionOptions};

fn semantic(message: impl Into<String>) -> ExecError {
    ExecError::Semantic(message.into())
}

pub(super) fn fields(name: &str) -> Result<&'static [&'static str], ExecError> {
    match name {
        "graph.project" => Ok(&[
            "nodeCount",
            "relationshipCount",
            "scannedRelationshipCount",
            "nonZeroCount",
            "csrBytes",
        ]),
        "graph.degree" => Ok(&["node", "outDegree", "inDegree"]),
        "graph.pagerank" => Ok(&["node", "score", "iterations", "converged", "residual"]),
        _ => Err(ExecError::Unsupported(format!("unknown procedure {name}"))),
    }
}

struct Options {
    projection: ProjectionOptions,
    iterations: usize,
    damping: f64,
    tolerance: f64,
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
        let weighted = name != "graph.degree" && key == "weightProperty";
        let pagerank = name == "graph.pagerank"
            && ["maxIterations", "dampingFactor", "tolerance"].contains(&key.as_str());
        if !common.contains(&key.as_str()) && !weighted && !pagerank {
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
    Ok(Options {
        projection: ProjectionOptions {
            relationship_type,
            weight_property: text(&map, "weightProperty")?,
            max_nodes: integer(&map, "maxNodes", 100_000, 1_000_000)?,
            max_relationships: integer(&map, "maxRelationships", 1_000_000, 10_000_000)?,
        },
        iterations: integer(&map, "maxIterations", 100, 1000)?,
        damping,
        tolerance,
    })
}

pub(super) fn validate(
    name: &str,
    args: &[Expr],
    yields: &[YieldItem],
    ctx: &Context<'_, '_>,
) -> Result<(), ExecError> {
    let available = fields(name)?;
    options(name, args, ctx)?;
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
    let opts = options(name, args, ctx)?;
    let projection = sparse_projection(ctx.txn, &opts.projection).map_err(projection_error)?;
    let available = fields(name)?;
    let bind = |values: Vec<Binding>| -> Bindings {
        yields
            .iter()
            .map(|item| {
                let index = available
                    .iter()
                    .position(|field| *field == item.field)
                    .expect("validated field");
                (item.alias.clone(), values[index].clone())
            })
            .collect()
    };
    match name {
        "graph.project" => Ok(vec![bind(vec![
            int(projection.nodes.len()),
            int(projection.relationship_count),
            int(projection.scanned_relationship_count),
            int(projection.matrix.non_zero_count()),
            int(projection.matrix.allocated_bytes()),
        ])]),
        "graph.degree" => Ok(projection
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
            .collect()),
        "graph.pagerank" => {
            let result = projection
                .matrix
                .pagerank_until(opts.iterations, opts.damping, opts.tolerance)
                .map_err(|e| semantic(e.to_string()))?;
            let mut scores: Vec<_> = projection.nodes.into_iter().zip(result.scores).collect();
            scores.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then(a.cmp(b)));
            Ok(scores
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
                .collect())
        }
        _ => unreachable!("validated procedure"),
    }
}
