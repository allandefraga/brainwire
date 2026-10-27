//! Expression evaluation over a slot row.
use super::val::{compare, equals, PathVal, Val};
use super::*;
use std::cmp::Ordering;

fn semantic(message: impl Into<String>) -> ExecError {
    ExecError::Semantic(message.into())
}

fn type_error(what: &str, value: &Val) -> ExecError {
    ExecError::Semantic(format!("{what} does not accept {}", value.type_name()))
}

pub(super) fn truth(value: Option<bool>) -> Val {
    value.map(Val::Bool).unwrap_or(Val::Null)
}

/// `true`, `false` or unknown (`None`) for a predicate result.
pub(super) fn boolean(value: Val) -> Result<Option<bool>, ExecError> {
    match value {
        Val::Null => Ok(None),
        Val::Bool(b) => Ok(Some(b)),
        other => Err(semantic(format!(
            "expected boolean or null, found {}",
            other.type_name()
        ))),
    }
}

/// Evaluate a row-independent expression (procedure arguments, SKIP, LIMIT).
pub(super) fn constant(expr: &Expr, ctx: &Context<'_, '_>) -> Result<Value, ExecError> {
    eval(expr, &[], ctx)?.into_value()
}

pub(super) fn predicate(
    expr: &Expr,
    row: &[Val],
    ctx: &Context<'_, '_>,
) -> Result<bool, ExecError> {
    Ok(boolean(eval(expr, row, ctx)?)? == Some(true))
}

fn var<'r>(name: &str, row: &'r [Val], ctx: &Context<'_, '_>) -> Result<&'r Val, ExecError> {
    ctx.vars
        .slot(name)
        .and_then(|slot| row.get(slot))
        .ok_or_else(|| semantic(format!("unknown variable {name}")))
}

/// Bind `name` in a copy of `row` (list comprehension and reduce scopes).
fn with_var(
    row: &[Val],
    name: &str,
    value: Val,
    ctx: &Context<'_, '_>,
) -> Result<Vec<Val>, ExecError> {
    let slot = ctx
        .vars
        .slot(name)
        .ok_or_else(|| semantic(format!("unknown variable {name}")))?;
    let mut next = row.to_vec();
    if next.len() <= slot {
        next.resize(slot + 1, Val::Null);
    }
    next[slot] = value;
    Ok(next)
}

