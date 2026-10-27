//! Explainable source ranking inspired by OntoRank, not its proprietary algorithm.
use super::*;
use brainwire_store::graph::{visit_by_indexed_property, visit_nodes_of_type};
use std::{cmp::Ordering, collections::BinaryHeap};

// Explicit policy v1: supplied normalized evidence, never inferred permissions.
const SIGNALS: [(&str, f64); 5] = [
    ("authority", 0.30),
    ("authorRelevance", 0.20),
    ("usage", 0.10),
    ("certified", 0.25),
    ("freshness", 0.15),
];

pub(super) struct RankOptions {
    label: String,
    term: String,
    domain: Option<String>,
    top_k: usize,
    max_candidates: usize,
    weights: [f64; 5],
}

fn weights(options: &BTreeMap<String, Value>) -> Result<[f64; 5], ExecError> {
    let Some(value) = options.get("weights") else {
        return Ok(SIGNALS.map(|(_, weight)| weight));
    };
    let Value::Map(supplied) = value else {
        return Err(semantic(
            "weights must be a map with all five ranking signals",
        ));
    };
    if supplied.len() != SIGNALS.len()
        || SIGNALS
            .iter()
            .any(|(name, _)| !supplied.contains_key(*name))
        || supplied
            .keys()
            .any(|key| !SIGNALS.iter().any(|(name, _)| *name == key.as_str()))
    {
        return Err(semantic(
            "weights must specify authority, authorRelevance, usage, certified and freshness",
        ));
    }
    let mut values = [0.0; 5];
    for (index, (name, _)) in SIGNALS.iter().enumerate() {
        values[index] = match &supplied[*name] {
            Value::Int(weight) if *weight >= 0 => *weight as f64,
            Value::Float(weight) if weight.is_finite() && *weight >= 0.0 => *weight,
            _ => {
                return Err(semantic(format!(
                    "weight {name} must be a finite non-negative number"
                )))
            }
        };
    }
    let total: f64 = values.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return Err(semantic("weights must have a positive finite sum"));
    }
    values.iter_mut().for_each(|weight| *weight /= total);
    Ok(values)
}

pub(super) fn options(args: &[Expr], ctx: &Context<'_, '_>) -> Result<RankOptions, ExecError> {
    if args.len() != 1 {
        return Err(semantic("knowledge.rank requires one options map"));
    }
    let Value::Map(map) = eval::constant(&args[0], ctx)? else {
        return Err(semantic("knowledge.rank options must be a map"));
    };
    for key in map.keys() {
        if ![
            "label",
            "term",
            "domain",
            "topK",
            "maxCandidates",
            "weights",
        ]
        .contains(&key.as_str())
        {
            return Err(semantic(format!("unknown option {key} for knowledge.rank")));
        }
    }
    let label = text(&map, "label")?.ok_or_else(|| semantic("label is required"))?;
    if ctx.catalog.node_type(&label).is_none() {
        return Err(semantic(format!("unknown label {label}")));
    }
    Ok(RankOptions {
        label,
        term: text(&map, "term")?.ok_or_else(|| semantic("term is required"))?,
        domain: text(&map, "domain")?,
        top_k: integer(&map, "topK", 10, 1000)?,
        max_candidates: integer(&map, "maxCandidates", 10_000, 1_000_000)?,
        weights: weights(&map)?,
    })
}

struct Candidate {
    node: NodeId,
    score: f64,
    contributions: [f64; 5],
}
impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Heap root is the worst retained result; stable ties favor lower node IDs.
        other
            .score
            .total_cmp(&self.score)
            .then(self.node.cmp(&other.node))
    }
}
fn contributions(
    props: &BTreeMap<String, Value>,
    weights: &[f64; 5],
) -> Result<[f64; 5], ExecError> {
    let mut parts = [0.0; 5];
    for (i, (name, _)) in SIGNALS.iter().enumerate() {
        let signal = match props.get(*name) {
            None | Some(Value::Null) => 0.0,
            Some(Value::Bool(certified)) if *name == "certified" => {
                if *certified {
                    1.0
                } else {
                    0.0
                }
            }
            Some(Value::Int(n)) if *name != "certified" && (0..=1).contains(n) => *n as f64,
            Some(Value::Float(n))
                if *name != "certified" && n.is_finite() && (0.0..=1.0).contains(n) =>
            {
                *n
            }
            _ => {
                return Err(semantic(format!(
                    "{name} must be {} or null",
                    if *name == "certified" {
                        "a boolean"
                    } else {
                        "a finite number between 0 and 1"
                    }
                )))
            }
        };
        parts[i] = signal * weights[i];
    }
    Ok(parts)
}

pub(super) fn run(
    args: &[Expr],
    ctx: &Context<'_, '_>,
    bind: impl Fn(Vec<Binding>) -> Result<Bindings, ExecError>,
) -> Result<Vec<Bindings>, ExecError> {
    let opts = options(args, ctx)?;
    let ty = ctx.catalog.node_type(&opts.label).expect("validated label");
    // Cover heap capacity growth, not every candidate. Row scopes release temporary data.
    ctx.budget.charge(opts.top_k.saturating_mul(256))?;
    let mut best = BinaryHeap::new();
    let mut scanned = 0;
    let term = Value::Str(opts.term);
    let domain = opts.domain.map(Value::Str);
    let mut visit = |id| -> Result<(), ExecError> {
        let _scope = ctx.budget.scope();
        ctx.budget.check_deadline()?;
        scanned += 1;
        if scanned > opts.max_candidates {
            return Err(semantic(
                "knowledge.rank exceeded maxCandidates; no partial ranking is returned",
            ));
        }
        if !graph::visible(ctx, id)? {
            return Ok(());
        }
        let node = graph::node_out(ctx, id)?;
        if node.props.get("term") != Some(&term)
            || domain
                .as_ref()
                .is_some_and(|d| node.props.get("domain") != Some(d))
        {
            return Ok(());
        }
        let parts = contributions(&node.props, &opts.weights)?;
        let candidate = Candidate {
            node: id,
            score: parts.iter().sum(),
            contributions: parts,
        };
        if best.len() < opts.top_k {
            best.push(candidate);
        } else if best.peek().is_some_and(|worst| candidate < *worst) {
            *best.peek_mut().expect("nonempty heap") = candidate;
        }
        Ok(())
    };
    // Reuse physical equality indexes and transaction overlays, with a streaming fallback.
    if !visit_by_indexed_property::<ExecError>(ctx.txn, ty, "term", &term, &mut visit)? {
        visit_nodes_of_type::<ExecError>(ctx.txn, ty.id, visit)?;
    }
    best.into_sorted_vec()
        .into_iter()
        .map(|candidate| {
            let parts = SIGNALS
                .iter()
                .zip(candidate.contributions)
                .map(|((name, _), part)| ((*name).into(), Value::Float(part)))
                .collect();
            bind(vec![
                Binding::Node(candidate.node),
                value(Value::Float(candidate.score)),
                value(Value::Map(parts)),
            ])
        })
        .collect()
}
