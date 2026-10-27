//! Execute a plan. Each query part runs as segments of streaming operators
//! (MATCH, UNWIND, read procedures, non-blocking WITH) separated by blocking
//! operators: aggregating/sorting/limited projections and writes, which see
//! every row of their segment before the next segment starts. LIMIT without
//! ORDER BY stops the underlying scans as soon as enough rows exist.
use super::eval::{eval, predicate};
use super::plan::*;
use super::val::{Key, Val};
use super::*;
use brainwire_core::ids::TypeId;
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
type Emit<'e> = dyn FnMut(Row) -> Result<(), Flow> + 'e;

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
    fn ctx<'t, 'db>(&'t self, txn: &'t Txn<'db>) -> Context<'t, 'db> {
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
        }
    }
}

pub(super) fn execute(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    plan: &Plan,
) -> Result<(QueryOutput, bool), ExecError> {
    let mut wrote = false;
    let mut rows: Vec<Row> = vec![];
    let mut seen: Option<HashSet<Key>> = None;
    for (index, part) in plan.parts.iter().enumerate() {
        let distinct = index > 0 && !plan.union_all[index - 1];
        if distinct && seen.is_none() {
            // A plain UNION deduplicates everything, including the first branch.
            let mut set = HashSet::new();
            rows.retain(|row| set.insert(Key(row.clone())));
            seen = Some(set);
        }
        let produced = run_part(txn, env, part, &mut wrote)?;
        if part.columns.is_empty() {
            continue;
        }
        for row in produced {
            let projected: Row = part.outputs.iter().map(|slot| row[*slot].clone()).collect();
            if let Some(set) = seen.as_mut() {
                if !set.insert(Key(projected.clone())) {
                    continue;
                }
            }
            rows.push(projected);
        }
    }
    let ctx = env.ctx(txn);
    let mut output = QueryOutput {
        fields: plan.parts[0].columns.clone(),
        rows: Vec::with_capacity(rows.len()),
    };
    for row in rows {
        let row: Vec<Out> = row
            .into_iter()
            .map(|value| graph::out(&ctx, value))
            .collect::<Result<_, _>>()?;
        env.budget.output(&row)?;
        output.rows.push(row);
    }
    env.budget.check_deadline()?;
    Ok((output, wrote))
}

fn run_part(
    txn: &mut Txn<'_>,
    env: &Env<'_>,
    part: &Part,
    wrote: &mut bool,
) -> Result<Vec<Row>, ExecError> {
    let ops = &part.ops;
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
                let mut rows = vec![];
                for row in input {
                    finish(stream(&ctx, segment, row, &mut |row| {
                        env.budget.retain_row(&row)?;
                        rows.push(row);
                        Ok(())
                    }))?;
                }
                return Ok(rows);
            }
            Some(Op::Project(projection)) => {
                let ctx = env.ctx(txn);
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
            return Ok(input);
        }
    }
}

/// Run streaming operators for one row, calling `emit` for each result.
fn stream(ctx: &Context<'_, '_>, ops: &[Op], row: Row, emit: &mut Emit<'_>) -> Result<(), Flow> {
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
    /// Numbers for stdev and percentiles; whether every input was an integer.
    Stdev(Vec<f64>, bool),
}

struct Group {
    key: Vec<Val>,
    states: Vec<AggState>,
    seen: Vec<Option<HashSet<Key>>>,
}