pub(super) fn eval(expr: &Expr, row: &[Val], ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    let value = match expr {
        Expr::Literal(v) => {
            ctx.budget.charge(budget::value_bytes(v))?;
            Val::from(v)
        }
        Expr::Param(name) => {
            if let Some(value) = ctx.param_vals.and_then(|vals| vals.get(name)) {
                ctx.budget.charge(value.bytes())?;
                return Ok(value.clone());
            }
            let value = ctx
                .params
                .get(name)
                .ok_or_else(|| ExecError::ParameterMissing(name.clone()))?;
            ctx.budget.charge(budget::value_bytes(value))?;
            Val::from(value)
        }
        Expr::Var(name) => {
            let value = var(name, row, ctx)?;
            ctx.budget.charge(value.bytes())?;
            return Ok(value.clone());
        }
        Expr::Prop(base, name) => match base.as_ref() {
            // Read through a variable without copying the whole map or row value.
            Expr::Var(variable) => match var(variable, row, ctx)? {
                Val::Map(map) => map.get(name).cloned().unwrap_or(Val::Null),
                Val::Node(id) => Val::from(graph::node_property(ctx, *id, name)?),
                Val::Rel(edge) => Val::from(graph::edge_property(ctx, edge, name)?),
                Val::Null => Val::Null,
                other => property(other.clone(), name, ctx)?,
            },
            base => property(eval(base, row, ctx)?, name, ctx)?,
        },
        Expr::Call {
            name,
            distinct: _,
            args,
        } => return call(name, args, row, ctx),
        Expr::CountStar => {
            return Err(ExecError::Unsupported(
                "aggregates are only supported in WITH and RETURN items".into(),
            ))
        }
        Expr::List(items) => Val::List(
            items
                .iter()
                .map(|e| eval(e, row, ctx))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Map(items) => Val::Map(
            items
                .iter()
                .map(|(k, e)| Ok((k.clone(), eval(e, row, ctx)?)))
                .collect::<Result<_, ExecError>>()?,
        ),
        Expr::Cmp(a, op, b) => {
            let (a, b) = (eval(a, row, ctx)?, eval(b, row, ctx)?);
            return Ok(truth(cmp(&a, *op, &b)));
        }
        Expr::Arith(a, op, b) => arith(eval(a, row, ctx)?, *op, eval(b, row, ctx)?)?,
        Expr::Neg(a) => match eval(a, row, ctx)? {
            Val::Null => Val::Null,
            Val::Int(i) => Val::Int(
                i.checked_neg()
                    .ok_or_else(|| semantic("integer overflow"))?,
            ),
            Val::Float(f) => Val::Float(-f),
            other => return Err(type_error("unary minus", &other)),
        },
        Expr::Text(a, op, b) => match (eval(a, row, ctx)?, eval(b, row, ctx)?) {
            (Val::Str(a), Val::Str(b)) => Val::Bool(match op {
                TextOp::StartsWith => a.starts_with(&b),
                TextOp::EndsWith => a.ends_with(&b),
                TextOp::Contains => a.contains(&b),
            }),
            _ => Val::Null,
        },
        Expr::IsNull(a, negated) => {
            let null = eval(a, row, ctx)?.is_null();
            Val::Bool(null != *negated)
        }
        Expr::In(a, b) => {
            let value = eval(a, row, ctx)?;
            // A parameter list is borrowed, not copied, for every row.
            if let Expr::Param(name) = b.as_ref() {
                if let Some(list) = ctx.param_vals.and_then(|vals| vals.get(name)) {
                    return membership(&value, list);
                }
            }
            membership(&value, &eval(b, row, ctx)?)?
        }
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Xor(a, b) => {
            // Evaluate both operands: invalid operand types must not be hidden by short circuiting.
            let a = boolean(eval(a, row, ctx)?)?;
            let b = boolean(eval(b, row, ctx)?)?;
            truth(match expr {
                Expr::And(..) => match (a, b) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                },
                Expr::Or(..) => match (a, b) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                },
                _ => a.zip(b).map(|(a, b)| a != b),
            })
        }
        Expr::Not(a) => truth(boolean(eval(a, row, ctx)?)?.map(|v| !v)),
        Expr::HasLabels(a, labels) => match eval(a, row, ctx)? {
            Val::Null => Val::Null,
            Val::Node(id) => {
                let label = graph::label_of(ctx, id)?;
                Val::Bool(labels.iter().all(|l| l == label))
            }
            other => return Err(type_error("a label predicate", &other)),
        },
        Expr::Index(base, index) => {
            index_value(eval(base, row, ctx)?, eval(index, row, ctx)?, ctx)?
        }
        Expr::Slice(base, from, to) => {
            let base = eval(base, row, ctx)?;
            let bound = |e: &Option<Box<Expr>>| -> Result<Option<Val>, ExecError> {
                e.as_ref().map(|e| eval(e, row, ctx)).transpose()
            };
            slice(base, bound(from)?, bound(to)?)?
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            let operand = operand.as_ref().map(|e| eval(e, row, ctx)).transpose()?;
            for (when, then) in whens {
                let hit = match &operand {
                    Some(value) => equals(value, &eval(when, row, ctx)?) == Some(true),
                    None => predicate(when, row, ctx)?,
                };
                if hit {
                    return eval(then, row, ctx);
                }
            }
            match otherwise {
                Some(e) => return eval(e, row, ctx),
                None => Val::Null,
            }
        }
        Expr::ListComp {
            var,
            list,
            filter,
            map,
        } => {
            let items = match eval(list, row, ctx)? {
                Val::Null => return Ok(Val::Null),
                Val::List(items) => items,
                other => return Err(type_error("a list comprehension", &other)),
            };
            let mut out = Vec::new();
            for item in items {
                let scoped = with_var(row, var, item.clone(), ctx)?;
                if let Some(filter) = filter {
                    if !predicate(filter, &scoped, ctx)? {
                        continue;
                    }
                }
                out.push(match map {
                    Some(map) => eval(map, &scoped, ctx)?,
                    None => item,
                });
            }
            Val::List(out)
        }
        Expr::Quantifier {
            kind,
            var,
            list,
            predicate: test,
        } => {
            let items = match eval(list, row, ctx)? {
                Val::Null => return Ok(Val::Null),
                Val::List(items) => items,
                other => return Err(type_error("a list predicate", &other)),
            };
            let (mut hits, mut misses, mut unknown) = (0usize, 0usize, false);
            for item in &items {
                let scoped = with_var(row, var, item.clone(), ctx)?;
                match boolean(eval(test, &scoped, ctx)?)? {
                    Some(true) => hits += 1,
                    Some(false) => misses += 1,
                    None => unknown = true,
                }
            }
            truth(match kind {
                Quantifier::All if misses > 0 => Some(false),
                Quantifier::Any | Quantifier::None if hits > 0 => Some(*kind == Quantifier::Any),
                Quantifier::Single if hits > 1 => Some(false),
                _ if unknown => None,
                Quantifier::All => Some(true),
                Quantifier::Any => Some(false),
                Quantifier::None => Some(true),
                Quantifier::Single => Some(hits == 1),
            })
        }
        Expr::Reduce {
            acc,
            init,
            var,
            list,
            expr,
        } => {
            let mut value = eval(init, row, ctx)?;
            let items = match eval(list, row, ctx)? {
                Val::Null => return Ok(Val::Null),
                Val::List(items) => items,
                other => return Err(type_error("reduce", &other)),
            };
            for item in items {
                let scoped = with_var(row, acc, value, ctx)?;
                let scoped = with_var(&scoped, var, item, ctx)?;
                value = eval(expr, &scoped, ctx)?;
            }
            value
        }
        Expr::MapProjection { var: name, items } => {
            let base = var(name, row, ctx)?.clone();
            if base.is_null() {
                return Ok(Val::Null);
            }
            let mut out = BTreeMap::new();
            for item in items {
                match item {
                    MapItem::Prop(key) => {
                        out.insert(key.clone(), property(base.clone(), key, ctx)?);
                    }
                    MapItem::AllProps => {
                        if let Val::Map(all) = properties(&base, ctx)? {
                            out.extend(all);
                        }
                    }
                    MapItem::Entry(key, e) => {
                        out.insert(key.clone(), eval(e, row, ctx)?);
                    }
                }
            }
            Val::Map(out)
        }
        Expr::Exists(pattern) => Val::Bool(graph::exists(ctx, row, pattern)?),
    };
    ctx.budget.charge(value.bytes())?;
    Ok(value)
}

