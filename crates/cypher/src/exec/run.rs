//! Execute a plan. Each query part runs as segments of streaming operators
//! (MATCH, UNWIND, read procedures, non-blocking WITH) separated by blocking
//! operators: aggregating/sorting/limited projections and writes, which see
//! every row of their segment before the next segment starts. LIMIT without
//! ORDER BY stops the underlying scans as soon as enough rows exist.
use super::eval::{eval, predicate};
use super::plan::*;
use super::val::{Key, Val};
use super::*;
use brainwire_core::{ids::TypeId, schema::RelationshipIndexState};
use brainwire_store::graph::{self as store, EdgeRef};
use std::collections::{HashMap, HashSet};

pub(super) type Row = Vec<Val>;

pub(super) enum Flow {
    Stop,
    Error(ExecError),
}
impl From<ExecError> for Flow {
    fn from(value: ExecError) -> Self {
        Self::Error(value)
    }
}
impl From<GraphError> for Flow {
    fn from(value: GraphError) -> Self {
        Self::Error(value.into())
    }
}
impl From<StoreError> for Flow {
    fn from(value: StoreError) -> Self {
        Self::Error(value.into())
    }
}
pub(super) type Emit<'e> = dyn FnMut(Row) -> Result<(), Flow> + 'e;
type NodeCandidate<'a> = dyn FnMut(NodeId, Option<&[u8]>) -> Result<(), Flow> + 'a;

fn finish(result: Result<(), Flow>) -> Result<(), ExecError> {
    match result {
        Ok(()) | Err(Flow::Stop) => Ok(()),
        Err(Flow::Error(error)) => Err(error),
    }
}

/// Shared, immutable execution settings; a `Context` is rebuilt around them
/// whenever the transaction is borrowed immutably between writes.
pub(super) struct Env<'a> {
    pub catalog: &'a Catalog,
    pub params: &'a BTreeMap<String, Value>,
    pub budget: &'a Budget<'a>,
    pub visibility: Option<&'a ReadVisibility>,
    pub checkpoint_dir: Option<&'a std::path::Path>,
    pub vars: &'a Vars,
    pub cache: NodeCache,
    pub param_vals: &'a BTreeMap<String, Val>,
}

impl<'a> Env<'a> {
    pub(super) fn ctx<'t, 'db>(&'t self, txn: &'t Txn<'db>) -> Context<'t, 'db> {
        Context {
            txn,
            catalog: self.catalog,
            params: self.params,
            budget: self.budget,
            visibility: self.visibility,
            checkpoint_dir: self.checkpoint_dir,
            vars: self.vars,
            cache: Some(&self.cache),
            param_vals: Some(self.param_vals),
            partition: None,
        }
    }
}

pub(super) fn execute(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    plan: &Plan,
) -> Result<(QueryOutput, bool), ExecError> {
    let (fields, rows, wrote) = execute_rows(txn, env, plan)?;
    let output = output_rows(&env.ctx(txn), env.budget, fields, rows)?;
    Ok((output, wrote))
}

/// Run every part and keep the projected rows, entities as handles (`NodeId`,
/// `Relationship`): what `execute` holds before it reads any node's properties.
pub(super) fn execute_rows(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    plan: &Plan,
) -> Result<(Vec<String>, Vec<Row>, bool), ExecError> {
    let mut wrote = false;
    let mut rows: Vec<Row> = vec![];
    let mut seen: Option<HashSet<Key>> = None;
    for (index, part) in plan.parts.iter().enumerate() {
        let distinct = index > 0 && !plan.union_all[index - 1];
        if distinct && seen.is_none() {
            // A plain UNION deduplicates everything, including the first branch.
            let mut set = HashSet::new();
            let mut error = None;
            rows.retain_mut(|row| {
                if error.is_some() {
                    return false;
                }
                match retain_union_key(&mut set, std::mem::take(row), env.budget) {
                    Ok(Some(kept)) => {
                        *row = kept;
                        true
                    }
                    Ok(None) => false,
                    Err(failed) => {
                        error = Some(failed);
                        false
                    }
                }
            });
            if let Some(error) = error {
                return Err(error);
            }
            seen = Some(set);
        }
        let produced = run_part(txn, env, part, &mut wrote)?;
        if part.columns.is_empty() {
            continue;
        }
        for mut projected in produced {
            if let Some(set) = seen.as_mut() {
                let Some(kept) = retain_union_key(set, projected, env.budget)? else {
                    continue;
                };
                projected = kept;
            }
            rows.push(projected);
        }
    }
    Ok((plan.parts[0].columns.clone(), rows, wrote))
}

fn retain_union_key(
    seen: &mut HashSet<Key>,
    row: Row,
    budget: &Budget<'_>,
) -> Result<Option<Row>, ExecError> {
    let key = Key(row);
    if seen.contains(&key) {
        return Ok(None);
    }
    budget.retain_row(&key.0)?;
    let row = key.0.clone();
    seen.insert(key);
    Ok(Some(row))
}

/// The one part of a plan that can run lazily: write-free, no UNION, and no operator
/// that needs every row first (aggregation, ORDER BY, DISTINCT, SKIP/LIMIT, writes,
/// catalog procedures). Its rows can go to the client as they are produced.
pub(super) fn lazy_part(plan: &Plan) -> Option<&Part> {
    let [part] = plan.parts.as_slice() else {
        return None;
    };
    (!part.columns.is_empty() && !part.ops.iter().any(|op| op.blocking() || op.is_write()))
        .then_some(part)
}

/// Runs a [`lazy_part`] and hands each output row, materialized under the read
/// visibility, to *sink* as soon as it exists; nothing is retained. *sink* returns
/// false to stop. Time inside *sink* (a consumer waiting for PULL) is not charged to
/// the RUN deadline. Sequential: the parallel scan would collect every row first.
pub(super) fn stream_part(
    ctx: &Context<'_, '_>,
    env: &Env<'_>,
    part: &Part,
    sink: &mut dyn FnMut(Vec<Out>, usize) -> bool,
) -> Result<(), ExecError> {
    *env.cache.0.borrow_mut() = None;
    let outputs = output_slots(&part.outputs);
    let row = vec![Val::Null; env.vars.len()];
    finish(stream(ctx, &part.ops, row, &mut |row| {
        let row = project_output(row, &outputs);
        let out = graph::output_row(ctx, row)?;
        let bytes = Budget::output_bytes(&out);
        // Materialized output exists during the callback even when it is streamed.
        // Output retained by the sink takes its own lease before this scope ends.
        let _output_scope = ctx.budget.scope();
        ctx.budget.charge(bytes)?;
        let waited = std::time::Instant::now();
        let more = sink(out, bytes);
        ctx.budget.extend_deadline(waited.elapsed());
        if more {
            Ok(())
        } else {
            Err(Flow::Stop)
        }
    }))
}

/// Sequential lazy rows with owned output accounting. Only the callback owns
/// retained rows; it must keep the supplied lease until delivery or discard.
pub(super) fn stream_part_leased(
    ctx: &Context<'_, '_>,
    env: &Env<'_>,
    part: &Part,
    sink: &mut dyn FnMut(Vec<Out>, usize, QueryOutputLease) -> bool,
) -> Result<(), ExecError> {
    let mut error = None;
    stream_part(
        ctx,
        env,
        part,
        &mut |row, bytes| match ctx.budget.output_lease(bytes) {
            Ok(lease) => sink(row, bytes, lease),
            Err(failed) => {
                error = Some(failed);
                false
            }
        },
    )?;
    error.map_or(Ok(()), Err)
}

/// Materialize *rows* (read each entity's record and properties) as output, charging
/// what is retained. The deferred path calls it one PULL page at a time.
pub(super) fn output_rows(
    ctx: &Context<'_, '_>,
    budget: &Budget<'_>,
    fields: Vec<String>,
    rows: impl IntoIterator<Item = Row>,
) -> Result<QueryOutput, ExecError> {
    let rows = rows.into_iter();
    // The outer vector owns each row's Vec header. Admit its requested capacity
    // before allocating, and keep it separate from the row's payload credit.
    let capacity = rows.size_hint().0;
    budget.retain(capacity.saturating_mul(std::mem::size_of::<Vec<Out>>()))?;
    let mut output = QueryOutput {
        fields,
        rows: Vec::with_capacity(capacity),
    };
    for row in rows {
        let row = graph::output_row(ctx, row)?;
        budget.output(&row)?;
        if output.rows.len() == output.rows.capacity() {
            let capacity = output.rows.capacity();
            let next = capacity.saturating_mul(2).max(4);
            budget.retain(
                next.saturating_sub(capacity)
                    .saturating_mul(std::mem::size_of::<Vec<Out>>()),
            )?;
            let _growth_scope = budget.scope();
            // Reallocation can hold the old block while allocating its replacement.
            budget.charge(capacity.saturating_mul(std::mem::size_of::<Vec<Out>>()))?;
            output
                .rows
                .reserve_exact(next.saturating_sub(output.rows.len()));
        }
        output.rows.push(row);
    }
    budget.check_deadline()?;
    Ok(output)
}

// Terminal output no longer needs working slots. Move each final occurrence
// so large strings/lists are not duplicated while compacting the output row.
pub(super) fn output_slots(outputs: &[usize]) -> Vec<(usize, bool)> {
    let mut seen = HashSet::new();
    let mut slots: Vec<_> = outputs
        .iter()
        .rev()
        .map(|&slot| (slot, seen.insert(slot)))
        .collect();
    slots.reverse();
    slots
}

pub(super) fn project_output(mut row: Row, outputs: &[(usize, bool)]) -> Row {
    outputs
        .iter()
        .map(|&(slot, last)| {
            if last {
                std::mem::replace(&mut row[slot], Val::Null)
            } else {
                row[slot].clone()
            }
        })
        .collect()
}

