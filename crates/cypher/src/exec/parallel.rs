//! Parallel label scans for aggregating and top-k projections: the anchor
//! label's id range is split across worker threads, each with its own query
//! guard share and accumulator, and the partial results are merged in scan
//! order on the calling thread. A visibility-restricted transaction filters
//! each worker's share with its policy, as the sequential scan would.
use super::plan::*;
use super::run::{stream, Accumulator, Flow, Row};
use super::*;
use brainwire_store::graph as store;

/// Labels smaller than this scan on the calling thread.
const MIN_NODES: u64 = 50_000;
const MAX_WORKERS: usize = 8;

pub(super) fn project(
    ctx: &Context<'_, '_>,
    segment: &[Op],
    projection: &ProjectPlan,
    input: &[Row],
) -> Result<Option<Vec<Row>>, ExecError> {
    if input.len() != 1 || projection.distinct || projection.presorted {
        return Ok(None);
    }
    match &projection.aggregate {
        Some(agg) if agg.aggs.iter().any(|a| a.distinct) => return Ok(None),
        Some(_) => {}
        None if projection.order.is_empty() || projection.limit.is_none() => return Ok(None),
        None => {}
    }
    let Some(partials) = split(ctx, segment, input, |worker, segment, row| {
        let mut acc = Accumulator::new(projection, worker)?;
        match stream(worker, segment, row, &mut |row| acc.push(worker, row)) {
            Ok(()) | Err(Flow::Stop) => Ok(acc),
            Err(Flow::Error(error)) => Err(error),
        }
    })?
    else {
        return Ok(None);
    };
    let mut merged = Accumulator::new(projection, ctx)?;
    for partial in partials {
        merged.merge(ctx, partial?)?;
    }
    merged.finish(ctx).map(Some)
}

/// A final streaming segment (no blocking projection): each worker collects
/// its rows and the chunks are concatenated in id order, exactly the order
/// of a sequential scan.
pub(super) fn collect(
    ctx: &Context<'_, '_>,
    segment: &[Op],
    input: &[Row],
) -> Result<Option<Vec<Row>>, ExecError> {
    let Some(partials) = split(ctx, segment, input, |worker, segment, row| {
        let mut rows = vec![];
        match stream(worker, segment, row, &mut |row| {
            worker.budget.retain_row(&row)?;
            rows.push(row);
            Ok(())
        }) {
            Ok(()) | Err(Flow::Stop) => Ok(rows),
            Err(Flow::Error(error)) => Err(error),
        }
    })?
    else {
        return Ok(None);
    };
    let mut rows = vec![];
    for partial in partials {
        for row in partial? {
            ctx.budget.retain_row(&row)?;
            rows.push(row);
        }
    }
    Ok(Some(rows))
}

/// Run `work` on every chunk of the segment's anchor label scan, or return
/// `None` when the segment does not start with a large label scan.
fn split<'p, T: Send>(
    ctx: &Context<'_, '_>,
    segment: &'p [Op],
    input: &[Row],
    work: impl Fn(&Context<'_, '_>, &'p [Op], Row) -> Result<T, ExecError> + Sync,
) -> Result<Option<Vec<Result<T, ExecError>>>, ExecError> {
    if input.len() != 1 {
        return Ok(None);
    }
    let Some(Op::Match(matched)) = segment.first() else {
        return Ok(None);
    };
    let Some(StepKind::Anchor(anchor)) = matched.steps.first().map(|s| &s.kind) else {
        return Ok(None);
    };
    if matched.optional
        || anchor.bound
        || !matches!(anchor.access, Access::Scan)
        || anchor.labels.len() != 1
    {
        return Ok(None);
    }
    let ty = ctx
        .catalog
        .node_type(&anchor.labels[0])
        .expect("validated label");
    let bound = store::node_seq_bound(ctx.txn, ty.id)?;
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS);
    if bound < MIN_NODES || workers < 2 {
        return Ok(None);
    }
    let (lo, inclusive_hi) = NodeId::type_range(0, ty.id).map_err(GraphError::from)?;
    let chunk = bound.div_ceil(workers as u64);
    let ranges: Vec<(NodeId, NodeId)> = (0..workers as u64)
        .map(|i| {
            let start = NodeId(lo.0 + i * chunk);
            let end = if i + 1 == workers as u64 {
                NodeId(inclusive_hi.0 + 1)
            } else {
                NodeId(lo.0 + (i + 1) * chunk)
            };
            (start, end)
        })
        .collect();
    let budgets: Vec<Budget<'static>> = ranges.iter().map(|_| ctx.budget.fork(workers)).collect();
    let (txn, catalog, params, vars, param_vals, checkpoint_dir, visibility) = (
        ctx.txn,
        ctx.catalog,
        ctx.params,
        ctx.vars,
        ctx.param_vals,
        ctx.checkpoint_dir,
        ctx.visibility,
    );
    let slot = anchor.slot;
    let row = &input[0];
    let work = &work;
    let partials: Vec<Result<T, ExecError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = ranges
            .into_iter()
            .zip(budgets)
            .map(|((start, end), budget)| {
                scope.spawn(move || {
                    let cache = NodeCache::default();
                    let worker = Context {
                        txn,
                        catalog,
                        params,
                        budget: &budget,
                        // Each worker filters its share with the same policy.
                        visibility,
                        checkpoint_dir,
                        vars,
                        cache: Some(&cache),
                        param_vals,
                        partition: Some((slot, start, end)),
                    };
                    work(&worker, segment, row.clone())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("scan worker panicked"))
            .collect()
    });
    Ok(Some(partials))
}