pub(super) fn property(base: Val, name: &str, ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    Ok(match base {
        Val::Node(id) => Val::from(graph::node_property(ctx, id, name)?),
        Val::Rel(edge) => Val::from(graph::edge_property(ctx, &edge, name)?),
        Val::Map(mut map) => map.remove(name).unwrap_or(Val::Null),
        Val::Null => Val::Null,
        other => {
            return Err(semantic(format!(
                "property access needs a node, relationship or map, found {}",
                other.type_name()
            )))
        }
    })
}

fn properties(base: &Val, ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    Ok(match base {
        Val::Node(id) => Val::from(Value::Map(graph::node_out(ctx, *id)?.props)),
        Val::Rel(edge) => Val::from(Value::Map(graph::edge_props(ctx, edge)?)),
        Val::Map(map) => Val::Map(map.clone()),
        Val::Null => Val::Null,
        other => return Err(type_error("properties", other)),
    })
}

pub(super) fn cmp(a: &Val, op: CmpOp, b: &Val) -> Option<bool> {
    if a.is_null() || b.is_null() {
        return None;
    }
    match op {
        CmpOp::Eq => equals(a, b),
        CmpOp::Ne => equals(a, b).map(|eq| !eq),
        _ => {
            // Lists compare element-wise; other cross-type comparisons are unknown.
            let ord = compare(a, b)?;
            Some(match op {
                CmpOp::Lt => ord.is_lt(),
                CmpOp::Le => ord.is_le(),
                CmpOp::Gt => ord.is_gt(),
                _ => ord.is_ge(),
            })
        }
    }
}