fn run_part(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    part: &Part,
    wrote: &mut bool,
) -> Result<Vec<Row>, ExecError> {
    let ops = &part.ops;
    let outputs = output_slots(&part.outputs);
    let mut input: Vec<Row> = vec![vec![Val::Null; env.vars.len()]];
    let mut start = 0;
    loop {
        // Writes between segments may change any cached node.
        *env.cache.0.borrow_mut() = None;
        let barrier = (start..ops.len()).find(|&i| ops[i].blocking());
        let end = barrier.unwrap_or(ops.len());
        let segment = &ops[start..end];
        match barrier.map(|b| &ops[b]) {
            None => {
                let ctx = env.ctx(txn);
                if let Some(rows) = parallel::collect(&ctx, segment, &input)? {
                    return Ok(rows
                        .into_iter()
                        .map(|row| project_output(row, &outputs))
                        .collect());
                }
                let mut rows = vec![];
                for row in input {
                    finish(stream(&ctx, segment, row, &mut |row| {
                        let row = project_output(row, &outputs);
                        env.budget.retain_row(&row)?;
                        rows.push(row);
                        Ok(())
                    }))?;
                }
                return Ok(rows);
            }
            Some(Op::Project(projection)) => {
                let ctx = env.ctx(txn);
                if let Some(rows) = parallel::project(&ctx, segment, projection, &input)? {
                    input = rows;
                    start = end + 1;
                    if start >= ops.len() {
                        return Ok(input
                            .into_iter()
                            .map(|row| project_output(row, &outputs))
                            .collect());
                    }
                    continue;
                }
                let mut acc = Accumulator::new(projection, &ctx)?;
                let mut stopped = false;
                for row in input {
                    if stopped {
                        break;
                    }
                    match stream(&ctx, segment, row, &mut |row| acc.push(&ctx, row)) {
                        Ok(()) => {}
                        Err(Flow::Stop) => stopped = true,
                        Err(Flow::Error(error)) => return Err(error),
                    }
                }
                input = acc.finish(&ctx)?;
            }
            Some(op) => {
                let mut rows = vec![];
                {
                    let ctx = env.ctx(txn);
                    for row in input {
                        finish(stream(&ctx, segment, row, &mut |row| {
                            env.budget.retain_row(&row)?;
                            rows.push(row);
                            Ok(())
                        }))?;
                    }
                }
                input = write::apply(txn, env, op, rows, wrote)?;
            }
        }
        start = end + 1;
        if start >= ops.len() {
            return Ok(input
                .into_iter()
                .map(|row| project_output(row, &outputs))
                .collect());
        }
    }
}

