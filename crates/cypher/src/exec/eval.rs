use super::*;
use std::cmp::Ordering;

pub(super) fn scalar(binding: Binding) -> Result<Value, ExecError> {
    match binding {
        Binding::Value(v) => Ok(v),
        Binding::Node(_) | Binding::Relationship(_) => {
            Err(ExecError::Semantic("expected a scalar value".into()))
        }
    }
}
pub(super) fn boolean(binding: Binding) -> Result<Option<bool>, ExecError> {
    match binding {
        Binding::Value(Value::Null) => Ok(None),
        Binding::Value(Value::Bool(b)) => Ok(Some(b)),
        _ => Err(ExecError::Semantic("expected boolean or null".into())),
    }
}
fn truth(v: Option<bool>) -> Binding {
    Binding::Value(v.map(Value::Bool).unwrap_or(Value::Null))
}

pub(super) fn eval(
    expr: &Expr,
    row: &Bindings,
    ctx: &Context<'_, '_>,
) -> Result<Binding, ExecError> {
    let v = match expr {
        Expr::Literal(v) => {
            ctx.budget.charge(budget::value_bytes(v))?;
            v.clone()
        }
        Expr::Param(name) => ctx
            .params
            .get(name)
            .cloned()
            .ok_or_else(|| ExecError::ParameterMissing(name.clone()))?,
        Expr::Var(name) => {
            let binding = row
                .get(name)
                .ok_or_else(|| ExecError::Semantic(format!("unknown variable {name}")))?;
            if let Binding::Value(value) = binding {
                ctx.budget.charge(budget::value_bytes(value))?;
            }
            return Ok(binding.clone());
        }
        Expr::Prop(base, name) => match eval(base, row, ctx)? {
            Binding::Node(id) => graph::node_property(ctx, id, name)?,
            Binding::Value(Value::Map(map)) => map.get(name).cloned().unwrap_or(Value::Null),
            Binding::Value(Value::Null) => Value::Null,
            _ => {
                return Err(ExecError::Semantic(
                    "property access needs a node or map".into(),
                ))
            }
        },
        Expr::Coalesce(args) => {
            for argument in args {
                let value = eval(argument, row, ctx)?;
                if value != Binding::Value(Value::Null) {
                    return Ok(value);
                }
            }
            Value::Null
        }
        Expr::ToLower(inner) | Expr::ToUpper(inner) => {
            let text = match scalar(eval(inner, row, ctx)?)? {
                Value::Null => return Ok(Binding::Value(Value::Null)),
                Value::Str(text) => text,
                _ => {
                    return Err(ExecError::Semantic(
                        "case conversion expects a string or null".into(),
                    ))
                }
            };
            // Unicode maps at most three scalars (12 bytes); allow String capacity growth too.
            // shortcut: refine this conservative charge if measured normalization workloads hit it.
            let allocation = text
                .len()
                .checked_mul(24)
                .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Value>()))
                .ok_or_else(|| ExecError::Semantic("case conversion size overflow".into()))?;
            ctx.budget.charge(allocation)?;
            // Whole-string casing preserves contextual Greek final sigma; locale-independent.
            let converted = if matches!(expr, Expr::ToLower(_)) {
                text.to_lowercase()
            } else {
                text.to_uppercase()
            };
            ctx.budget.check_deadline()?;
            return Ok(Binding::Value(Value::Str(converted)));
        }
        Expr::Size(inner) => {
            let length = match scalar(eval(inner, row, ctx)?)? {
                Value::Null => return Ok(Binding::Value(Value::Null)),
                Value::List(items) => items.len(),
                Value::Str(text) => text.chars().count(),
                _ => {
                    return Err(ExecError::Semantic(
                        "size expects a list, string or null".into(),
                    ))
                }
            };
            Value::Int(
                i64::try_from(length).map_err(|_| ExecError::Semantic("size overflow".into()))?,
            )
        }
        Expr::List(items) => Value::List(
            items
                .iter()
                .map(|e| scalar(eval(e, row, ctx)?))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Map(items) => Value::Map(
            items
                .iter()
                .map(|(k, e)| Ok((k.clone(), scalar(eval(e, row, ctx)?)?)))
                .collect::<Result<_, ExecError>>()?,
        ),
        Expr::Cmp(a, op, b) => return Ok(compare(eval(a, row, ctx)?, op, eval(b, row, ctx)?)),
        Expr::And(a, b) | Expr::Or(a, b) => {
            // Evaluate both operands: invalid operand types must not be hidden by short circuiting.
            let a = boolean(eval(a, row, ctx)?)?;
            let b = boolean(eval(b, row, ctx)?)?;
            return Ok(truth(if matches!(expr, Expr::And(..)) {
                match (a, b) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            } else {
                match (a, b) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }
            }));
        }
        Expr::Not(a) => return Ok(truth(boolean(eval(a, row, ctx)?)?.map(|v| !v))),
        Expr::Count(_) | Expr::CountStar => {
            return Err(ExecError::Unsupported(
                "aggregate is only supported as a top-level RETURN item".into(),
            ))
        }
    };
    ctx.budget.charge(budget::value_bytes(&v))?;
    Ok(Binding::Value(v))
}