/// Cypher list membership: true on an equal element; otherwise null when the value
/// or any element is null (the answer is unknown), else false. A null list is null.
fn membership(value: &Val, list: &Val) -> Result<Val, ExecError> {
    let items = match list {
        Val::Null => return Ok(Val::Null),
        Val::List(items) => items,
        other => return Err(type_error("IN", other)),
    };
    let mut unknown = false;
    for item in items {
        match equals(value, item) {
            Some(true) => return Ok(Val::Bool(true)),
            None => unknown = true,
            Some(false) => {}
        }
    }
    Ok(truth(if unknown { None } else { Some(false) }))
}

fn float(v: &Val) -> Option<f64> {
    match v {
        Val::Int(i) => Some(*i as f64),
        Val::Float(f) => Some(*f),
        _ => None,
    }
}

fn arith(a: Val, op: ArithOp, b: Val) -> Result<Val, ExecError> {
    if a.is_null() || b.is_null() {
        return Ok(Val::Null);
    }
    let overflow = || semantic("integer overflow");
    Ok(match (op, a, b) {
        (ArithOp::Add, Val::List(mut a), Val::List(b)) => {
            a.extend(b);
            Val::List(a)
        }
        (ArithOp::Add, Val::List(mut a), b) => {
            a.push(b);
            Val::List(a)
        }
        (ArithOp::Add, a, Val::List(b)) => {
            let mut out = vec![a];
            out.extend(b);
            Val::List(out)
        }
        (ArithOp::Add, Val::Str(a), b) => Val::Str(a + &text(&b)?),
        (ArithOp::Add, a, Val::Str(b)) => Val::Str(text(&a)? + &b),
        (op, Val::Int(a), Val::Int(b)) => match op {
            ArithOp::Add => Val::Int(a.checked_add(b).ok_or_else(overflow)?),
            ArithOp::Sub => Val::Int(a.checked_sub(b).ok_or_else(overflow)?),
            ArithOp::Mul => Val::Int(a.checked_mul(b).ok_or_else(overflow)?),
            ArithOp::Div if b == 0 => return Err(semantic("/ by zero")),
            ArithOp::Div => Val::Int(a.checked_div(b).ok_or_else(overflow)?),
            ArithOp::Mod if b == 0 => return Err(semantic("% by zero")),
            ArithOp::Mod => Val::Int(a.checked_rem(b).ok_or_else(overflow)?),
            ArithOp::Pow => Val::Float((a as f64).powf(b as f64)),
        },
        (op, a, b) => {
            let (Some(x), Some(y)) = (float(&a), float(&b)) else {
                return Err(semantic(format!(
                    "cannot apply arithmetic to {} and {}",
                    a.type_name(),
                    b.type_name()
                )));
            };
            Val::Float(match op {
                ArithOp::Add => x + y,
                ArithOp::Sub => x - y,
                ArithOp::Mul => x * y,
                ArithOp::Div => x / y,
                ArithOp::Mod => x % y,
                ArithOp::Pow => x.powf(y),
            })
        }
    })
}

/// String form used by `+` concatenation and `toString`.
fn text(v: &Val) -> Result<String, ExecError> {
    Ok(match v {
        Val::Str(s) => s.clone(),
        Val::Int(i) => i.to_string(),
        Val::Float(f) => float_text(*f),
        Val::Bool(b) => b.to_string(),
        other => return Err(type_error("string conversion", other)),
    })
}

fn float_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

fn index_value(base: Val, index: Val, ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    Ok(match (base, index) {
        (Val::Null, _) | (_, Val::Null) => Val::Null,
        (Val::List(items), Val::Int(i)) => {
            let len = items.len() as i64;
            let at = if i < 0 { len + i } else { i };
            if (0..len).contains(&at) {
                items.into_iter().nth(at as usize).unwrap_or(Val::Null)
            } else {
                Val::Null
            }
        }
        (base @ (Val::Map(_) | Val::Node(_) | Val::Rel(_)), Val::Str(key)) => {
            property(base, &key, ctx)?
        }
        (base, index) => {
            return Err(semantic(format!(
                "cannot index {} with {}",
                base.type_name(),
                index.type_name()
            )))
        }
    })
}