/// Run streaming operators for one row, calling `emit` for each result.
pub(super) fn stream(
    ctx: &Context<'_, '_>,
    ops: &[Op],
    row: Row,
    emit: &mut Emit<'_>,
) -> Result<(), Flow> {
    let Some((op, rest)) = ops.split_first() else {
        return emit(row);
    };
    ctx.budget.tick()?;
    match op {
        Op::Match(plan) => run_match(ctx, plan, row, &mut |row| stream(ctx, rest, row, emit)),
        Op::Unwind { expr, slot } => {
            let _scope = ctx.budget.scope();
            match eval(expr, &row, ctx)? {
                Val::Null => Ok(()),
                Val::List(items) => {
                    for item in items {
                        let _row_scope = ctx.budget.scope();
                        let mut next = row.clone();
                        next[*slot] = item;
                        ctx.budget.row(&next)?;
                        stream(ctx, rest, next, emit)?;
                    }
                    Ok(())
                }
                other => {
                    let mut next = row;
                    next[*slot] = other;
                    stream(ctx, rest, next, emit)
                }
            }
        }
        Op::Call {
            name,
            args,
            yields,
            slots,
            where_,
        } => {
            for produced in procedures::run(name, args, yields, ctx)? {
                let mut next = row.clone();
                for (item, slot) in yields.iter().zip(slots) {
                    next[*slot] = produced.get(&item.alias).cloned().unwrap_or(Val::Null);
                }
                if let Some(test) = where_ {
                    if !predicate(test, &next, ctx)? {
                        continue;
                    }
                }
                stream(ctx, rest, next, emit)?;
            }
            Ok(())
        }
        Op::Project(projection) => {
            let _scope = ctx.budget.scope();
            let values = projection
                .items
                .iter()
                .map(|(expr, _)| eval(expr, &row, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            let mut next = row;
            for ((_, slot), value) in projection.items.iter().zip(values) {
                next[*slot] = value;
            }
            if let Some(test) = &projection.where_ {
                if !predicate(test, &next, ctx)? {
                    return Ok(());
                }
            }
            stream(ctx, rest, next, emit)
        }
        _ => unreachable!("blocking operators end a segment"),
    }
}

// -- projection --------------------------------------------------------------

enum AggState {
    Count(i64),
    Sum {
        int: i64,
        float: f64,
        floats: bool,
    },
    Avg {
        sum: f64,
        n: i64,
    },
    Min(Option<Val>),
    Max(Option<Val>),
    Collect(Vec<Val>),
    /// Original numeric values for the discrete percentile.
    Discrete(Vec<Val>),
    /// Numbers for stdev and the continuous percentile.
    Stdev(Vec<f64>),
}

struct Group {
    key: Vec<Val>,
    states: Vec<AggState>,
    seen: Vec<Option<HashSet<Key>>>,
}

pub(super) struct Accumulator<'p> {
    plan: &'p ProjectPlan,
    skip: usize,
    limit: usize,
    rows: Vec<(Vec<Val>, Row)>,
    distinct: HashSet<Key>,
    groups: HashMap<Key, usize>,
    group_list: Vec<Group>,
    descending: Vec<bool>,
}

impl<'p> Accumulator<'p> {
    pub(super) fn new(plan: &'p ProjectPlan, ctx: &Context<'_, '_>) -> Result<Self, ExecError> {
        let skip = plan
            .skip
            .as_ref()
            .map(|e| eval::limit(e, ctx))
            .transpose()?
            .unwrap_or(0);
        let limit = plan
            .limit
            .as_ref()
            .map(|e| eval::limit(e, ctx))
            .transpose()?
            .unwrap_or(usize::MAX);
        Ok(Self {
            plan,
            skip,
            limit,
            rows: vec![],
            distinct: HashSet::new(),
            groups: HashMap::new(),
            group_list: vec![],
            descending: plan.order.iter().map(|(_, d)| *d).collect(),
        })
    }

    fn keep(&self) -> usize {
        self.skip.saturating_add(self.limit)
    }

    pub(super) fn push(&mut self, ctx: &Context<'_, '_>, row: Row) -> Result<(), Flow> {
        if self.limit == 0 {
            return Err(Flow::Stop);
        }
        let _scope = ctx.budget.scope();
        if let Some(agg) = &self.plan.aggregate {
            let key: Vec<Val> = agg
                .keys
                .iter()
                .map(|&index| eval(&self.plan.items[index].0, &row, ctx))
                .collect::<Result<_, _>>()?;
            let lookup = Key(key);
            let index = match self.groups.get(&lookup) {
                Some(index) => *index,
                None => {
                    let key_bytes = lookup
                        .0
                        .iter()
                        .fold(0usize, |bytes, value| bytes.saturating_add(value.bytes()));
                    ctx.budget.retain(
                        key_bytes
                            .saturating_mul(2)
                            .saturating_add(128 * agg.aggs.len().max(1)),
                    )?;
                    let group = Group {
                        key: lookup.0.clone(),
                        states: agg.aggs.iter().map(|spec| initial(&spec.func)).collect(),
                        seen: agg
                            .aggs
                            .iter()
                            .map(|spec| spec.distinct.then(HashSet::new))
                            .collect(),
                    };
                    self.group_list.push(group);
                    self.groups.insert(lookup, self.group_list.len() - 1);
                    self.group_list.len() - 1
                }
            };
            let group = &mut self.group_list[index];
            for ((spec, state), seen) in agg.aggs.iter().zip(&mut group.states).zip(&mut group.seen)
            {
                let mut value = match &spec.arg {
                    None => {
                        if let AggState::Count(n) = state {
                            *n += 1;
                        }
                        continue;
                    }
                    Some(arg) => eval(arg, &row, ctx)?,
                };
                if value.is_null() {
                    continue;
                }
                if let Some(seen) = seen {
                    let key = Key(vec![value]);
                    if seen.contains(&key) {
                        continue;
                    }
                    // Admit the retained key copy before cloning its payload.
                    ctx.budget.retain(key.0[0].bytes())?;
                    value = key.0[0].clone();
                    seen.insert(key);
                }
                update(state, value, ctx)?;
            }
            return Ok(());
        }
        let mut values = self
            .plan
            .items
            .iter()
            .map(|(expr, _)| eval(expr, &row, ctx))
            .collect::<Result<Vec<_>, _>>()?;
        if self.plan.distinct {
            let key = Key(values);
            if self.distinct.contains(&key) {
                return Ok(());
            }
            ctx.budget.retain(
                key.0
                    .iter()
                    .fold(0usize, |bytes, value| bytes.saturating_add(value.bytes())),
            )?;
            values = key.0.clone();
            self.distinct.insert(key);
        }
        let mut next = row;
        for ((_, slot), value) in self.plan.items.iter().zip(values) {
            next[*slot] = value;
        }
        if let Some(test) = &self.plan.where_ {
            if !predicate(test, &next, ctx)? {
                return Ok(());
            }
        }
        let order = self
            .plan
            .order
            .iter()
            .map(|(expr, _)| eval(expr, &next, ctx))
            .collect::<Result<Vec<_>, _>>()?;
        ctx.budget.retain_row(&next)?;
        ctx.budget.retain_row(&order)?;
        self.rows.push((order, next));
        let keep = self.keep();
        if self.plan.order.is_empty() || self.plan.presorted {
            if self.rows.len() >= keep {
                return Err(Flow::Stop);
            }
        } else if keep != usize::MAX && self.rows.len() >= keep.saturating_mul(2).max(64) {
            self.trim(ctx, keep);
        }
        Ok(())
    }

    /// Fold a parallel worker's partial result into this one. Workers are
    /// merged in scan order, so first-seen group order and collect order
    /// match a sequential scan.
    pub(super) fn merge(
        &mut self,
        ctx: &Context<'_, '_>,
        other: Accumulator<'p>,
    ) -> Result<(), ExecError> {
        for group in other.group_list {
            let _scope = ctx.budget.scope();
            let key_bytes = group.key.iter().map(Val::bytes).sum::<usize>();
            ctx.budget.charge(key_bytes)?;
            let lookup = Key(group.key.clone());
            match self.groups.get(&lookup) {
                Some(&index) => {
                    for (state, incoming) in
                        self.group_list[index].states.iter_mut().zip(group.states)
                    {
                        merge_state(state, incoming, ctx.budget)?;
                    }
                }
                None => {
                    ctx.budget.retain(
                        key_bytes
                            .saturating_mul(2)
                            .saturating_add(128 * group.states.len().max(1)),
                    )?;
                    self.group_list.push(group);
                    self.groups.insert(lookup, self.group_list.len() - 1);
                }
            }
        }
        for (order, row) in other.rows {
            ctx.budget.retain_row(&row)?;
            self.rows.push((order, row));
        }
        Ok(())
    }

    fn trim(&mut self, ctx: &Context<'_, '_>, keep: usize) {
        eval::order_rows(&mut self.rows, &self.descending);
        for (order, row) in self.rows.drain(keep.min(self.rows.len())..) {
            ctx.budget.release_row(&order);
            ctx.budget.release_row(&row);
        }
    }

    pub(super) fn finish(mut self, ctx: &Context<'_, '_>) -> Result<Vec<Row>, ExecError> {
        if let Some(agg) = &self.plan.aggregate {
            if self.group_list.is_empty() && agg.keys.is_empty() {
                self.group_list.push(Group {
                    key: vec![],
                    states: agg.aggs.iter().map(|spec| initial(&spec.func)).collect(),
                    seen: vec![],
                });
            }
            let width = ctx.vars.len();
            for group in std::mem::take(&mut self.group_list) {
                let mut row = vec![Val::Null; width];
                for (&index, value) in agg.keys.iter().zip(group.key) {
                    row[self.plan.items[index].1] = value;
                }
                for (spec, state) in agg.aggs.iter().zip(group.states) {
                    row[spec.slot] = result(state, spec, ctx)?;
                }
                let values = self
                    .plan
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !agg.keys.contains(index))
                    .map(|(_, (expr, slot))| Ok((*slot, eval(expr, &row, ctx)?)))
                    .collect::<Result<Vec<_>, ExecError>>()?;
                for (slot, value) in values {
                    row[slot] = value;
                }
                if let Some(test) = &self.plan.where_ {
                    if !predicate(test, &row, ctx)? {
                        continue;
                    }
                }
                let order = self
                    .plan
                    .order
                    .iter()
                    .map(|(expr, _)| eval(expr, &row, ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                ctx.budget.retain_row(&row)?;
                self.rows.push((order, row));
            }
        }
        if !self.plan.order.is_empty() && !self.plan.presorted {
            eval::order_rows(&mut self.rows, &self.descending);
        }
        let keep = self.keep().min(self.rows.len());
        let skip = self.skip.min(keep);
        Ok(self
            .rows
            .into_iter()
            .take(keep)
            .skip(skip)
            .map(|(_, row)| row)
            .collect())
    }
}

fn merge_state(
    state: &mut AggState,
    incoming: AggState,
    budget: &Budget<'_>,
) -> Result<(), ExecError> {
    match (state, incoming) {
        (AggState::Count(a), AggState::Count(b)) => *a += b,
        (
            AggState::Sum { int, float, floats },
            AggState::Sum {
                int: i2,
                float: f2,
                floats: fl2,
            },
        ) => {
            if !*floats && !fl2 {
                *int = int
                    .checked_add(i2)
                    .ok_or_else(|| ExecError::Semantic("integer overflow in sum".into()))?;
            } else {
                let mine = if *floats { *float } else { *int as f64 };
                *float = mine + if fl2 { f2 } else { i2 as f64 };
                *floats = true;
            }
        }
        (AggState::Avg { sum, n }, AggState::Avg { sum: s2, n: n2 }) => {
            *sum += s2;
            *n += n2;
        }
        (AggState::Min(a), AggState::Min(b)) => {
            if let Some(b) = b {
                if a.as_ref().is_none_or(|x| val::sort_cmp(&b, x).is_lt()) {
                    budget.retain(b.bytes())?;
                    *a = Some(b);
                }
            }
        }
        (AggState::Max(a), AggState::Max(b)) => {
            if let Some(b) = b {
                if a.as_ref().is_none_or(|x| val::sort_cmp(&b, x).is_gt()) {
                    budget.retain(b.bytes())?;
                    *a = Some(b);
                }
            }
        }
        (AggState::Collect(a), AggState::Collect(b))
        | (AggState::Discrete(a), AggState::Discrete(b)) => {
            let growth = merge_capacity_bytes(a, b.len());
            budget.retain(
                b.iter()
                    .map(Val::bytes)
                    .sum::<usize>()
                    .saturating_add(growth),
            )?;
            a.extend(b);
        }
        (AggState::Stdev(a), AggState::Stdev(b)) => {
            let growth = merge_capacity_bytes(a, b.len());
            budget.retain(
                b.len()
                    .saturating_mul(std::mem::size_of::<f64>())
                    .saturating_add(growth),
            )?;
            a.extend(b);
        }
        _ => unreachable!("merged states come from the same aggregate"),
    }
    Ok(())
}

// A Vec extension can grow geometrically; account the replacement buffer
// before extending while the workers' original buffers remain retained.
fn merge_capacity_bytes<T>(values: &Vec<T>, additional: usize) -> usize {
    let length = values.len().saturating_add(additional);
    if length <= values.capacity() {
        0
    } else {
        length
            .max(4)
            .saturating_mul(2)
            .saturating_mul(std::mem::size_of::<T>())
    }
}

fn initial(func: &str) -> AggState {
    match func {
        "count" => AggState::Count(0),
        "sum" => AggState::Sum {
            int: 0,
            float: 0.0,
            floats: false,
        },
        "avg" => AggState::Avg { sum: 0.0, n: 0 },
        "min" => AggState::Min(None),
        "max" => AggState::Max(None),
        "collect" => AggState::Collect(vec![]),
        "percentiledisc" => AggState::Discrete(vec![]),
        _ => AggState::Stdev(vec![]),
    }
}

fn update(state: &mut AggState, value: Val, ctx: &Context<'_, '_>) -> Result<(), ExecError> {
    let number = |v: &Val| match v {
        Val::Int(i) => Ok(*i as f64),
        Val::Float(f) => Ok(*f),
        other => Err(ExecError::Semantic(format!(
            "numeric aggregate does not accept {}",
            other.type_name()
        ))),
    };
    match state {
        AggState::Count(n) => *n += 1,
        AggState::Sum { int, float, floats } => match value {
            Val::Int(i) if !*floats => match int.checked_add(i) {
                Some(total) => *int = total,
                None => return Err(ExecError::Semantic("integer overflow in sum".into())),
            },
            other => {
                if !*floats {
                    *float = *int as f64;
                    *floats = true;
                }
                *float += number(&other)?;
            }
        },
        AggState::Avg { sum, n } => {
            *sum += number(&value)?;
            *n += 1;
        }
        AggState::Min(current) => {
            if current
                .as_ref()
                .is_none_or(|c| val::sort_cmp(&value, c).is_lt())
            {
                *current = Some(value);
            }
        }
        AggState::Max(current) => {
            if current
                .as_ref()
                .is_none_or(|c| val::sort_cmp(&value, c).is_gt())
            {
                *current = Some(value);
            }
        }
        AggState::Collect(items) => {
            ctx.budget.retain(value.bytes())?;
            items.push(value);
        }
        AggState::Discrete(items) => {
            number(&value)?;
            ctx.budget
                .retain(value.bytes().saturating_add(merge_capacity_bytes(items, 1)))?;
            items.push(value);
        }
        AggState::Stdev(items) => {
            ctx.budget.retain(8)?;
            items.push(number(&value)?);
        }
    }
    Ok(())
}

fn result(state: AggState, spec: &AggSpec, ctx: &Context<'_, '_>) -> Result<Val, ExecError> {
    Ok(match state {
        AggState::Count(n) => Val::Int(n),
        AggState::Sum { int, float, floats } => {
            if floats {
                Val::Float(float)
            } else {
                Val::Int(int)
            }
        }
        AggState::Avg { sum, n } => {
            if n == 0 {
                Val::Null
            } else {
                Val::Float(sum / n as f64)
            }
        }
        AggState::Min(v) | AggState::Max(v) => v.unwrap_or(Val::Null),
        AggState::Collect(items) => Val::List(items),
        AggState::Discrete(mut items) => {
            let fraction = percentile_fraction(spec, ctx)?;
            if items.is_empty() {
                return Ok(Val::Null);
            }
            // Sort original values so large integers and mixed numeric types
            // retain both their exact ordering and the selected input's type.
            items.sort_unstable_by(discrete_cmp);
            let index = ((fraction * items.len() as f64).ceil() as usize).saturating_sub(1);
            items.swap_remove(index.min(items.len() - 1))
        }
        AggState::Stdev(mut items) => match spec.func.as_str() {
            "percentilecont" => {
                let fraction = percentile_fraction(spec, ctx)?;
                if items.is_empty() {
                    return Ok(Val::Null);
                }
                items.sort_by(f64::total_cmp);
                let position = fraction * (items.len() - 1) as f64;
                let (low, high) = (position.floor() as usize, position.ceil() as usize);
                Val::Float(items[low] + (items[high] - items[low]) * (position - low as f64))
            }
            func => {
                let sample = func == "stdev";
                if items.len() < usize::from(sample) + 1 {
                    return Ok(Val::Float(0.0));
                }
                let n = items.len() as f64;
                let mean = items.iter().sum::<f64>() / n;
                let squares = items.iter().map(|x| (x - mean).powi(2)).sum::<f64>();
                Val::Float((squares / if sample { n - 1.0 } else { n }).sqrt())
            }
        },
    })
}

// Equal numeric values need a deterministic representative regardless of
// input order or parallel merge: integers precede floats, then IEEE total order.
fn discrete_cmp(a: &Val, b: &Val) -> std::cmp::Ordering {
    val::sort_cmp(a, b).then_with(|| match (a, b) {
        (Val::Int(_), Val::Float(_)) => std::cmp::Ordering::Less,
        (Val::Float(_), Val::Int(_)) => std::cmp::Ordering::Greater,
        (Val::Float(a), Val::Float(b)) => a.total_cmp(b),
        _ => std::cmp::Ordering::Equal,
    })
}

fn percentile_fraction(spec: &AggSpec, ctx: &Context<'_, '_>) -> Result<f64, ExecError> {
    let fraction = match spec
        .fraction
        .as_ref()
        .map(|e| eval::constant(e, ctx))
        .transpose()?
    {
        Some(Value::Int(i)) => i as f64,
        Some(Value::Float(f)) => f,
        _ => return Err(ExecError::Semantic("percentile needs a number".into())),
    };
    if !(0.0..=1.0).contains(&fraction) {
        return Err(ExecError::Semantic(
            "percentile must be between 0.0 and 1.0".into(),
        ));
    }
    Ok(fraction)
}

// -- matching ------------------------------------------------------------------

fn run_match(
    ctx: &Context<'_, '_>,
    plan: &MatchPlan,
    row: Row,
    emit: &mut Emit<'_>,
) -> Result<(), Flow> {
    let mut pre = true;
    for test in &plan.pre {
        if !predicate(test, &row, ctx)? {
            pre = false;
            break;
        }
    }
    if !plan.optional {
        return if pre {
            steps(ctx, &plan.steps, row, emit)
        } else {
            Ok(())
        };
    }
    let mut found = false;
    if pre {
        steps(ctx, &plan.steps, row.clone(), &mut |row| {
            found = true;
            emit(row)
        })?;
    }
    if !found {
        let mut row = row;
        for slot in &plan.binds {
            row[*slot] = Val::Null;
        }
        emit(row)?;
    }
    Ok(())
}

fn filters_pass(ctx: &Context<'_, '_>, step: &Step, row: &[Val]) -> Result<bool, ExecError> {
    for test in &step.filters {
        if !predicate(test, row, ctx)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn steps(ctx: &Context<'_, '_>, list: &[Step], row: Row, emit: &mut Emit<'_>) -> Result<(), Flow> {
    let Some((step, rest)) = list.split_first() else {
        return emit(row);
    };
    if let StepKind::Anchor(node) = &step.kind {
        if scan_relationship_index(ctx, node, step, rest, &row, emit)? {
            return Ok(());
        }
        if let Some(scan) = edge_scan(ctx, node, step, rest) {
            return scan_edges(ctx, node, scan, row, rest, emit);
        }
    }
    let mut next = |row: Row| -> Result<(), Flow> {
        if filters_pass(ctx, step, &row)? {
            steps(ctx, rest, row, emit)
        } else {
            Ok(())
        }
    };
    match &step.kind {
        StepKind::Anchor(node) => anchor(ctx, node, row, &mut next),
        StepKind::Expand(expand) => expand_from(ctx, expand, row, &mut next),
        StepKind::Path { slot, nodes, rels } => {
            let mut ids = vec![];
            for node in nodes {
                match &row[*node] {
                    Val::Node(id) => ids.push(*id),
                    _ => return Ok(()),
                }
            }
            let rels: Vec<Val> = rels.iter().map(|slot| row[*slot].clone()).collect();
            let path = eval::path(&ids, &rels)?;
            let mut row = row;
            row[*slot] = path;
            next(row)
        }
    }
}

/// Probe only when an index supplies a conservative set of physical edges.
fn scan_relationship_index(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    anchor_step: &Step,
    rest: &[Step],
    row: &[Val],
    emit: &mut Emit<'_>,
) -> Result<bool, Flow> {
    if !ctx.txn.database().has_relationship_index_format()
        || ctx.partition.is_some()
        || node.bound
        || !matches!(node.access, Access::Scan)
        || !node.props.is_empty()
        || !anchor_step.filters.is_empty()
    {
        return Ok(false);
    }
    let Some((expand_step, after)) = rest.split_first() else {
        return Ok(false);
    };
    let StepKind::Expand(expand) = &expand_step.kind else {
        return Ok(false);
    };
    if expand.from != node.slot || expand.rel_bound || expand.to.bound || expand.length.is_some() {
        return Ok(false);
    }
    let [type_id] = expand.types.as_slice() else {
        return Ok(false);
    };
    let Some(ty) = ctx.catalog.edge_types.iter().find(|ty| ty.id == *type_id) else {
        return Ok(false);
    };
    let constant = |expr: &Expr| matches!(expr, Expr::Param(_) | Expr::Literal(_));
    let ready = |field: &str| {
        ty.property_indexes
            .iter()
            .any(|idx| idx.property == field && idx.state == RelationshipIndexState::Ready)
    };
    let from_pattern = expand.rel_props.iter().find_map(|(field, expr)| {
        (ready(field) && constant(expr)).then_some((field.as_str(), CmpOp::Eq, expr))
    });
    fn relationship_property<'e>(expr: &'e Expr, vars: &Vars, slot: usize) -> Option<&'e str> {
        match expr {
            Expr::Prop(base, field) if matches!(base.as_ref(), Expr::Var(name) if vars.slot(name) == Some(slot)) => {
                Some(field.as_str())
            }
            _ => None,
        }
    }
    let filters = || {
        expand_step.filters.iter().filter_map(|filter| {
            let Expr::Cmp(left, op, right) = filter else {
                return None;
            };
            if *op == CmpOp::Ne {
                return None;
            }
            if let Some(field) = relationship_property(left, ctx.vars, expand.rel)
                .filter(|field| ready(field) && constant(right))
            {
                return Some((field, *op, right.as_ref()));
            }
            let field = relationship_property(right, ctx.vars, expand.rel)
                .filter(|field| ready(field) && constant(left))?;
            let reversed = match op {
                CmpOp::Eq => CmpOp::Eq,
                CmpOp::Lt => CmpOp::Gt,
                CmpOp::Le => CmpOp::Ge,
                CmpOp::Gt => CmpOp::Lt,
                CmpOp::Ge => CmpOp::Le,
                CmpOp::Ne => return None,
            };
            Some((field, reversed, left.as_ref()))
        })
    };
    let Some((field, op, expr)) = from_pattern.or_else(|| filters().next()) else {
        return Ok(false);
    };
    let _scope = ctx.budget.scope();
    let value = eval(expr, row, ctx)?.into_value()?;
    let mut equality = None;
    let mut lower = None;
    let mut upper = None;
    match op {
        CmpOp::Eq => equality = Some(value),
        CmpOp::Gt | CmpOp::Ge => {
            lower = Some(value);
            if let Some((_, _, expr)) = filters().find(|(candidate, op, _)| {
                *candidate == field && matches!(op, CmpOp::Lt | CmpOp::Le)
            }) {
                upper = Some(eval(expr, row, ctx)?.into_value()?);
            }
        }
        CmpOp::Lt | CmpOp::Le => {
            upper = Some(value);
            if let Some((_, _, expr)) = filters().find(|(candidate, op, _)| {
                *candidate == field && matches!(op, CmpOp::Gt | CmpOp::Ge)
            }) {
                lower = Some(eval(expr, row, ctx)?.into_value()?);
            }
        }
        CmpOp::Ne => return Ok(false),
    }
    store::visit_relationship_index::<Flow>(
        ctx.txn,
        ty,
        field,
        equality.as_ref(),
        lower.as_ref(),
        upper.as_ref(),
        |(source, edge_type, target, id)| {
            ctx.budget.tick()?;
            let mut orientation = |from, other, cf| {
                let _scope = ctx.budget.scope();
                ctx.budget.charge(16)?;
                let mut row = row.to_vec();
                row[node.slot] = Val::Node(from);
                if !node_ok(ctx, node, from, &row)? {
                    return Ok(());
                }
                let edge = EdgeRef {
                    edge_type,
                    other,
                    id,
                };
                try_edge(ctx, expand, &row, from, edge, cf, &mut |row| {
                    if filters_pass(ctx, expand_step, &row)? {
                        steps(ctx, after, row, emit)
                    } else {
                        Ok(())
                    }
                })
            };
            if matches!(expand.direction, Direction::Out | Direction::Both) {
                orientation(source, target, Cf::Out)?;
            }
            if expand.direction == Direction::In
                || (expand.direction == Direction::Both && source != target)
            {
                orientation(target, source, Cf::In)?;
            }
            Ok(())
        },
    )
}

/// A directional expansion can stream adjacency across owner label ranges
/// instead of visiting every node and seeking each one's edges.
fn edge_scan<'s>(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    step: &Step,
    rest: &'s [Step],
) -> Option<&'s Expand> {
    if node.bound
        || !matches!(node.access, Access::Scan)
        || node.labels.len() > 1
        || (ctx.visibility.is_some() && !node.labels.is_empty())
        || !node.props.is_empty()
        || !step.filters.is_empty()
    {
        return None;
    }
    match rest.first().map(|s| &s.kind) {
        Some(StepKind::Expand(expand))
            if expand.from == node.slot
                && expand.length.is_none()
                && !expand.rel_bound
                && expand.direction != Direction::Both
                && (!node.labels.is_empty()
                    || expand.types.len() == 1
                    || expand.no_matching_types) =>
        {
            Some(expand)
        }
        _ => None,
    }
}