// Compare an integer with an IEEE float without rounding the integer to f64.
// The cast is safe only inside these explicit i64 bounds; Rust truncates toward zero.
fn int_float(i: i64, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f >= 9223372036854775808.0 {
        return Some(Ordering::Less);
    }
    if f < -9223372036854775808.0 {
        return Some(Ordering::Greater);
    }
    let truncated = f as i64;
    Some(i.cmp(&truncated).then_with(|| {
        if f.fract() > 0.0 {
            Ordering::Less
        } else if f.fract() < 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }))
}
fn order(a: &Binding, b: &Binding) -> Option<Ordering> {
    match (a, b) {
        (Binding::Node(a), Binding::Node(b)) => Some(a.cmp(b)),
        (Binding::Relationship(a), Binding::Relationship(b)) => Some(a.cmp(b)),
        (Binding::Value(a), Binding::Value(b)) => match (a, b) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Int(a), Value::Float(b)) => int_float(*a, *b),
            (Value::Float(a), Value::Int(b)) => int_float(*b, *a).map(Ordering::reverse),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            _ => None,
        },
        _ => None,
    }
}
fn compare(a: Binding, op: &CmpOp, b: Binding) -> Binding {
    if a == Binding::Value(Value::Null) || b == Binding::Value(Value::Null) {
        return truth(None);
    }
    let ord = order(&a, &b);
    truth(match op {
        CmpOp::Eq => Some(ord == Some(Ordering::Equal)),
        CmpOp::Ne => ord.map(|o| o != Ordering::Equal),
        CmpOp::Lt => ord.map(|o| o.is_lt()),
        CmpOp::Le => ord.map(|o| o.is_le()),
        CmpOp::Gt => ord.map(|o| o.is_gt()),
        CmpOp::Ge => ord.map(|o| o.is_ge()),
    })
}
pub(super) fn equal(a: &Binding, b: &Binding) -> bool {
    order(a, b) == Some(Ordering::Equal)
}
fn group_equal(a: &Binding, b: &Binding) -> bool {
    if equal(a, b) {
        return true;
    }
    match (a, b) {
        (Binding::Value(Value::Null), Binding::Value(Value::Null)) => true,
        (Binding::Value(Value::List(a)), Binding::Value(Value::List(b))) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(a, b)| {
                    group_equal(&Binding::Value(a.clone()), &Binding::Value(b.clone()))
                })
        }
        (Binding::Value(Value::Map(a)), Binding::Value(Value::Map(b))) => {
            a.len() == b.len()
                && a.iter().all(|(k, a)| {
                    b.get(k).is_some_and(|b| {
                        group_equal(&Binding::Value(a.clone()), &Binding::Value(b.clone()))
                    })
                })
        }
        _ => false,
    }
}
pub(super) fn out(v: Binding, ctx: &Context<'_, '_>) -> Result<Out, ExecError> {
    match v {
        Binding::Value(v) => Ok(Out::Value(v)),
        Binding::Node(id) => Ok(Out::Node(graph::node_out(ctx, id)?)),
        Binding::Relationship(_) => Err(ExecError::Unsupported(
            "RETURN relationship values is not supported".into(),
        )),
    }
}
pub(super) fn project_row(
    ctx: &Context<'_, '_>,
    row: &Bindings,
    items: &[ReturnItem],
) -> Result<Vec<Out>, ExecError> {
    let result = items
        .iter()
        .map(|i| out(eval(&i.expr, row, ctx)?, ctx))
        .collect::<Result<Vec<_>, _>>()?;
    ctx.budget.output(&result)?;
    Ok(result)
}
pub(super) fn limit(expr: &Expr, ctx: &Context<'_, '_>) -> Result<usize, ExecError> {
    match scalar(eval(expr, &Bindings::new(), ctx)?)? {
        Value::Int(n) if n >= 0 => usize::try_from(n)
            .map_err(|_| ExecError::Semantic("LIMIT exceeds platform capacity".into())),
        _ => Err(ExecError::Semantic(
            "LIMIT needs a nonnegative integer".into(),
        )),
    }
}
pub(super) fn project(
    ctx: &Context<'_, '_>,
    rows: &[Bindings],
    items: &[ReturnItem],
    limit_expr: Option<&Expr>,
) -> Result<QueryOutput, ExecError> {
    let max = limit_expr
        .map(|e| limit(e, ctx))
        .transpose()?
        .unwrap_or(usize::MAX);
    let fields = items
        .iter()
        .map(|item| item.alias.clone().unwrap_or_else(|| item.text.clone()))
        .collect();
    let mut output = vec![];
    let aggregate = |e: &Expr| matches!(e, Expr::CountStar | Expr::Count(_));
    if !items.iter().any(|i| aggregate(&i.expr)) {
        for row in rows.iter().take(max) {
            output.push(project_row(ctx, row, items)?);
        }
    } else {
        // First-seen group order. Linear lookup is intentionally simple for this eager MVP.
        let mut groups: Vec<(Vec<Binding>, Vec<i64>)> = vec![];
        if rows.is_empty() && items.iter().all(|i| aggregate(&i.expr)) {
            groups.push((vec![], vec![0; items.len()]));
        }
        for row in rows {
            let key = items
                .iter()
                .filter(|i| !aggregate(&i.expr))
                .map(|i| eval(&i.expr, row, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            let idx = groups.iter().position(|(k, _)| {
                k.len() == key.len() && k.iter().zip(&key).all(|(a, b)| group_equal(a, b))
            });
            let idx = match idx {
                Some(idx) => idx,
                None => {
                    ctx.budget.charge(key.iter().fold(
                        items.len().saturating_mul(8) + 64,
                        |sum, b| {
                            sum.saturating_add(match b {
                                Binding::Node(_) => 16,
                                Binding::Relationship(_) => std::mem::size_of::<Relationship>(),
                                Binding::Value(v) => budget::value_bytes(v),
                            })
                        },
                    ))?;
                    groups.push((key, vec![0; items.len()]));
                    groups.len() - 1
                }
            };
            for (j, item) in items.iter().enumerate() {
                let add = match &item.expr {
                    Expr::CountStar => true,
                    Expr::Count(e) => eval(e, row, ctx)? != Binding::Value(Value::Null),
                    _ => false,
                };
                if add {
                    groups[idx].1[j] = groups[idx].1[j]
                        .checked_add(1)
                        .ok_or_else(|| ExecError::Semantic("count overflow".into()))?;
                }
            }
        }
        for (key, counts) in groups.into_iter().take(max) {
            let mut key = key.into_iter();
            let mut result = vec![];
            for (j, item) in items.iter().enumerate() {
                result.push(if aggregate(&item.expr) {
                    Out::Value(Value::Int(counts[j]))
                } else {
                    out(key.next().expect("group key per nonaggregate item"), ctx)?
                });
            }
            ctx.budget.output(&result)?;
            output.push(result);
        }
    }
    output.truncate(max);
    Ok(QueryOutput {
        fields,
        rows: output,
    })
}