fn slice(base: Val, from: Option<Val>, to: Option<Val>) -> Result<Val, ExecError> {
    let items = match base {
        Val::Null => return Ok(Val::Null),
        Val::List(items) => items,
        other => return Err(type_error("slicing", &other)),
    };
    let len = items.len() as i64;
    let bound = |v: Option<Val>, default: i64| -> Result<Option<i64>, ExecError> {
        match v {
            None => Ok(Some(default)),
            Some(Val::Null) => Ok(None),
            Some(Val::Int(i)) => Ok(Some(if i < 0 { (len + i).max(0) } else { i.min(len) })),
            Some(other) => Err(type_error("a slice bound", &other)),
        }
    };
    let (Some(from), Some(to)) = (bound(from, 0)?, bound(to, len)?) else {
        return Ok(Val::Null);
    };
    Ok(Val::List(if from >= to {
        vec![]
    } else {
        items[from as usize..to as usize].to_vec()
    }))
}

fn int_arg(v: &Val, what: &str) -> Result<Option<i64>, ExecError> {
    match v {
        Val::Null => Ok(None),
        Val::Int(i) => Ok(Some(*i)),
        Val::Float(f) if f.fract() == 0.0 => Ok(Some(*f as i64)),
        other => Err(type_error(what, other)),
    }
}

fn str_arg<'v>(v: &'v Val, what: &str) -> Result<Option<&'v str>, ExecError> {
    match v {
        Val::Null => Ok(None),
        Val::Str(s) => Ok(Some(s)),
        other => Err(type_error(what, other)),
    }
}

fn arity(name: &str, args: &[Val], min: usize, max: usize) -> Result<(), ExecError> {
    if args.len() < min || args.len() > max {
        return Err(semantic(format!("wrong number of arguments for {name}")));
    }
    Ok(())
}