fn scan_edges(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    expand: &Expand,
    row: Row,
    rest: &[Step],
    emit: &mut Emit<'_>,
) -> Result<(), Flow> {
    if expand.no_matching_types {
        return Ok(());
    }
    let cf = if expand.direction == Direction::Out {
        Cf::Out
    } else {
        Cf::In
    };
    let (expand_step, after) = rest.split_first().expect("expand step follows");
    let mut visit = |owner: NodeId, edge: EdgeRef| {
        let _scope = ctx.budget.scope();
        ctx.budget.charge(16)?;
        if node.labels.is_empty() {
            if ctx.visibility.is_some() && !graph::visible(ctx, owner)? {
                return Ok(());
            }
            ctx.budget.row(&row)?;
        }
        let mut row = row.clone();
        row[node.slot] = Val::Node(owner);
        try_edge(ctx, expand, &row, owner, edge, cf, &mut |row| {
            if filters_pass(ctx, expand_step, &row)? {
                steps(ctx, after, row, emit)
            } else {
                Ok(())
            }
        })
    };
    if let Some(label) = node.labels.first() {
        let ty = ctx.catalog.node_type(label).expect("validated label");
        return match ctx.partition.filter(|(slot, _, _)| *slot == node.slot) {
            Some((_, lo, hi)) => {
                store::visit_edges_in_range::<Flow>(ctx.txn, cf, lo, hi, &expand.types, visit)
            }
            None => store::visit_edges_of_label::<Flow>(ctx.txn, cf, ty.id, &expand.types, visit),
        };
    }

    let edge_type = ctx
        .catalog
        .edge_types
        .iter()
        .find(|ty| ty.id == expand.types[0])
        .expect("validated relationship type");
    let mut visit_typed = |owner, edge: EdgeRef| {
        if edge.edge_type == edge_type.id {
            visit(owner, edge)
        } else {
            // Also check cancellation while skipping unrelated edges.
            ctx.budget.tick().map_err(Flow::from)
        }
    };
    if let Some((_, lo, hi)) = ctx.partition.filter(|(slot, _, _)| *slot == node.slot) {
        return store::visit_edges_in_range::<Flow>(ctx.txn, cf, lo, hi, &[], visit_typed);
    }

    // Keep the original anchor scan order and visit each label just once,
    // even when several signatures share an endpoint label.
    for ty in &ctx.catalog.node_types {
        if edge_type
            .signatures
            .iter()
            .any(|&(source, target)| ty.id == if cf == Cf::Out { source } else { target })
        {
            store::visit_edges_of_label::<Flow>(ctx.txn, cf, ty.id, &[], &mut visit_typed)?;
        }
    }
    Ok(())
}

