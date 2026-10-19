//! Resolve names and unsupported shapes before any row can disappear.
use super::*;
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Node,
    Relationship,
    Value,
}
type Scope = BTreeMap<String, Kind>;
fn semantic(s: impl Into<String>) -> ExecError {
    ExecError::Semantic(s.into())
}
fn unsupported(s: impl Into<String>) -> ExecError {
    ExecError::Unsupported(s.into())
}
fn children(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Prop(e, _)
        | Expr::Count(e)
        | Expr::Size(e)
        | Expr::ToLower(e)
        | Expr::ToUpper(e)
        | Expr::Not(e) => vec![e],
        Expr::Cmp(a, _, b) | Expr::And(a, b) | Expr::Or(a, b) => vec![a, b],
        Expr::List(es) | Expr::Coalesce(es) => es.iter().collect(),
        Expr::Map(es) => es.iter().map(|(_, e)| e).collect(),
        _ => vec![],
    }
}
pub(super) fn mentions(e: &Expr, var: Option<&str>) -> bool {
    matches!(e,Expr::Var(v) if var.is_none_or(|s| s == v))
        || children(e).into_iter().any(|e| mentions(e, var))
}
fn expr(e: &Expr, scope: &Scope, ctx: &Context<'_, '_>, count: bool) -> Result<(), ExecError> {
    expr_checked(e, scope, ctx, count, true)
}
fn expr_checked(
    e: &Expr,
    scope: &Scope,
    ctx: &Context<'_, '_>,
    count: bool,
    evaluate: bool,
) -> Result<(), ExecError> {
    match e {
        Expr::Prop(base, _) if matches!(base.as_ref(), Expr::Var(var) if scope.get(var) == Some(&Kind::Relationship)) =>
        {
            return Err(unsupported("relationship property reads are not supported"));
        }
        Expr::Var(v) if !scope.contains_key(v) => {
            return Err(semantic(format!("unknown variable {v}")))
        }
        Expr::Param(p) if evaluate && !ctx.params.contains_key(p) => {
            return Err(ExecError::ParameterMissing(p.clone()))
        }
        Expr::Count(_) | Expr::CountStar if !count => {
            return Err(unsupported("count must be a top-level RETURN item"))
        }
        _ => {}
    }
    for child in children(e) {
        // Coalesce validates every name/shape, but runtime checks follow its lazy choice.
        expr_checked(
            child,
            scope,
            ctx,
            false,
            evaluate && !matches!(e, Expr::Coalesce(_)),
        )?;
    }
    if !evaluate {
        return Ok(());
    }
    match e {
        Expr::And(a, b) | Expr::Or(a, b) => {
            for operand in [a, b] {
                if !mentions(operand, None) {
                    eval::boolean(eval(operand, &Bindings::new(), ctx)?)?;
                }
            }
        }
        Expr::Not(operand) if !mentions(operand, None) => {
            eval::boolean(eval(operand, &Bindings::new(), ctx)?)?;
        }
        _ => {}
    }
    // Check row-independent subexpressions even when there are no input rows.
    if !mentions(e, None) && !matches!(e, Expr::Count(_) | Expr::CountStar) {
        eval(e, &Bindings::new(), ctx)?;
    }
    Ok(())
}
fn label(p: &NodePat, ctx: &Context<'_, '_>, required: bool) -> Result<(), ExecError> {
    if let Some(label) = &p.label {
        if ctx.catalog.node_type(label).is_none() {
            return Err(semantic(format!("unknown label {label}")));
        }
    } else if required {
        return Err(unsupported("MATCH needs a label"));
    }
    Ok(())
}
fn properties(p: &[(String, Expr)], scope: &Scope, ctx: &Context<'_, '_>) -> Result<(), ExecError> {
    let mut names = BTreeSet::new();
    for (name, e) in p {
        if !names.insert(name) {
            return Err(semantic(format!("duplicate property {name}")));
        }
        expr(e, scope, ctx, false)?;
    }
    Ok(())
}
fn node_var(p: &NodePat, scope: &mut Scope) -> Result<(), ExecError> {
    if let Some(v) = &p.var {
        if scope.get(v).is_some_and(|kind| *kind != Kind::Node) {
            return Err(semantic(format!("{v} is not a node")));
        }
        scope.insert(v.clone(), Kind::Node);
    }
    Ok(())
}
fn bound(v: &str, scope: &Scope) -> Result<(), ExecError> {
    if scope.get(v) != Some(&Kind::Node) {
        return Err(semantic(format!("{v} is not a bound node")));
    }
    Ok(())
}
fn pattern(
    p: &Pattern,
    scope: &mut Scope,
    ctx: &Context<'_, '_>,
    write: bool,
) -> Result<(), ExecError> {
    label(&p.start, ctx, !write)?;
    if let Some((rel, end)) = &p.hop {
        label(end, ctx, !write)?;
        if ctx.catalog.edge_type(&rel.rel_type).is_none() {
            return Err(semantic(format!("unknown edge type {}", rel.rel_type)));
        }
        if write {
            if rel.var.is_some() {
                return Err(unsupported(
                    "relationship variables in writes are not supported",
                ));
            }
            for endpoint in [&p.start, end] {
                if endpoint.label.is_some() || !endpoint.props.is_empty() {
                    return Err(unsupported("edge writes need bare bound node endpoints"));
                }
                bound(
                    endpoint
                        .var
                        .as_deref()
                        .ok_or_else(|| unsupported("edge writes need bound endpoints"))?,
                    scope,
                )?;
            }
            properties(&rel.props, scope, ctx)?;
        } else {
            if !rel.props.is_empty() {
                return Err(unsupported(
                    "MATCH relationship property filters are not supported",
                ));
            }
            node_var(&p.start, scope)?;
            properties(&p.start.props, scope, ctx)?;
            node_var(end, scope)?;
            properties(&end.props, scope, ctx)?;
            if let Some(var) = &rel.var {
                if scope
                    .get(var)
                    .is_some_and(|kind| *kind != Kind::Relationship)
                {
                    return Err(semantic(format!("{var} is not a relationship")));
                }
                scope.insert(var.clone(), Kind::Relationship);
            }
        }
    } else if write {
        if p.start.label.is_none() || !p.start.props.iter().any(|(k, _)| k == "entity_id") {
            return Err(unsupported("node writes need a label and entity_id"));
        }
        if p.start.var.as_ref().is_some_and(|v| scope.contains_key(v)) {
            return Err(unsupported(
                "node writes cannot rebind an existing variable",
            ));
        }
        properties(&p.start.props, scope, ctx)?;
        node_var(&p.start, scope)?;
    } else {
        node_var(&p.start, scope)?;
        properties(&p.start.props, scope, ctx)?;
    }
    Ok(())
}
pub(super) fn query(query: &Query, ctx: &Context<'_, '_>) -> Result<(), ExecError> {
    if query
        .clauses
        .iter()
        .any(|c| matches!(c, Clause::Call { .. }))
        && !(query.clauses.len() == 2
            && matches!(query.clauses[0], Clause::Call { .. })
            && matches!(query.clauses[1], Clause::Return { .. }))
    {
        return Err(unsupported(
            "analytics currently requires CALL ... YIELD ... RETURN as the whole query",
        ));
    }
    let mut scope = Scope::new();
    for (idx, c) in query.clauses.iter().enumerate() {
        match c {
            Clause::Call {
                name,
                args,
                yields,
                where_,
            } => {
                for argument in args {
                    expr(argument, &scope, ctx, false)?;
                }
                procedures::validate(name, args, yields, ctx)?;
                for item in yields {
                    scope.insert(
                        item.alias.clone(),
                        if item.field == "node"
                            || (item.field == "component"
                                && matches!(name.as_str(), "graph.wcc" | "graph.scc"))
                        {
                            Kind::Node
                        } else {
                            Kind::Value
                        },
                    );
                }
                if let Some(predicate) = where_ {
                    expr(predicate, &scope, ctx, false)?;
                    if !mentions(predicate, None) {
                        eval::boolean(eval(predicate, &Bindings::new(), ctx)?)?;
                    }
                }
            }
            Clause::Match { patterns, where_ } => {
                for p in patterns {
                    pattern(p, &mut scope, ctx, false)?;
                }
                if let Some(e) = where_ {
                    expr(e, &scope, ctx, false)?;
                    if !mentions(e, None) {
                        eval::boolean(eval(e, &Bindings::new(), ctx)?)?;
                    }
                }
            }
            Clause::Create(patterns) => {
                for p in patterns {
                    pattern(p, &mut scope, ctx, true)?;
                }
            }
            Clause::Merge(p) => pattern(p, &mut scope, ctx, true)?,
            Clause::Set(items) => {
                for item in items {
                    bound(&item.var, &scope)?;
                    if item.prop == "entity_id" {
                        return Err(semantic("SET cannot change entity_id"));
                    }
                    expr(&item.value, &scope, ctx, false)?;
                }
            }
            Clause::Delete(vars) => {
                for var in vars {
                    if scope.get(var) != Some(&Kind::Relationship) {
                        return Err(semantic(format!(
                            "{var} is not a bound relationship; use DETACH DELETE for nodes"
                        )));
                    }
                }
            }
            Clause::DetachDelete(vars) => {
                for v in vars {
                    bound(v, &scope)?;
                }
            }
            Clause::Unwind { expr: e, var } => {
                expr(e, &scope, ctx, false)?;
                if scope.contains_key(var) {
                    return Err(semantic(format!("variable {var} is already bound")));
                }
                if !mentions(e, None) {
                    match scalar(eval(e, &Bindings::new(), ctx)?)? {
                        Value::List(_) | Value::Null => {}
                        _ => return Err(semantic("UNWIND needs a list or null")),
                    }
                }
                scope.insert(var.clone(), Kind::Value);
            }
            Clause::Return { items, limit } => {
                if idx + 1 != query.clauses.len() {
                    return Err(unsupported("RETURN must be final"));
                }
                for item in items {
                    if matches!(&item.expr, Expr::Var(var) if scope.get(var) == Some(&Kind::Relationship))
                    {
                        return Err(unsupported("RETURN relationship values is not supported"));
                    }
                    expr(&item.expr, &scope, ctx, true)?;
                }
                if let Some(e) = limit {
                    expr(e, &scope, ctx, false)?;
                    if mentions(e, None) {
                        return Err(semantic("LIMIT must be row-independent"));
                    }
                    eval::limit(e, ctx)?;
                }
            }
        }
    }
    Ok(())
}