struct Accumulator<'p> {
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
    fn new(plan: &'p ProjectPlan, ctx: &Context<'_, '_>) -> Result<Self, ExecError> {
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

    fn push(&mut self, ctx: &Context<'_, '_>, row: Row) -> Result<(), Flow> {
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
                    ctx.budget.retain(
                        lookup.0.iter().map(Val::bytes).sum::<usize>()
                            + 128 * agg.aggs.len().max(1),
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
                let value = match &spec.arg {
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
                    if !seen.insert(Key(vec![value.clone()])) {
                        continue;
                    }
                    ctx.budget.retain(value.bytes())?;
                }
                update(state, value, ctx)?;
            }
            return Ok(());
        }
        let values = self
            .plan
            .items
            .iter()
            .map(|(expr, _)| eval(expr, &row, ctx))
            .collect::<Result<Vec<_>, _>>()?;
        if self.plan.distinct && !self.distinct.insert(Key(values.clone())) {
            return Ok(());
        }
        if self.plan.distinct {
            ctx.budget.retain(values.iter().map(Val::bytes).sum())?;
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

    fn trim(&mut self, ctx: &Context<'_, '_>, keep: usize) {
        eval::order_rows(&mut self.rows, &self.descending);
        for (order, row) in self.rows.drain(keep.min(self.rows.len())..) {
            ctx.budget.release_row(&order);
            ctx.budget.release_row(&row);
        }
    }

    fn finish(mut self, ctx: &Context<'_, '_>) -> Result<Vec<Row>, ExecError> {
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
        _ => AggState::Stdev(vec![], true),
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
        AggState::Stdev(items, ints) => {
            ctx.budget.retain(8)?;
            *ints &= matches!(value, Val::Int(_));
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
        AggState::Stdev(mut items, ints) => match spec.func.as_str() {
            "percentilecont" | "percentiledisc" => {
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
                if items.is_empty() {
                    return Ok(Val::Null);
                }
                items.sort_by(f64::total_cmp);
                let last = (items.len() - 1) as f64;
                if spec.func == "percentiledisc" {
                    let index = ((fraction * items.len() as f64).ceil() as usize).saturating_sub(1);
                    let value = items[index.min(items.len() - 1)];
                    // percentileDisc returns one of its inputs, keeping integers.
                    if ints {
                        Val::Int(value as i64)
                    } else {
                        Val::Float(value)
                    }
                } else {
                    let position = fraction * last;
                    let (low, high) = (position.floor() as usize, position.ceil() as usize);
                    Val::Float(items[low] + (items[high] - items[low]) * (position - low as f64))
                }
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
    let mut next = |row: Row| -> Result<(), Flow> {
        if filters_pass(ctx, step, &row)? {
            steps(ctx, rest, row, emit)
        } else {
            Ok(())
        }
    };
    match &step.kind {
        StepKind::Anchor(node) => {
            if let Some(scan) = label_edge_scan(ctx, node, step, rest) {
                return scan_edges(ctx, node, scan, row, rest, emit);
            }
            anchor(ctx, node, row, &mut next)
        }
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

/// A label scan immediately expanded over one direction needs no node reads:
/// stream the adjacency column family across the label's id range instead.
fn label_edge_scan<'s>(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    step: &Step,
    rest: &'s [Step],
) -> Option<&'s Expand> {
    if ctx.visibility.is_some()
        || node.bound
        || !matches!(node.access, Access::Scan)
        || node.labels.len() != 1
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
                && expand.direction != Direction::Both =>
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
    let ty = ctx
        .catalog
        .node_type(&node.labels[0])
        .expect("validated label");
    let cf = if expand.direction == Direction::Out {
        Cf::Out
    } else {
        Cf::In
    };
    let (expand_step, after) = rest.split_first().expect("expand step follows");
    store::visit_edges_of_label::<Flow>(ctx.txn, cf, ty.id, &expand.types, |owner, edge| {
        let _scope = ctx.budget.scope();
        ctx.budget.charge(16)?;
        let mut row = row.clone();
        row[node.slot] = Val::Node(owner);
        try_edge(ctx, expand, &row, owner, edge, cf, &mut |row| {
            if filters_pass(ctx, expand_step, &row)? {
                steps(ctx, after, row, emit)
            } else {
                Ok(())
            }
        })
    })
}

fn node_ok(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    id: NodeId,
    row: &[Val],
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

fn anchor(
    ctx: &Context<'_, '_>,
    node: &NodeStep,
    row: Row,
    next: &mut Emit<'_>,
) -> Result<(), Flow> {
    let mut candidate = |id: NodeId| -> Result<(), Flow> {
        let _scope = ctx.budget.scope();
        ctx.budget.charge(16)?;
        if !node_ok(ctx, node, id, &row)? {
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
    let scan = |candidate: &mut dyn FnMut(NodeId) -> Result<(), Flow>| -> Result<(), Flow> {
        let types: Vec<TypeId> = match ty {
            Some(ty) => vec![ty.id],
            None => ctx.catalog.node_types.iter().map(|t| t.id).collect(),
        };
        for ty in types {
            store::visit_nodes_of_type_with_values::<Flow>(ctx.txn, ty, |id, bytes| {
                if let Some(cache) = ctx.cache {
                    let mut slot = cache.0.borrow_mut();
                    match slot.as_mut() {
                        Some((cached, buffer)) => {
                            *cached = id;
                            buffer.clear();
                            buffer.extend_from_slice(bytes);
                        }
                        None => *slot = Some((id, bytes.to_vec())),
                    }
                }
                candidate(id)
            })?;
        }
        Ok(())
    };
    match &node.access {
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
                    candidate(id)?;
                }
            }
            Ok(())
        }
        Access::EntityIn(expr) => {
            let ty = ty.expect("entity access has a label");
            let Val::List(items) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let mut ids = BTreeSet::new();
            for item in items {
                if let Val::Str(entity_id) = item {
                    ids.extend(store::find_by_entity_id(ctx.txn, &entity_id, Some(ty.id))?);
                }
            }
            for id in ids {
                candidate(id)?;
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
                    candidate(id)?;
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
            if !store::visit_by_indexed_property::<Flow>(
                ctx.txn,
                ty,
                field,
                &value,
                &mut candidate,
            )? {
                return scan(&mut candidate);
            }
            Ok(())
        }
        Access::IndexIn(field, expr) => {
            let ty = ty.expect("index access has a label");
            let Val::List(items) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let mut values = HashSet::new();
            let mut ids = BTreeSet::new();
            for item in items {
                if item.is_null() || !values.insert(Key(vec![item.clone()])) {
                    continue;
                }
                let value = item.into_value()?;
                let ready =
                    store::visit_by_indexed_property::<Flow>(ctx.txn, ty, field, &value, |id| {
                        ids.insert(id);
                        Ok(())
                    })?;
                if !ready {
                    return scan(&mut candidate);
                }
            }
            for id in ids {
                candidate(id)?;
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
            let mut ids = BTreeSet::new();
            let ready = store::visit_index_range::<Flow>(
                ctx.txn,
                ty,
                field,
                lower.as_ref(),
                upper.as_ref(),
                None,
                |id| {
                    ids.insert(id);
                    Ok(())
                },
            )?;
            if !ready {
                return scan(&mut candidate);
            }
            for id in ids {
                candidate(id)?;
            }
            Ok(())
        }
        Access::IndexPrefix(field, expr) => {
            let ty = ty.expect("index access has a label");
            let Val::Str(prefix) = eval(expr, &row, ctx)? else {
                return Ok(());
            };
            let mut ids = BTreeSet::new();
            let ready = store::visit_index_range::<Flow>(
                ctx.txn,
                ty,
                field,
                None,
                None,
                Some(&prefix),
                |id| {
                    ids.insert(id);
                    Ok(())
                },
            )?;
            if !ready {
                return scan(&mut candidate);
            }
            for id in ids {
                candidate(id)?;
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
            candidate(id)
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
            .into())
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
            if ok && (expand.types.is_empty() || expand.types.contains(&rel.edge_type)) {
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
    if max.is_some_and(|max| depth >= max) {
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
        rel.types
            .iter()
            .map(|name| {
                ctx.catalog
                    .edge_type(name)
                    .map(|t| t.id)
                    .ok_or_else(|| ExecError::Semantic(format!("unknown edge type {name}")))
            })
            .collect()
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
            if max.is_some_and(|m| depth >= m) || depth > 64 {
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