fn call(name: &str, args: &[Expr], row: &[Val], ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    if crate::ast::AGGREGATES.contains(&name) {
        return Err(ExecError::Unsupported(
            "aggregates are only supported in WITH and RETURN items".into(),
        ));
    }
    if name == "coalesce" {
        for argument in args {
            let value = eval(argument, row, ctx)?;
            if !value.is_null() {
                return Ok(value);
            }
        }
        return Ok(Val::Null);
    }
    let values = args
        .iter()
        .map(|e| eval(e, row, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    let result = function(name, values, ctx)?;
    ctx.budget.charge(result.bytes())?;
    Ok(result)
}

fn function(name: &str, mut args: Vec<Val>, ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    let first = || args.first().cloned().unwrap_or(Val::Null);
    let one = |args: &[Val]| arity(name, args, 1, 1);
    Ok(match name {
        "id" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Node(id) => Val::Int(id.0 as i64),
                Val::Rel(rel) => Val::Int(rel.legacy_id()),
                other => return Err(type_error("id", &other)),
            }
        }
        "elementid" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Node(id) => Val::Str(id.element_id()),
                Val::Rel(rel) => Val::Str(rel.element_id()),
                other => return Err(type_error("elementId", &other)),
            }
        }
        "labels" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Node(id) => Val::List(vec![Val::Str(graph::label_of(ctx, id)?.to_string())]),
                other => return Err(type_error("labels", &other)),
            }
        }
        "type" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Rel(rel) => Val::Str(graph::type_of(ctx, &rel)?.to_string()),
                other => return Err(type_error("type", &other)),
            }
        }
        "properties" => {
            one(&args)?;
            properties(&first(), ctx)?
        }
        "keys" => {
            one(&args)?;
            match properties(&first(), ctx)? {
                Val::Map(map) => Val::List(map.into_keys().map(Val::Str).collect()),
                _ => Val::Null,
            }
        }
        "startnode" | "endnode" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Rel(rel) => Val::Node(if name == "startnode" {
                    rel.source
                } else {
                    rel.target
                }),
                other => return Err(type_error(name, &other)),
            }
        }
        "nodes" | "relationships" | "length" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Path(path) => match name {
                    "nodes" => Val::List(path.nodes.iter().copied().map(Val::Node).collect()),
                    "relationships" => Val::List(path.rels.iter().copied().map(Val::Rel).collect()),
                    _ => Val::Int(path.rels.len() as i64),
                },
                other => return Err(type_error(name, &other)),
            }
        }
        "size" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::List(items) => Val::Int(items.len() as i64),
                Val::Str(s) => Val::Int(s.chars().count() as i64),
                other => return Err(type_error("size", &other)),
            }
        }
        "isempty" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::List(items) => Val::Bool(items.is_empty()),
                Val::Str(s) => Val::Bool(s.is_empty()),
                Val::Map(m) => Val::Bool(m.is_empty()),
                other => return Err(type_error("isEmpty", &other)),
            }
        }
        "exists" => {
            one(&args)?;
            Val::Bool(!first().is_null())
        }
        "tolower" | "toupper" | "lower" | "upper" => {
            one(&args)?;
            let Some(text) = str_arg(&args[0], name)? else {
                return Ok(Val::Null);
            };
            // Unicode maps at most three scalars (12 bytes); allow String capacity growth too.
            let allocation = text
                .len()
                .checked_mul(24)
                .ok_or_else(|| semantic("case conversion size overflow"))?;
            ctx.budget.charge(allocation)?;
            Val::Str(if matches!(name, "tolower" | "lower") {
                text.to_lowercase()
            } else {
                text.to_uppercase()
            })
        }
        "tostring" | "tostringornull" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                v @ (Val::Str(_) | Val::Int(_) | Val::Float(_) | Val::Bool(_)) => {
                    Val::Str(text(&v)?)
                }
                other if name == "tostringornull" => {
                    let _ = other;
                    Val::Null
                }
                other => return Err(type_error("toString", &other)),
            }
        }
        "tointeger" | "tointegerornull" => {
            one(&args)?;
            match first() {
                Val::Int(i) => Val::Int(i),
                Val::Float(f) if f.is_finite() && f.abs() < 9.2e18 => Val::Int(f.trunc() as i64),
                Val::Bool(b) => Val::Int(i64::from(b)),
                Val::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .or_else(|| {
                        s.trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|f| f.is_finite() && f.abs() < 9.2e18)
                            .map(|f| f.trunc() as i64)
                    })
                    .map(Val::Int)
                    .unwrap_or(Val::Null),
                Val::Null => Val::Null,
                _ if name == "tointegerornull" => Val::Null,
                other => return Err(type_error("toInteger", &other)),
            }
        }
        "tofloat" | "tofloatornull" => {
            one(&args)?;
            match first() {
                Val::Int(i) => Val::Float(i as f64),
                Val::Float(f) => Val::Float(f),
                Val::Str(s) => s.trim().parse::<f64>().map(Val::Float).unwrap_or(Val::Null),
                Val::Null => Val::Null,
                _ if name == "tofloatornull" => Val::Null,
                other => return Err(type_error("toFloat", &other)),
            }
        }
        "toboolean" => {
            one(&args)?;
            match first() {
                Val::Bool(b) => Val::Bool(b),
                Val::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" => Val::Bool(true),
                    "false" => Val::Bool(false),
                    _ => Val::Null,
                },
                Val::Int(i) => Val::Bool(i != 0),
                Val::Null => Val::Null,
                other => return Err(type_error("toBoolean", &other)),
            }
        }
        "trim" | "ltrim" | "rtrim" => {
            one(&args)?;
            let Some(s) = str_arg(&args[0], name)? else {
                return Ok(Val::Null);
            };
            Val::Str(
                match name {
                    "trim" => s.trim(),
                    "ltrim" => s.trim_start(),
                    _ => s.trim_end(),
                }
                .to_string(),
            )
        }
        "replace" => {
            arity(name, &args, 3, 3)?;
            match (
                str_arg(&args[0], name)?,
                str_arg(&args[1], name)?,
                str_arg(&args[2], name)?,
            ) {
                (Some(s), Some(from), Some(to)) => {
                    ctx.budget.charge(s.len().saturating_mul(to.len().max(1)))?;
                    Val::Str(s.replace(from, to))
                }
                _ => Val::Null,
            }
        }
        "substring" => {
            arity(name, &args, 2, 3)?;
            let Some(s) = str_arg(&args[0], name)? else {
                return Ok(Val::Null);
            };
            let Some(start) = int_arg(&args[1], name)? else {
                return Ok(Val::Null);
            };
            let length = match args.get(2) {
                Some(v) => match int_arg(v, name)? {
                    Some(n) => Some(n),
                    None => return Ok(Val::Null),
                },
                None => None,
            };
            if start < 0 || length.is_some_and(|n| n < 0) {
                return Err(semantic("substring bounds must be nonnegative"));
            }
            let chars = s.chars().skip(start as usize);
            Val::Str(match length {
                Some(n) => chars.take(n as usize).collect(),
                None => chars.collect(),
            })
        }
        "left" | "right" => {
            arity(name, &args, 2, 2)?;
            let (Some(s), Some(n)) = (str_arg(&args[0], name)?, int_arg(&args[1], name)?) else {
                return Ok(Val::Null);
            };
            if n < 0 {
                return Err(semantic(format!("{name} length must be nonnegative")));
            }
            let count = s.chars().count();
            let n = (n as usize).min(count);
            Val::Str(if name == "left" {
                s.chars().take(n).collect()
            } else {
                s.chars().skip(count - n).collect()
            })
        }
        "split" => {
            arity(name, &args, 2, 2)?;
            match (str_arg(&args[0], name)?, str_arg(&args[1], name)?) {
                (Some(s), Some(sep)) => Val::List(if sep.is_empty() {
                    s.chars().map(|c| Val::Str(c.to_string())).collect()
                } else {
                    s.split(sep).map(|p| Val::Str(p.to_string())).collect()
                }),
                _ => Val::Null,
            }
        }
        "reverse" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::Str(s) => Val::Str(s.chars().rev().collect()),
                Val::List(mut items) => {
                    items.reverse();
                    Val::List(items)
                }
                other => return Err(type_error("reverse", &other)),
            }
        }
        "abs" | "ceil" | "floor" | "round" | "sqrt" | "sign" | "exp" | "log" | "log10" => {
            arity(name, &args, 1, 2)?;
            match (name, first()) {
                (_, Val::Null) => Val::Null,
                ("abs", Val::Int(i)) => Val::Int(
                    i.checked_abs()
                        .ok_or_else(|| semantic("integer overflow"))?,
                ),
                ("sign", Val::Int(i)) => Val::Int(i.signum()),
                (_, v) => {
                    let Some(x) = float(&v) else {
                        return Err(type_error(name, &v));
                    };
                    match name {
                        "abs" => Val::Float(x.abs()),
                        "ceil" => Val::Float(x.ceil()),
                        "floor" => Val::Float(x.floor()),
                        "round" => {
                            match args.get(1).map(|p| int_arg(p, name)).transpose()?.flatten() {
                                Some(precision) => {
                                    let scale = 10f64.powi(precision as i32);
                                    Val::Float((x * scale).round() / scale)
                                }
                                None => Val::Float(x.round()),
                            }
                        }
                        "sqrt" => Val::Float(x.sqrt()),
                        "sign" => Val::Int(if x > 0.0 {
                            1
                        } else if x < 0.0 {
                            -1
                        } else {
                            0
                        }),
                        "exp" => Val::Float(x.exp()),
                        "log" => Val::Float(x.ln()),
                        _ => Val::Float(x.log10()),
                    }
                }
            }
        }
        "head" | "last" | "tail" => {
            one(&args)?;
            match first() {
                Val::Null => Val::Null,
                Val::List(mut items) => match name {
                    "head" => items.into_iter().next().unwrap_or(Val::Null),
                    "last" => items.pop().unwrap_or(Val::Null),
                    _ => Val::List(items.into_iter().skip(1).collect()),
                },
                other => return Err(type_error(name, &other)),
            }
        }
        "range" => {
            arity(name, &args, 2, 3)?;
            let step = match args.get(2) {
                Some(v) => {
                    int_arg(v, name)?.ok_or_else(|| semantic("range step cannot be null"))?
                }
                None => 1,
            };
            let (Some(from), Some(to)) = (int_arg(&args[0], name)?, int_arg(&args[1], name)?)
            else {
                return Err(semantic("range bounds cannot be null"));
            };
            if step == 0 {
                return Err(semantic("range step cannot be zero"));
            }
            let count = if (step > 0 && from > to) || (step < 0 && from < to) {
                0
            } else {
                ((to as i128 - from as i128) / step as i128 + 1) as u128
            };
            ctx.budget.charge(
                usize::try_from(count)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(std::mem::size_of::<Val>()),
            )?;
            let mut out = Vec::with_capacity(count as usize);
            let mut current = from as i128;
            for _ in 0..count {
                out.push(Val::Int(current as i64));
                current += step as i128;
            }
            Val::List(out)
        }
        "pi" => Val::Float(std::f64::consts::PI),
        "e" => Val::Float(std::f64::consts::E),
        "rand" => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            let mut x = u64::from(nanos) ^ 0x9E37_79B9_7F4A_7C15;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            Val::Float((x >> 11) as f64 / (1u64 << 53) as f64)
        }
        "timestamp" => {
            arity(name, &args, 0, 0)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| semantic("clock before the epoch"))?;
            Val::Int(now.as_millis() as i64)
        }
        "valuetype" => {
            one(&args)?;
            Val::Str(
                match first() {
                    Val::Null => "NULL",
                    Val::Bool(_) => "BOOLEAN NOT NULL",
                    Val::Int(_) => "INTEGER NOT NULL",
                    Val::Float(_) => "FLOAT NOT NULL",
                    Val::Str(_) => "STRING NOT NULL",
                    Val::List(_) => "LIST<ANY> NOT NULL",
                    Val::Map(_) => "MAP NOT NULL",
                    Val::Node(_) => "NODE NOT NULL",
                    Val::Rel(_) => "RELATIONSHIP NOT NULL",
                    Val::Path(_) => "PATH NOT NULL",
                }
                .into(),
            )
        }
        _ => {
            args.clear();
            return Err(ExecError::Unsupported(format!("function {name}")));
        }
    })
}