fn node_ok(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    id: NodeId,
    row: &[Val],
) -> Result<bool, ExecError> {
    node_ok_with_value(ctx, node, id, row, None)
}

fn node_ok_with_value(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    id: NodeId,
    row: &[Val],
    value: Option<&[u8]>,
) -> Result<bool, ExecError> {
    if !node.labels.is_empty() {
        let label = graph::label_of(ctx, id)?;
        if node.labels.iter().any(|l| l != label) {
            return Ok(false);
        }
    }
    if !node.bound && ctx.visibility.is_some() && !graph::visible(ctx, id)? {
        return Ok(false);
    }
    if let (Some(value), Some(cache)) = (value, ctx.cache) {
        cache_node_value(cache, ctx.budget, id, value)?;
    }
    for (name, expected) in &node.props {
        let _scope = ctx.budget.scope();
        let expected = eval(expected, row, ctx)?;
        let actual = Val::from(graph::node_property(ctx, id, name)?);
        if val::equals(&actual, &expected) != Some(true) {
            return Ok(false);
        }
    }
    Ok(true)
}

// A scan cache keeps one raw row. Admit growth before copying, including the
// old buffer while a replacement is allocated, and keep its capacity retained.
fn cache_node_value(
    cache: &NodeCache,
    budget: &Budget<'_>,
    id: NodeId,
    value: &[u8],
) -> Result<(), ExecError> {
    let mut slot = cache.0.borrow_mut();
    let capacity = slot.as_ref().map_or(0, |(_, bytes)| bytes.capacity());
    if value.len() > capacity {
        if !budget.try_retain_cache(value.len() - capacity, capacity)? {
            // A skipped replacement must not expose an older value for this ID.
            if slot.as_ref().is_some_and(|(cached, _)| *cached == id) {
                *slot = None;
            }
            return Ok(());
        }
        *slot = Some((id, value.to_vec()));
    } else if let Some((cached, buffer)) = slot.as_mut() {
        *cached = id;
        buffer.clear();
        buffer.extend_from_slice(value);
    } else {
        *slot = Some((id, Vec::new()));
    }
    Ok(())
}

fn anchor(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    row: Row,
    next: &mut Emit<'_>,
) -> Result<(), Flow> {
    if matches!(node.access, Access::Empty) {
        return Ok(());
    }
    let mut candidate = |id: NodeId, value: Option<&[u8]>| -> Result<(), Flow> {
        let _scope = ctx.budget.scope();
        ctx.budget.charge(16)?;
        if !node_ok_with_value(ctx, node, id, &row, value)? {
            return Ok(());
        }
        let mut row = row.clone();
        row[node.slot] = Val::Node(id);
        ctx.budget.row(&row)?;
        next(row)
    };
    let ty = node
        .labels
        .first()
        .map(|label| ctx.catalog.node_type(label).expect("validated label"));
    let scan = |candidate: &mut NodeCandidate<'_>| -> Result<(), Flow> {
        let types: Vec<TypeId> = match ty {
            Some(ty) => vec![ty.id],
            None => ctx.catalog.node_types.iter().map(|t| t.id).collect(),
        };
        let partition = ctx.partition.filter(|(slot, _, _)| *slot == node.slot);
        for ty in types {
            let mut each = |id: NodeId, bytes: &[u8]| candidate(id, Some(bytes));
            match partition {
                Some((_, lo, hi)) => {
                    store::visit_nodes_in_range_with_values::<Flow>(ctx.txn, lo, hi, &mut each)?
                }
                None => store::visit_nodes_of_type_with_values::<Flow>(ctx.txn, ty, &mut each)?,
            }
        }
        Ok(())
    };
    match &node.access {
        Access::Empty => Ok(()),
        Access::Bound => match &row[node.slot] {
            Val::Node(id) => {
                let id = *id;
                if !node_ok(ctx, node, id, &row)? {
                    return Ok(());
                }
                next(row)
            }
            Val::Null => Ok(()),
            other => Err(ExecError::Semantic(format!(
                "expected a node, found {}",
                other.type_name()
            ))
            .into()),
        },
        Access::Scan => scan(&mut candidate),
        Access::EntityEq(expr) => {
            let ty = ty.expect("entity access has a label");
            if let Val::Str(entity_id) = eval(expr, &row, ctx)? {
                for id in store::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))? {
                    candidate(id, None)?;
                }
            }
            Ok(())
        }
        Access::EntityIn(expr) => {
            let ty = ty.expect("entity access has a label");
            let _input_scope = ctx.budget.scope();
            let Val::List(items) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let mut entities = HashSet::new();
            let mut ids = BTreeSet::new();
            for item in items {
                ctx.budget.tick()?;
                if let Val::Str(entity_id) = item {
                    if entities.contains(&entity_id) {
                        continue;
                    }
                    // Include set overhead and the temporary primary-key buffer.
                    ctx.budget.charge(
                        entity_id
                            .len()
                            .saturating_mul(2)
                            .saturating_add(std::mem::size_of::<String>())
                            .saturating_add(128),
                    )?;
                    for id in store::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))? {
                        if !ids.contains(&id) {
                            ctx.budget.charge(128)?;
                            ids.insert(id);
                        }
                    }
                    entities.insert(entity_id);
                }
            }
            for id in ids {
                candidate(id, None)?;
            }
            Ok(())
        }
        Access::EntityRange {
            lower,
            upper,
            prefix,
        } => {
            let ty = ty.expect("entity access has a label");
            let text = |bound: &Option<(Expr, bool)>| -> Result<Option<Option<String>>, ExecError> {
                Ok(match bound {
                    None => Some(None),
                    Some((expr, _)) => match eval(expr, &row, ctx)? {
                        Val::Str(s) => Some(Some(s)),
                        _ => None,
                    },
                })
            };
            let (Some(lower), Some(upper)) = (text(lower)?, text(upper)?) else {
                return Ok(());
            };
            let prefix = match prefix {
                None => None,
                Some(expr) => match eval(expr, &row, ctx)? {
                    Val::Str(s) => Some(s),
                    _ => return Ok(()),
                },
            };
            let from = match (&lower, &prefix) {
                (Some(l), Some(p)) => l.clone().max(p.clone()),
                (Some(l), None) => l.clone(),
                (None, Some(p)) => p.clone(),
                (None, None) => String::new(),
            };
            store::visit_entities_from::<Flow>(ctx.txn, &from, |entity_id, node_ty, id| {
                if upper.as_deref().is_some_and(|u| entity_id > u)
                    || prefix.as_deref().is_some_and(|p| !entity_id.starts_with(p))
                {
                    return Ok(false);
                }
                if node_ty == ty.id {
                    candidate(id, None)?;
                }
                Ok(true)
            })
        }
        Access::IndexEq(field, expr) => {
            let ty = ty.expect("index access has a label");
            let value = eval(expr, &row, ctx)?;
            if value.is_null() {
                return Ok(());
            }
            let value = value.into_value()?;
            if !store::visit_by_indexed_property::<Flow>(ctx.txn, ty, field, &value, |id| {
                candidate(id, None)
            })? {
                return scan(&mut candidate);
            }
            Ok(())
        }
        Access::IndexIn(field, expr) => {
            let ty = ty.expect("index access has a label");
            // These collections are temporary, but must remain admitted while
            // candidate emission and its nested row scopes are active.
            let input_scope = ctx.budget.scope();
            let Val::List(items) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let mut values = HashSet::new();
            let mut ids = BTreeSet::new();
            let mut items = items.into_iter();
            while let Some(item) = items.next() {
                ctx.budget.tick()?;
                if item.is_null() {
                    continue;
                }
                let key_bytes = item
                    .bytes()
                    .saturating_add(std::mem::size_of::<Key>())
                    .saturating_add(128);
                let key = {
                    let _probe_scope = ctx.budget.scope();
                    ctx.budget.charge(key_bytes)?;
                    let key = Key(vec![item.clone()]);
                    if values.contains(&key) {
                        // Drop the duplicate allocation before its scope restores.
                        drop(key);
                        continue;
                    }
                    key
                };
                // Transfer this admitted probe into outer temporary accounting.
                // No allocation intervenes; the shared lease keeps its high-water.
                ctx.budget.charge(key_bytes)?;
                values.insert(key);
                let value = item.into_value()?;
                let ready =
                    store::visit_by_indexed_property::<Flow>(ctx.txn, ty, field, &value, |id| {
                        ctx.budget.tick()?;
                        if !ids.contains(&id) {
                            // Include tree-node/allocation overhead before insertion.
                            ctx.budget.charge(128)?;
                            ids.insert(id);
                        }
                        Ok(())
                    })?;
                if !ready {
                    // No IDs have been emitted; fallback must not carry the
                    // partial indexed lookup's allocations into the scan.
                    drop(value);
                    drop(items);
                    drop(values);
                    drop(ids);
                    drop(input_scope);
                    return scan(&mut candidate);
                }
            }
            for id in ids {
                candidate(id, None)?;
            }
            Ok(())
        }
        Access::IndexRange {
            field,
            lower,
            upper,
        } => {
            let ty = ty.expect("index access has a label");
            let bound = |b: &Option<(Expr, bool)>| -> Result<Option<Value>, ExecError> {
                b.as_ref()
                    .map(|(e, _)| eval(e, &row, ctx)?.into_value())
                    .transpose()
            };
            let (lower, upper) = (bound(lower)?, bound(upper)?);
            if matches!(lower, Some(Value::Null)) || matches!(upper, Some(Value::Null)) {
                return Ok(());
            }
            let ready = store::visit_index_range::<Flow>(
                ctx.txn,
                ty,
                field,
                lower.as_ref(),
                upper.as_ref(),
                None,
                |id| candidate(id, None),
            )?;
            if !ready {
                return scan(&mut candidate);
            }
            Ok(())
        }
        Access::IndexPrefix(field, expr) => {
            let ty = ty.expect("index access has a label");
            let Val::Str(prefix) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let ready = store::visit_index_range::<Flow>(
                ctx.txn,
                ty,
                field,
                None,
                None,
                Some(&prefix),
                |id| candidate(id, None),
            )?;
            if !ready {
                return scan(&mut candidate);
            }
            Ok(())
        }
        Access::NodeId(expr) => {
            let Expr::Call { name, args, .. } = expr else {
                unreachable!("planned id lookup")
            };
            let id = match (name.as_str(), eval(&args[0], &row, ctx)?) {
                ("id", Val::Int(raw)) if raw >= 0 => NodeId(raw as u64),
                ("elementid", Val::Str(text)) => match NodeId::parse_element_id(&text) {
                    Ok(id) => id,
                    Err(_) => return Ok(()),
                },
                _ => return Ok(()),
            };
            if ty.is_some_and(|ty| ty.id != id.type_id())
                || !ctx.catalog.node_types.iter().any(|t| t.id == id.type_id())
                || !store::node_exists(ctx.txn, id)?
            {
                return Ok(());
            }
            candidate(id, None)
        }
    }
}

