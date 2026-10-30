//! Parallel label scans for aggregating and top-k projections: the anchor
//! label's id range is split across worker threads, each with its own query
//! guard share and accumulator, and the partial results are merged in scan
//! order on the calling thread. Visibility-restricted scans filter each worker.
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
    let Some(scan) = streaming_scan(ctx, segment)? else {
        return Ok(None);
    };
    let workers = scan.ranges.len();
    let ranges = scan.ranges;
    let slot = scan.slot;
    let mut reservations: Vec<_> = ranges
        .iter()
        .map(|_| ctx.budget.worker_reservation())
        .collect();
    let budgets: Vec<_> = reservations
        .iter()
        .map(|reservation| ctx.budget.fork(workers, reservation.as_ref()))
        .collect();
    let (txn, catalog, params, vars, param_vals, checkpoint_dir, visibility) = (
        ctx.txn,
        ctx.catalog,
        ctx.params,
        ctx.vars,
        ctx.param_vals,
        ctx.checkpoint_dir,
        ctx.visibility,
    );
    let row = &input[0];
    let work = &work;
    let partials: Vec<(Result<T, ExecError>, usize)> = std::thread::scope(|scope| {
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
                        visibility,
                        checkpoint_dir,
                        vars,
                        cache: Some(&cache),
                        param_vals,
                        partition: Some((slot, start, end)),
                    };
                    let result = budget
                        .clone_row(row)
                        .and_then(|row| work(&worker, segment, row));
                    (result, budget.high_water())
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("scan worker panicked"))
            .collect()
    });
    if let Err(error) = ctx.budget.absorb_worker_reservations(
        &mut reservations,
        partials.iter().map(|(_, high_water)| *high_water),
    ) {
        drop(partials);
        return Err(error);
    }
    Ok(Some(
        partials.into_iter().map(|(result, _)| result).collect(),
    ))
}

/// Immutable partitioning decided before any streaming output is emitted.
pub(super) struct StreamingScan {
    ranges: Vec<(NodeId, NodeId)>,
    slot: usize,
}
pub(super) fn streaming_scan(
    ctx: &Context<'_, '_>,
    segment: &[Op],
) -> Result<Option<StreamingScan>, ExecError> {
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
    Ok(Some(StreamingScan {
        ranges,
        slot: anchor.slot,
    }))
}

type OwnedRow = (Vec<Out>, usize, QueryOutputLease);

/// The coordinator owns no row budget: each worker's fixed share covers its
/// scratch state and all of its output leases, including rows already delivered.
pub(super) fn stream_leased(
    ctx: &Context<'_, '_>,
    part: &Part,
    scan: StreamingScan,
    cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    sink: &mut dyn FnMut(Vec<Out>, usize, QueryOutputLease) -> bool,
) -> Result<(), ExecError> {
    use std::sync::{atomic::Ordering, mpsc, Mutex};
    let workers = scan.ranges.len();
    let reservations: Vec<_> = scan
        .ranges
        .iter()
        .map(|_| ctx.budget.worker_reservation())
        .collect();
    let budgets: Vec<_> = reservations
        .iter()
        .map(|reservation| ctx.budget.fork(workers, reservation.as_ref()))
        .collect();
    let (txn, catalog, params, vars, param_vals, checkpoint_dir, visibility) = (
        ctx.txn,
        ctx.catalog,
        ctx.params,
        ctx.vars,
        ctx.param_vals,
        ctx.checkpoint_dir,
        ctx.visibility,
    );
    let outputs = super::run::output_slots(&part.outputs);
    let failure = Mutex::new(None);
    let initial = vec![val::Val::Null; vars.len()];
    std::thread::scope(|scope| {
        let mut receivers = Vec::with_capacity(workers);
        let mut handles = Vec::with_capacity(workers);
        for ((start, end), budget) in scan.ranges.into_iter().zip(budgets) {
            let (sender, receiver) = mpsc::sync_channel::<Vec<OwnedRow>>(1);
            receivers.push(receiver);
            let outputs = &outputs;
            let initial = &initial;
            let failure = &failure;
            let slot = scan.slot;
            handles.push(scope.spawn(move || {
                let cache = NodeCache::default();
                let worker = Context {
                    txn,
                    catalog,
                    params,
                    budget: &budget,
                    visibility,
                    checkpoint_dir,
                    vars,
                    cache: Some(&cache),
                    param_vals,
                    partition: Some((slot, start, end)),
                };
                let page_limit = (budget.remaining() / 32).clamp(1, 32 << 10);
                let mut page = Vec::new();
                let mut page_bytes = 0usize;
                let result = budget
                    .clone_row(initial)
                    .map_err(Flow::from)
                    .and_then(|row| {
                        stream(&worker, &part.ops, row, &mut |row| {
                            let _scope = budget.scope();
                            let row = super::run::project_output(row, outputs);
                            let out = row
                                .into_iter()
                                .map(|value| graph::out(&worker, value))
                                .collect::<Result<Vec<_>, _>>()?;
                            let bytes = Budget::output_bytes(&out);
                            budget.charge(bytes)?;
                            let lease = budget.output_lease(bytes)?;
                            page.push((out, bytes, lease));
                            page_bytes = page_bytes.saturating_add(bytes);
                            if page.len() >= 64 || page_bytes >= page_limit {
                                let waited = std::time::Instant::now();
                                let sent = sender.send(std::mem::take(&mut page)).is_ok();
                                budget.extend_deadline(waited.elapsed());
                                page_bytes = 0;
                                if !sent {
                                    return Err(Flow::Stop);
                                }
                            }
                            Ok(())
                        })
                    });
                match result {
                    Ok(()) => {
                        if !page.is_empty() && !cancellation.load(Ordering::Acquire) {
                            let waited = std::time::Instant::now();
                            let _ = sender.send(page);
                            budget.extend_deadline(waited.elapsed());
                        }
                    }
                    Err(Flow::Stop) => {}
                    Err(Flow::Error(error)) => {
                        if !cancellation.swap(true, Ordering::AcqRel) {
                            *failure.lock().expect("scan error mutex poisoned") = Some(error);
                        }
                    }
                }
            }));
        }
        'ranges: for receiver in &receivers {
            while let Ok(page) = receiver.recv() {
                if cancellation.load(Ordering::Acquire) {
                    break 'ranges;
                }
                for (row, bytes, lease) in page {
                    if !sink(row, bytes, lease) {
                        cancellation.store(true, Ordering::Release);
                        break 'ranges;
                    }
                }
            }
            if cancellation.load(Ordering::Acquire) {
                break;
            }
        }
        // Closing every queue unblocks a worker waiting in send before joining it.
        drop(receivers);
        for handle in handles {
            handle.join().expect("streaming scan worker panicked");
        }
    });
    let result = failure.into_inner().expect("scan error mutex poisoned");
    result.map_or(Ok(()), Err)
}