/// Assemble a path value from bound pattern slots (`rels` entries may be lists).
pub(super) fn path(nodes: &[NodeId], rels: &[Val]) -> Result<Val, ExecError> {
    let mut path = PathVal {
        nodes: vec![nodes[0]],
        rels: vec![],
    };
    for (index, rel) in rels.iter().enumerate() {
        let segment: Vec<Relationship> = match rel {
            Val::Rel(rel) => vec![*rel],
            Val::List(items) => items
                .iter()
                .map(|item| match item {
                    Val::Rel(rel) => Ok(*rel),
                    _ => Err(semantic("path segment is not a relationship")),
                })
                .collect::<Result<_, _>>()?,
            _ => return Ok(Val::Null),
        };
        let mut current = *path.nodes.last().expect("start node");
        for rel in segment {
            current = if rel.source == current {
                rel.target
            } else {
                rel.source
            };
            path.rels.push(rel);
            path.nodes.push(current);
        }
        if current != nodes[index + 1] {
            return Err(semantic("path does not connect its nodes"));
        }
    }
    Ok(Val::Path(Box::new(path)))
}

pub(super) fn order_rows<T>(rows: &mut [(Vec<Val>, T)], descending: &[bool]) {
    rows.sort_by(|a, b| keys_cmp(&a.0, &b.0, descending));
}

pub(super) fn keys_cmp(a: &[Val], b: &[Val], descending: &[bool]) -> Ordering {
    for ((x, y), desc) in a.iter().zip(b).zip(descending) {
        let ord = val::sort_cmp(x, y);
        let ord = if *desc { ord.reverse() } else { ord };
        if ord.is_ne() {
            return ord;
        }
    }
    Ordering::Equal
}

/// A nonnegative integer for SKIP and LIMIT.
pub(super) fn limit(expr: &Expr, ctx: &Context<'_, '_>) -> Result<usize, ExecError> {
    match constant(expr, ctx)? {
        Value::Int(n) if n >= 0 => {
            usize::try_from(n).map_err(|_| semantic("LIMIT exceeds platform capacity"))
        }
        _ => Err(semantic("SKIP and LIMIT need a nonnegative integer")),
    }
}