fn rel_used(row: &[Val], slots: &[usize], rel: &Relationship) -> bool {
    slots.iter().any(|slot| match &row[*slot] {
        Val::Rel(other) => other == rel,
        Val::List(items) => items
            .iter()
            .any(|item| matches!(item, Val::Rel(other) if other == rel)),
        _ => false,
    })
}

fn relationship(from: NodeId, edge: &EdgeRef, cf: Cf) -> Relationship {
    let (source, target) = if cf == Cf::Out {
        (from, edge.other)
    } else {
        (edge.other, from)
    };
    Relationship {
        source,
        target,
        edge_type: edge.edge_type,
        id: edge.id,
    }
}

fn rel_props_ok(
    ctx: &Context<'_, '_>,
    props: &[(String, Expr)],
    rel: &Relationship,
    row: &[Val],
) -> Result<bool, ExecError> {
    for (name, expected) in props {
        let expected = eval(expected, row, ctx)?;
        let actual = Val::from(graph::edge_property(ctx, rel, name)?);
        if val::equals(&actual, &expected) != Some(true) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Try one adjacency entry of `from`; binds the relationship and target.
fn try_edge(
    ctx: &Context<'_, '_>,
    expand: &Expand,
    row: &[Val],
    from: NodeId,
    edge: EdgeRef,
    cf: Cf,
    next: &mut Emit<'_>,
) -> Result<(), Flow> {
    let rel = relationship(from, &edge, cf);
    if rel_used(row, &expand.distinct_from, &rel) {
        return Ok(());
    }
    if expand.to.bound {
        if row[expand.to.slot] != Val::Node(edge.other)
            || !node_ok(ctx, &expand.to, edge.other, row)?
        {
            return Ok(());
        }
    } else if !node_ok(ctx, &expand.to, edge.other, row)? {
        return Ok(());
    }
    if !rel_props_ok(ctx, &expand.rel_props, &rel, row)? {
        return Ok(());
    }
    let mut next_row = row.to_vec();
    next_row[expand.rel] = Val::Rel(rel);
    next_row[expand.to.slot] = Val::Node(edge.other);
    next(next_row)
}

fn adjacency(
    ctx: &Context<'_, '_>,
    node: NodeId,
    direction: Direction,
    types: &[brainwire_core::ids::EdgeTypeId],
    visit: &mut dyn FnMut(EdgeRef, Cf) -> Result<(), Flow>,
) -> Result<(), Flow> {
    let families: &[Cf] = match direction {
        Direction::Out => &[Cf::Out],
        Direction::In => &[Cf::In],
        Direction::Both => &[Cf::Out, Cf::In],
    };
    for &cf in families {
        let mut each = |edge: EdgeRef| {
            // An undirected self-loop is listed once.
            if direction == Direction::Both && cf == Cf::In && edge.other == node {
                return Ok(());
            }
            visit(edge, cf)
        };
        if types.is_empty() {
            store::visit_edges::<Flow>(ctx.txn, cf, node, &mut each)?;
        } else {
            for ty in types {
                store::visit_edges_of_type::<Flow>(ctx.txn, cf, node, *ty, &mut each)?;
            }
        }
    }
    Ok(())
}

fn expand_from(
    ctx: &Context<'_, '_>,
    expand: &Expand,
    row: Row,
    next: &mut Emit<'_>,
) -> Result<(), Flow> {
    let from = match &row[expand.from] {
        Val::Node(id) => *id,
        Val::Null => return Ok(()),
        other => {
            return Err(ExecError::Semantic(format!(
                "expected a node, found {}",
                other.type_name()
            ))
            .into());
        }
    };
    if expand.rel_bound {
        let Val::Rel(rel) = row[expand.rel] else {
            return Ok(());
        };
        let candidates = [
            (
                rel.source == from && expand.direction != Direction::In,
                rel.target,
                Cf::Out,
            ),
            (
                rel.target == from && expand.direction != Direction::Out,
                rel.source,
                Cf::In,
            ),
        ];
        for (ok, other, cf) in candidates {
            if ok
                && !expand.no_matching_types
                && (expand.types.is_empty() || expand.types.contains(&rel.edge_type))
            {
                let edge = EdgeRef {
                    edge_type: rel.edge_type,
                    other,
                    id: rel.id,
                };
                if expand.to.bound && row[expand.to.slot] != Val::Node(other) {
                    continue;
                }
                if !node_ok(ctx, &expand.to, other, &row)? {
                    continue;
                }
                let mut next_row = row.clone();
                next_row[expand.to.slot] = Val::Node(other);
                let _ = (edge, cf);
                return next(next_row);
            }
        }
        return Ok(());
    }
    let Some((min, max)) = expand.length else {
        if expand.no_matching_types {
            return Ok(());
        }
        return adjacency(
            ctx,
            from,
            expand.direction,
            &expand.types,
            &mut |edge, cf| {
                let _scope = ctx.budget.scope();
                try_edge(ctx, expand, &row, from, edge, cf, next)
            },
        );
    };
    let mut path: Vec<Relationship> = vec![];
    var_length(ctx, expand, &row, from, 0, min, max, &mut path, next)
}

#[allow(clippy::too_many_arguments)]
fn var_length(
    ctx: &Context<'_, '_>,
    expand: &Expand,
    row: &[Val],
    current: NodeId,
    depth: u32,
    min: u32,
    max: Option<u32>,
    path: &mut Vec<Relationship>,
    next: &mut Emit<'_>,
) -> Result<(), Flow> {
    ctx.budget.check_deadline()?;
    if depth > 1024 {
        return Err(
            ExecError::Semantic("variable-length expansion deeper than 1024 hops".into()).into(),
        );
    }
    if depth >= min {
        let end_ok = if expand.to.bound {
            row[expand.to.slot] == Val::Node(current) && node_ok(ctx, &expand.to, current, row)?
        } else {
            node_ok(ctx, &expand.to, current, row)?
        };
        if end_ok {
            let mut rels: Vec<Val> = path.iter().copied().map(Val::Rel).collect();
            if expand.reversed {
                rels.reverse();
            }
            let mut next_row = row.to_vec();
            next_row[expand.rel] = Val::List(rels);
            next_row[expand.to.slot] = Val::Node(current);
            ctx.budget.row(&next_row)?;
            next(next_row)?;
        }
    }
    if expand.no_matching_types || max.is_some_and(|max| depth >= max) {
        return Ok(());
    }
    adjacency(
        ctx,
        current,
        expand.direction,
        &expand.types,
        &mut |edge, cf| {
            let rel = relationship(current, &edge, cf);
            if path.contains(&rel) || rel_used(row, &expand.distinct_from, &rel) {
                return Ok(());
            }
            if ctx.visibility.is_some() && !graph::visible(ctx, edge.other)? {
                return Ok(());
            }
            if !rel_props_ok(ctx, &expand.rel_props, &rel, row)? {
                return Ok(());
            }
            path.push(rel);
            let result = var_length(
                ctx,
                expand,
                row,
                edge.other,
                depth + 1,
                min,
                max,
                path,
                next,
            );
            path.pop();
            result
        },
    )
}

/// A pattern predicate: anchored on its first bound node (or a label scan).
pub(super) fn pattern_exists(
    ctx: &Context<'_, '_>,
    row: &[Val],
    pattern: &Pattern,
) -> Result<bool, ExecError> {
    let chain: Vec<&NodePat> = std::iter::once(&pattern.start)
        .chain(pattern.hops.iter().map(|(_, n)| n))
        .collect();
    let bound = |pat: &NodePat| -> Option<NodeId> {
        let slot = ctx.vars.slot(pat.var.as_deref()?)?;
        match row.get(slot)? {
            Val::Node(id) => Some(*id),
            _ => None,
        }
    };
    let node_matches = |pat: &NodePat, id: NodeId| -> Result<bool, ExecError> {
        if let Some(existing) = bound(pat) {
            if existing != id {
                return Ok(false);
            }
        }
        let step = NodeStep {
            slot: 0,
            bound: bound(pat).is_some(),
            labels: pat.labels.clone(),
            props: pat.props.clone(),
            access: Access::Bound,
        };
        node_ok(ctx, &step, id, row)
    };
    let types = |rel: &RelPat| -> Result<Vec<brainwire_core::ids::EdgeTypeId>, ExecError> {
        Ok(rel
            .types
            .iter()
            .filter_map(|name| ctx.catalog.edge_type(name).map(|t| t.id))
            .collect())
    };
    // Walk the chain left to right from a known start node.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        ctx: &Context<'_, '_>,
        row: &[Val],
        pattern: &Pattern,
        hop: usize,
        current: NodeId,
        used: &mut Vec<Relationship>,
        node_matches: &dyn Fn(&NodePat, NodeId) -> Result<bool, ExecError>,
        types: &dyn Fn(&RelPat) -> Result<Vec<brainwire_core::ids::EdgeTypeId>, ExecError>,
    ) -> Result<bool, ExecError> {
        let Some((rel, end)) = pattern.hops.get(hop) else {
            return Ok(true);
        };
        let (min, max) = rel.length.unwrap_or((1, Some(1)));
        let edge_types = types(rel)?;
        let mut found = false;
        let mut frontier = vec![(current, 0u32, used.len())];
        while let Some((node, depth, _)) = frontier.pop() {
            if depth >= min
                && node_matches(end, node)?
                && walk(ctx, row, pattern, hop + 1, node, used, node_matches, types)?
            {
                found = true;
                break;
            }
            if (!rel.types.is_empty() && edge_types.is_empty())
                || max.is_some_and(|m| depth >= m)
                || depth > 64
            {
                continue;
            }
            let mut next = vec![];
            let result = adjacency(ctx, node, rel.direction, &edge_types, &mut |edge, cf| {
                let r = relationship(node, &edge, cf);
                if used.contains(&r) {
                    return Ok(());
                }
                if ctx.visibility.is_some() && !graph::visible(ctx, edge.other)? {
                    return Ok(());
                }
                if !rel_props_ok(ctx, &rel.props, &r, row)? {
                    return Ok(());
                }
                next.push((edge.other, r));
                Ok(())
            });
            finish(result)?;
            for (other, r) in next {
                used.push(r);
                frontier.push((other, depth + 1, used.len()));
            }
        }
        Ok(found)
    }
    let mut used = vec![];
    if let Some(start) = bound(chain[0]) {
        return Ok(node_matches(chain[0], start)?
            && walk(
                ctx,
                row,
                pattern,
                0,
                start,
                &mut used,
                &node_matches,
                &types,
            )?);
    }
    // Anchor on the far end when only it is bound: reverse a one-hop pattern.
    if pattern.hops.len() == 1 {
        if let Some(end) = bound(chain[1]) {
            let (rel, _) = &pattern.hops[0];
            let reversed = Pattern {
                path: None,
                start: chain[1].clone(),
                hops: vec![(
                    RelPat {
                        direction: match rel.direction {
                            Direction::Out => Direction::In,
                            Direction::In => Direction::Out,
                            Direction::Both => Direction::Both,
                        },
                        ..rel.clone()
                    },
                    chain[0].clone(),
                )],
            };
            return walk(
                ctx,
                row,
                &reversed,
                0,
                end,
                &mut used,
                &node_matches,
                &types,
            );
        }
    }
    let mut found = false;
    let label_types: Vec<_> = match chain[0].labels.first() {
        Some(label) => ctx
            .catalog
            .node_type(label)
            .map(|t| vec![t.id])
            .unwrap_or_default(),
        None => ctx.catalog.node_types.iter().map(|t| t.id).collect(),
    };
    for ty in label_types {
        let result = store::visit_nodes_of_type::<Flow>(ctx.txn, ty, |id| {
            if node_matches(chain[0], id)?
                && walk(ctx, row, pattern, 0, id, &mut vec![], &node_matches, &types)?
            {
                found = true;
                return Err(Flow::Stop);
            }
            Ok(())
        });
        finish(result)?;
        if found {
            break;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod merge_budget_tests {
    use super::*;

    fn with_projection_context(limit: usize, check: impl FnOnce(&Context<'_, '_>)) {
        use brainwire_store::db::{Database, Resources, StoreConfig};
        let directory = tempfile::tempdir().unwrap();
        let db =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let txn = db.begin();
        let catalog = Catalog {
            version: 1,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let params = BTreeMap::new();
        let vars = Vars::default();
        let param_vals = BTreeMap::from([("payload".into(), Val::Str("x".repeat(16 << 10)))]);
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: limit,
                timeout: None,
            },
            None,
        );
        let context = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &vars,
            cache: None,
            param_vals: Some(&param_vals),
            partition: None,
        };
        check(&context);
    }

    #[test]
    fn unlabeled_relationship_scan_respects_owner_partition() {
        use brainwire_core::schema::GraphMapping;
        use brainwire_store::db::{Database, Resources, StoreConfig};

        let directory = tempfile::tempdir().unwrap();
        let db =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let mapping: GraphMapping = serde_json::from_str(
            r#"{"version":"1","nodes":[{"label":"A"},{"label":"B"}],"edges":[{"type":"LINK","from":{"label":"A"},"to":{"label":"B"}}]}"#,
        )
        .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let params = BTreeMap::new();
        crate::execute(
            &db,
            "CREATE (a:A {entity_id:'a'}), (b:A {entity_id:'b'}), (c:B {entity_id:'c'}), (a)-[:LINK]->(c), (b)-[:LINK]->(c)",
            &params,
        )
        .unwrap();

        let txn = db.begin();
        let catalog = db.catalog().unwrap().unwrap();
        let budget = Budget::new(ExecutionLimits::default(), None);
        let vars = Vars::default();
        let context = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &vars,
            cache: None,
            param_vals: None,
            partition: None,
        };
        let plan = plan::plan(&parse("MATCH (n)-[:LINK]->() RETURN n").unwrap(), &context).unwrap();
        let Op::Match(matched) = &plan.parts[0].ops[0] else {
            panic!("expected match");
        };
        let owner_slot = plan.vars.slot("n").unwrap();
        let a = store::find_by_entity_id(&txn, "a", None).unwrap()[0];
        let b = store::find_by_entity_id(&txn, "b", None).unwrap()[0];
        let context = Context {
            vars: &plan.vars,
            partition: Some((owner_slot, b, NodeId(b.0 + 1))),
            ..context
        };
        let mut owners = Vec::new();
        finish(run_match(
            &context,
            matched,
            vec![Val::Null; plan.vars.len()],
            &mut |row| {
                owners.push(row[owner_slot].clone());
                Ok(())
            },
        ))
        .unwrap();
        assert_ne!(a, b);
        assert_eq!(owners, vec![Val::Node(b)]);
    }

    fn projection(distinct: bool, aggregate: Option<AggPlan>) -> ProjectPlan {
        ProjectPlan {
            items: vec![(Expr::Param("payload".into()), 0)],
            aggregate,
            distinct,
            order: vec![],
            skip: None,
            limit: None,
            where_: None,
            presorted: false,
        }
    }

    #[test]
    fn distinct_refusal_does_not_store_an_unadmitted_key() {
        with_projection_context(24 << 10, |ctx| {
            let plan = projection(true, None);
            let mut acc = Accumulator::new(&plan, ctx).unwrap();
            assert!(matches!(
                acc.push(ctx, vec![Val::Null]),
                Err(Flow::Error(ExecError::MemoryLimit { .. }))
            ));
            assert!(
                acc.distinct.is_empty(),
                "refused DISTINCT retained its deep-cloned key"
            );
            assert!(acc.rows.is_empty());
        });
    }

    #[test]
    fn aggregate_distinct_refusal_does_not_store_an_unadmitted_key() {
        with_projection_context(24 << 10, |ctx| {
            let plan = projection(
                false,
                Some(AggPlan {
                    keys: vec![],
                    aggs: vec![AggSpec {
                        func: "count".into(),
                        distinct: true,
                        arg: Some(Expr::Param("payload".into())),
                        fraction: None,
                        slot: 0,
                    }],
                }),
            );
            let mut acc = Accumulator::new(&plan, ctx).unwrap();
            assert!(matches!(
                acc.push(ctx, vec![Val::Null]),
                Err(Flow::Error(ExecError::MemoryLimit { .. }))
            ));
            assert!(
                acc.group_list[0].seen[0].as_ref().unwrap().is_empty(),
                "refused aggregate DISTINCT retained its deep-cloned key"
            );
            assert!(matches!(acc.group_list[0].states[0], AggState::Count(0)));
        });
    }

    #[test]
    fn grouping_admits_both_retained_key_copies() {
        with_projection_context(40 << 10, |ctx| {
            let plan = projection(
                false,
                Some(AggPlan {
                    keys: vec![0],
                    aggs: vec![AggSpec {
                        func: "count".into(),
                        distinct: false,
                        arg: None,
                        fraction: None,
                        slot: 1,
                    }],
                }),
            );
            let mut acc = Accumulator::new(&plan, ctx).unwrap();
            assert!(matches!(
                acc.push(ctx, vec![Val::Null; 2]),
                Err(Flow::Error(ExecError::MemoryLimit { .. }))
            ));
            assert!(acc.groups.is_empty());
            assert!(acc.group_list.is_empty());
        });
    }

    #[test]
    fn duplicate_distinct_inputs_do_not_reserve_again() {
        for aggregate in [false, true] {
            with_projection_context(96 << 10, |ctx| {
                let agg = aggregate.then(|| AggPlan {
                    keys: vec![],
                    aggs: vec![AggSpec {
                        func: "count".into(),
                        distinct: true,
                        arg: Some(Expr::Param("payload".into())),
                        fraction: None,
                        slot: 0,
                    }],
                });
                let plan = projection(!aggregate, agg);
                let mut acc = Accumulator::new(&plan, ctx).unwrap();
                assert!(acc.push(ctx, vec![Val::Null]).is_ok());
                let remaining = ctx.budget.remaining();
                for _ in 0..3 {
                    assert!(acc.push(ctx, vec![Val::Null]).is_ok());
                }
                assert_eq!(ctx.budget.remaining(), remaining);
                if aggregate {
                    assert!(matches!(acc.group_list[0].states[0], AggState::Count(1)));
                } else {
                    assert_eq!(acc.rows.len(), 1);
                }
            });
        }
    }

    fn run_union_with_budget(query: &str, limit: usize) -> (Result<usize, ExecError>, usize) {
        use brainwire_store::db::{Database, Resources, StoreConfig};
        let directory = tempfile::tempdir().unwrap();
        let db =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let mut txn = db.begin();
        let catalog = Catalog {
            version: 1,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let params = BTreeMap::from([
            ("left".into(), Value::Str("x".repeat(16 << 10))),
            ("right".into(), Value::Str("y".repeat(16 << 10))),
        ]);
        let vars = Vars::default();
        let param_vals = BTreeMap::from([
            ("left".into(), Val::Str("x".repeat(16 << 10))),
            ("right".into(), Val::Str("y".repeat(16 << 10))),
        ]);
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: limit,
                timeout: None,
            },
            None,
        );
        let context = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &vars,
            cache: None,
            param_vals: Some(&param_vals),
            partition: None,
        };
        let plan = plan::plan(&parse(query).unwrap(), &context).unwrap();
        let env = Env {
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &plan.vars,
            cache: NodeCache::default(),
            param_vals: &param_vals,
        };
        let result = execute_rows(&mut txn, &env, &plan).map(|(_, rows, _)| rows.len());
        (result, budget.high_water())
    }

    #[test]
    fn union_distinct_admits_its_retained_key_copies() {
        let all = "RETURN $left AS x UNION ALL RETURN $right AS x";
        let unique = "RETURN $left AS x UNION RETURN $right AS x";
        let (result, peak) = run_union_with_budget(all, 1 << 20);
        assert_eq!(result.unwrap(), 2);
        let limit = peak + (8 << 10);
        assert_eq!(run_union_with_budget(all, limit).0.unwrap(), 2);
        assert!(matches!(
            run_union_with_budget(unique, limit).0,
            Err(ExecError::MemoryLimit { .. })
        ));
    }

    #[test]
    fn terminal_projection_moves_unique_payloads_and_only_clones_repeated_slots() {
        let payload = "payload".repeat(1024);
        let pointer = payload.as_ptr();
        let projected = project_output(
            vec![
                Val::Int(1),
                Val::Str(payload),
                Val::Str("unreturned".into()),
            ],
            &output_slots(&[1, 0, 1]),
        );
        assert_eq!(projected[1], Val::Int(1));
        let (Val::Str(first), Val::Str(last)) = (&projected[0], &projected[2]) else {
            panic!("expected string columns")
        };
        assert_eq!(first, last);
        assert_eq!(
            last.as_ptr(),
            pointer,
            "last occurrence must retain original allocation"
        );
        assert_ne!(
            first.as_ptr(),
            pointer,
            "earlier occurrence needs its own allocation"
        );
    }

    fn tiny_budget() -> Budget<'static> {
        Budget::new(
            ExecutionLimits {
                memory_bytes: 1,
                timeout: None,
            },
            None,
        )
    }

    #[test]
    fn cache_reuses_capacity_and_skipped_growth_keeps_previous_value() {
        let cache = NodeCache::default();
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 2500,
                timeout: None,
            },
            None,
        );
        cache_node_value(&cache, &budget, NodeId(1), &[7; 1024]).unwrap();
        let remaining = budget.remaining();
        cache_node_value(&cache, &budget, NodeId(2), &[8; 512]).unwrap();
        assert_eq!(budget.remaining(), remaining);
        cache_node_value(&cache, &budget, NodeId(3), &[9; 2048]).unwrap();
        assert_eq!(budget.remaining(), remaining);
        let slot = cache.0.borrow();
        let (id, bytes) = slot.as_ref().unwrap();
        assert_eq!(*id, NodeId(2));
        assert_eq!(bytes, &[8; 512]);
        assert_eq!(bytes.capacity(), 1024);
        drop(slot);
        cache_node_value(&cache, &budget, NodeId(2), &[9; 2048]).unwrap();
        assert!(cache.0.borrow().is_none());
        assert_eq!(budget.remaining(), remaining);
    }

    #[test]
    fn denied_large_scan_values_are_not_copied_into_the_cache() {
        use brainwire_core::{acl::ReadVisibility, schema::GraphMapping};
        use brainwire_store::db::{Database, Resources, StoreConfig};
        let directory = tempfile::tempdir().unwrap();
        let db =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let mapping: GraphMapping = serde_json::from_str(
            r#"{"version":"1","nodes":[{"label":"Doc","properties":["payload"]}],"edges":[]}"#,
        )
        .unwrap();
        db.apply_ontology(&mapping).unwrap();
        crate::execute(
            &db,
            "CREATE (n:Doc {entity_id:'hidden',payload:$payload})",
            &BTreeMap::from([("payload".into(), Value::Str("x".repeat(1024 * 1024)))]),
        )
        .unwrap();
        let txn = db.begin();
        let catalog = db.catalog().unwrap().unwrap();
        let params = BTreeMap::new();
        let vars = Vars::default();
        let denied = ReadVisibility {
            deny_all: true,
            ..Default::default()
        };
        let cache = NodeCache::default();
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 128 * 1024 * 1024,
                timeout: None,
            },
            None,
        );
        let context = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: Some(&denied),
            checkpoint_dir: None,
            vars: &vars,
            cache: Some(&cache),
            param_vals: None,
            partition: None,
        };
        let step = NodeStep {
            slot: 0,
            bound: false,
            labels: vec!["Doc".into()],
            props: vec![],
            access: Access::Scan,
        };
        let mut emitted = 0;
        finish(anchor(&context, &step, vec![Val::Null], &mut |_| {
            emitted += 1;
            Ok(())
        }))
        .unwrap();
        assert_eq!(emitted, 0);
        assert!(cache.0.borrow().is_none());

        let small_cache = NodeCache::default();
        let small_budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 64 * 1024,
                timeout: None,
            },
            None,
        );
        let context = Context {
            budget: &small_budget,
            visibility: None,
            cache: Some(&small_cache),
            ..context
        };
        let mut counted = 0;
        finish(anchor(&context, &step, vec![Val::Null], &mut |_| {
            counted += 1;
            Ok(())
        }))
        .unwrap();
        assert_eq!(counted, 1);
        assert!(small_cache.0.borrow().is_none());
        let id = store::find_by_entity_id(&txn, "hidden", None).unwrap()[0];
        assert_eq!(
            graph::node_property(&context, id, "entity_id").unwrap(),
            Value::Str("hidden".into())
        );
        assert!(matches!(
            graph::node_property(&context, id, "payload"),
            Err(ExecError::MemoryLimit { .. })
        ));
    }

    #[test]
    fn collect_and_numeric_merge_reject_growth_before_mutating_state() {
        let mut collected = AggState::Collect(vec![Val::Int(1)]);
        assert!(matches!(
            merge_state(
                &mut collected,
                AggState::Collect(vec![Val::Str("payload".into())]),
                &tiny_budget()
            ),
            Err(ExecError::MemoryLimit { .. })
        ));
        assert!(matches!(collected, AggState::Collect(ref values) if values.len() == 1));
        let mut numeric = AggState::Stdev(vec![1.0]);
        assert!(matches!(
            merge_state(&mut numeric, AggState::Stdev(vec![2.5]), &tiny_budget()),
            Err(ExecError::MemoryLimit { .. })
        ));
        assert!(matches!(numeric, AggState::Stdev(ref values) if values == &[1.0]));
        let mut discrete = AggState::Discrete(vec![Val::Int(9007199254740993)]);
        assert!(matches!(
            merge_state(
                &mut discrete,
                AggState::Discrete(vec![Val::Float(1.5)]),
                &tiny_budget()
            ),
            Err(ExecError::MemoryLimit { .. })
        ));
        assert!(
            matches!(discrete, AggState::Discrete(ref values) if matches!(values.as_slice(), [Val::Int(9007199254740993)]))
        );
    }

    #[test]
    fn merge_keeps_collect_order_and_numeric_values_with_budget() {
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 4096,
                timeout: None,
            },
            None,
        );
        let mut collected = AggState::Collect(vec![Val::Int(1)]);
        merge_state(
            &mut collected,
            AggState::Collect(vec![Val::Int(2)]),
            &budget,
        )
        .unwrap();
        assert!(
            matches!(collected, AggState::Collect(ref values) if matches!(values.as_slice(), [Val::Int(1), Val::Int(2)]))
        );
        let mut numeric = AggState::Stdev(vec![1.0]);
        merge_state(&mut numeric, AggState::Stdev(vec![2.5]), &budget).unwrap();
        assert!(matches!(numeric, AggState::Stdev(ref values) if values == &[1.0, 2.5]));
        let mut discrete = AggState::Discrete(vec![Val::Int(9007199254740993)]);
        merge_state(
            &mut discrete,
            AggState::Discrete(vec![Val::Float(1.5)]),
            &budget,
        )
        .unwrap();
        assert!(
            matches!(discrete, AggState::Discrete(ref values) if matches!(values.as_slice(), [Val::Int(9007199254740993), Val::Float(1.5)]))
        );
    }
    #[test]
    fn discrete_merge_orders_equal_numeric_types_and_signed_zero() {
        for reverse in [false, true] {
            let budget = Budget::new(
                ExecutionLimits {
                    memory_bytes: 4096,
                    timeout: None,
                },
                None,
            );
            let mut left = vec![Val::Int(2), Val::Float(0.0)];
            let mut right = vec![Val::Float(2.0), Val::Float(-0.0)];
            if reverse {
                std::mem::swap(&mut left, &mut right);
            }
            let mut state = AggState::Discrete(left);
            merge_state(&mut state, AggState::Discrete(right), &budget).unwrap();
            let AggState::Discrete(mut values) = state else {
                unreachable!()
            };
            values.sort_unstable_by(discrete_cmp);
            assert!(
                matches!(values.as_slice(), [Val::Float(negative), Val::Float(positive), Val::Int(2), Val::Float(two)]
                if negative.to_bits() == (-0.0_f64).to_bits() && positive.to_bits() == 0.0_f64.to_bits() && *two == 2.0)
            );
        }
    }
}
