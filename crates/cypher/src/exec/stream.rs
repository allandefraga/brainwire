//! Row-at-a-time read pipelines. Writes, grouped aggregates and CALL retain the
//! eager executor; Bolt still buffers the final result until PULL/DISCARD.
use super::*;

enum WalkError {
    Done,
    Query(ExecError),
}
impl From<ExecError> for WalkError {
    fn from(value: ExecError) -> Self {
        Self::Query(value)
    }
}
impl From<GraphError> for WalkError {
    fn from(value: GraphError) -> Self {
        Self::Query(value.into())
    }
}
impl From<StoreError> for WalkError {
    fn from(value: StoreError) -> Self {
        Self::Query(value.into())
    }
}
type Emit<'a> = dyn FnMut(Bindings) -> Result<(), WalkError> + 'a;

fn aggregate(expr: &Expr) -> bool {
    matches!(expr, Expr::Count(_) | Expr::CountStar)
}

pub(super) fn supports(query: &Query) -> bool {
    let Some(Clause::Return { items, .. }) = query.clauses.last() else {
        return false;
    };
    // ponytail: bounded recursive continuations, not a general operator VM.
    // Very deep plans and grouped aggregates keep the eager path for now.
    let depth: usize = query
        .clauses
        .iter()
        .map(|c| match c {
            Clause::Match { patterns, .. } => patterns.len() + 1,
            _ => 1,
        })
        .sum();
    depth <= 64
        && query.clauses.iter().all(|c| {
            matches!(
                c,
                Clause::Match { .. } | Clause::Unwind { .. } | Clause::Return { .. }
            )
        })
        && (items.iter().all(|i| aggregate(&i.expr)) || items.iter().all(|i| !aggregate(&i.expr)))
}

pub(super) fn run(ctx: &Context<'_, '_>, query: &Query) -> Result<QueryOutput, ExecError> {
    let Clause::Return {
        items,
        order,
        skip,
        limit,
    } = query.clauses.last().expect("return")
    else {
        unreachable!()
    };
    let max = limit
        .as_ref()
        .map(|e| eval::limit(e, ctx))
        .transpose()?
        .unwrap_or(usize::MAX);
    let skip = skip
        .as_ref()
        .map(|e| eval::limit(e, ctx))
        .transpose()?
        .unwrap_or(0);
    // Rows kept while walking: all of them to sort, else the first SKIP + LIMIT. A
    // sorted top-k keeps twice that and trims, so a page of a large label stays small.
    let keep = skip.saturating_add(max);
    let mut output = QueryOutput {
        fields: items
            .iter()
            .map(|i| i.alias.clone().unwrap_or_else(|| i.text.clone()))
            .collect(),
        rows: vec![],
    };
    if max == 0 {
        return Ok(output);
    }
    let counts_only = items.iter().all(|i| aggregate(&i.expr));
    let mut counts = vec![0i64; if counts_only { items.len() } else { 0 }];
    ctx.budget.retain(counts.len().saturating_mul(8))?;
    let result = walk(
        ctx,
        &query.clauses[..query.clauses.len() - 1],
        Bindings::new(),
        &mut |row| {
            let _projection_scope = ctx.budget.scope();
            if counts_only {
                for (count, item) in counts.iter_mut().zip(items) {
                    let increment = match &item.expr {
                        Expr::CountStar => true,
                        Expr::Count(expr) => eval(expr, &row, ctx)? != Binding::Value(Value::Null),
                        _ => unreachable!(),
                    };
                    if increment {
                        *count = count
                            .checked_add(1)
                            .ok_or_else(|| ExecError::Semantic("count overflow".into()))?;
                    }
                }
            } else {
                output.rows.push(eval::project_row(ctx, &row, items)?);
                if order.is_empty() {
                    if output.rows.len() == keep {
                        return Err(WalkError::Done);
                    }
                } else if keep != usize::MAX && output.rows.len() >= keep.saturating_mul(2).max(64)
                {
                    let rows = std::mem::take(&mut output.rows);
                    output.rows = eval::page(ctx, rows, order, 0, keep);
                }
            }
            Ok(())
        },
    );
    match result {
        Ok(()) | Err(WalkError::Done) => {}
        Err(WalkError::Query(error)) => return Err(error),
    }
    if counts_only {
        let row: Vec<_> = counts
            .into_iter()
            .map(|n| Out::Value(Value::Int(n)))
            .collect();
        ctx.budget.output(&row)?;
        output.rows.push(row);
    }
    let rows = std::mem::take(&mut output.rows);
    output.rows = eval::page(ctx, rows, order, skip, max);
    ctx.budget.check_deadline()?;
    Ok(output)
}

fn walk(
    ctx: &Context<'_, '_>,
    clauses: &[Clause],
    row: Bindings,
    emit: &mut Emit<'_>,
) -> Result<(), WalkError> {
    let Some((clause, rest)) = clauses.split_first() else {
        return emit(row);
    };
    match clause {
        Clause::Unwind { expr, var } => {
            let _list_scope = ctx.budget.scope();
            match scalar(eval(expr, &row, ctx)?)? {
                Value::Null => {}
                Value::List(values) => {
                    for value in values {
                        let _row_scope = ctx.budget.scope();
                        let mut next = row.clone();
                        next.insert(var.clone(), Binding::Value(value));
                        ctx.budget.bindings(&next)?;
                        walk(ctx, rest, next, emit)?;
                    }
                }
                _ => return Err(ExecError::Semantic("UNWIND needs a list or null".into()).into()),
            }
        }
        Clause::Match { patterns, where_ } => {
            match_patterns(ctx, patterns, where_.as_ref(), row, &mut |row| {
                if let Some(predicate) = where_ {
                    if eval::boolean(eval(predicate, &row, ctx)?)? != Some(true) {
                        return Ok(());
                    }
                }
                walk(ctx, rest, row, emit)
            })?;
        }
        _ => unreachable!("streaming plan validated"),
    }
    Ok(())
}

fn match_patterns(
    ctx: &Context<'_, '_>,
    patterns: &[Pattern],
    predicate: Option<&Expr>,
    row: Bindings,
    emit: &mut Emit<'_>,
) -> Result<(), WalkError> {
    let Some((pattern, rest)) = patterns.split_first() else {
        return emit(row);
    };
    graph::visit_pattern(ctx, &row, pattern, predicate, &mut |next| {
        match_patterns(ctx, rest, predicate, next, emit)
    })
}
