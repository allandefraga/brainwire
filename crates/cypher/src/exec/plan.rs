//! Turn a parsed query into slot-addressed operators: resolve variables,
//! validate shapes before any row can disappear, choose how each pattern is
//! anchored (bound variable, entity id, index, label scan) and push WHERE
//! conjuncts down to the earliest step that binds their variables.
use super::*;
use std::collections::HashMap;

fn semantic(s: impl Into<String>) -> ExecError {
    ExecError::Semantic(s.into())
}
fn unsupported(s: impl Into<String>) -> ExecError {
    ExecError::Unsupported(s.into())
}

/// One slot per variable name; rows are `Vec<Val>` indexed by slot.
#[derive(Debug, Default)]
pub(crate) struct Vars {
    names: HashMap<String, usize>,
}

impl Vars {
    pub fn slot(&self, name: &str) -> Option<usize> {
        self.names.get(name).copied()
    }
    fn add(&mut self, name: &str) -> usize {
        let next = self.names.len();
        *self.names.entry(name.to_string()).or_insert(next)
    }
    pub fn len(&self) -> usize {
        self.names.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Node,
    Rel,
    /// A variable-length relationship variable: a list of relationships.
    RelList,
    Path,
    Any,
}

type Scope = BTreeMap<String, Kind>;

pub(super) struct Plan {
    pub parts: Vec<Part>,
    pub union_all: Vec<bool>,
    pub vars: Vars,
}

pub(super) struct Part {
    pub ops: Vec<Op>,
    pub columns: Vec<String>,
    /// Slots of the output columns, in order.
    pub outputs: Vec<usize>,
}

pub(super) enum Op {
    Match(MatchPlan),
    Unwind {
        expr: Expr,
        slot: usize,
    },
    Call {
        name: String,
        args: Vec<Expr>,
        yields: Vec<YieldItem>,
        slots: Vec<usize>,
        where_: Option<Expr>,
    },
    Project(Box<ProjectPlan>),
    Create(Vec<WritePattern>),
    Merge {
        pattern: WritePattern,
        on_create: Vec<SetPlan>,
        on_match: Vec<SetPlan>,
    },
    Set(Vec<SetPlan>),
    Delete {
        detach: bool,
        targets: Vec<Expr>,
    },
}

impl Op {
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Op::Create(_) | Op::Merge { .. } | Op::Set(_) | Op::Delete { .. }
        ) || matches!(self, Op::Call { name, .. } if name == "db.setNodeAcl" || name == "db.setNodeAcls")
    }
    /// Operators that need all of their input rows before producing output.
    pub fn blocking(&self) -> bool {
        match self {
            Op::Project(p) => p.blocking(),
            other => {
                other.is_write() || matches!(other, Op::Call { name, .. } if is_admin_call(name))
            }
        }
    }
}

/// Procedures that change the catalog or run outside the row stream.
pub(super) fn is_admin_call(name: &str) -> bool {
    name.starts_with("db.create") || name.starts_with("db.drop")
}

pub(super) struct MatchPlan {
    pub optional: bool,
    /// Conjuncts that only depend on rows bound before this clause.
    pub pre: Vec<Expr>,
    pub steps: Vec<Step>,
    /// Slots this clause binds; OPTIONAL MATCH sets them to null on no match.
    pub binds: Vec<usize>,
}

pub(super) struct Step {
    pub kind: StepKind,
    /// Conjuncts whose variables are all bound once this step has run.
    pub filters: Vec<Expr>,
}

pub(super) enum StepKind {
    Anchor(NodeStep),
    Expand(Expand),
    Path {
        slot: usize,
        nodes: Vec<usize>,
        rels: Vec<usize>,
    },
}

pub(super) struct NodeStep {
    pub slot: usize,
    pub bound: bool,
    pub labels: Vec<String>,
    pub props: Vec<(String, Expr)>,
    pub access: Access,
}

#[derive(Debug, Clone)]
pub(super) enum Access {
    Bound,
    Scan,
    EntityEq(Expr),
    EntityIn(Expr),
    /// Entity id order on one label: `>`/`>=` lower bound, `<`/`<=` upper bound, STARTS WITH.
    EntityRange {
        lower: Option<(Expr, bool)>,
        upper: Option<(Expr, bool)>,
        prefix: Option<Expr>,
    },
    IndexEq(String, Expr),
    IndexIn(String, Expr),
    IndexRange {
        field: String,
        lower: Option<(Expr, bool)>,
        upper: Option<(Expr, bool)>,
    },
    IndexPrefix(String, Expr),
    NodeId(Expr),
}

impl Access {
    fn score(&self) -> u8 {
        match self {
            Access::Bound => 0,
            Access::NodeId(_) | Access::EntityEq(_) => 1,
            Access::EntityIn(_) => 2,
            Access::IndexEq(..) => 3,
            Access::IndexIn(..) => 4,
            Access::EntityRange { .. } => 5,
            Access::IndexPrefix(..) | Access::IndexRange { .. } => 6,
            Access::Scan => 10,
        }
    }
}

pub(super) struct Expand {
    pub from: usize,
    pub rel: usize,
    pub rel_bound: bool,
    /// Empty means any type.
    pub types: Vec<brainwire_core::ids::EdgeTypeId>,
    /// Direction relative to `from`.
    pub direction: Direction,
    pub to: NodeStep,
    pub rel_props: Vec<(String, Expr)>,
    pub length: Option<(u32, Option<u32>)>,
    /// Expanding right to left: store var-length lists reversed back to pattern order.
    pub reversed: bool,
    /// Relationship slots bound earlier in this clause (relationship uniqueness).
    pub distinct_from: Vec<usize>,
}

pub(super) struct ProjectPlan {
    pub items: Vec<(Expr, usize)>,
    pub aggregate: Option<AggPlan>,
    pub distinct: bool,
    pub order: Vec<(Expr, bool)>,
    pub skip: Option<Expr>,
    pub limit: Option<Expr>,
    pub where_: Option<Expr>,
}

impl ProjectPlan {
    pub fn blocking(&self) -> bool {
        self.aggregate.is_some()
            || self.distinct
            || !self.order.is_empty()
            || self.skip.is_some()
            || self.limit.is_some()
    }
}

pub(super) struct AggPlan {
    /// Item indexes that are grouping keys.
    pub keys: Vec<usize>,
    pub aggs: Vec<AggSpec>,
}

pub(super) struct AggSpec {
    pub func: String,
    pub distinct: bool,
    /// `None` for `count(*)`.
    pub arg: Option<Expr>,
    pub slot: usize,
}

pub(super) struct WritePattern {
    pub nodes: Vec<WriteNode>,
    /// `rels[i]` joins `nodes[i]` and `nodes[i + 1]`.
    pub rels: Vec<WriteRel>,
    pub path: Option<usize>,
}

pub(super) struct WriteNode {
    pub slot: usize,
    pub bound: bool,
    pub label: Option<String>,
    pub props: Vec<(String, Expr)>,
}

pub(super) struct WriteRel {
    pub slot: usize,
    pub rel_type: String,
    pub direction: Direction,
    pub props: Vec<(String, Expr)>,
}

pub(super) enum SetPlan {
    Prop {
        slot: usize,
        prop: String,
        value: Expr,
    },
    Replace {
        slot: usize,
        value: Expr,
    },
    Merge {
        slot: usize,
        value: Expr,
    },
    Labels {
        slot: usize,
        labels: Vec<String>,
    },
}

struct Planner<'c, 'a, 'db> {
    ctx: &'c Context<'a, 'db>,
    vars: Vars,
    hidden: usize,
}

/// Variables an expression reads (excluding list-comprehension locals).
pub(super) fn free_vars(expr: &Expr, out: &mut BTreeSet<String>) {
    fn walk(expr: &Expr, locals: &mut Vec<String>, out: &mut BTreeSet<String>) {
        match expr {
            Expr::Var(name) => {
                if !locals.contains(name) {
                    out.insert(name.clone());
                }
            }
            Expr::MapProjection { var, items } => {
                if !locals.contains(var) {
                    out.insert(var.clone());
                }
                for item in items {
                    if let MapItem::Entry(_, e) = item {
                        walk(e, locals, out);
                    }
                }
            }
            Expr::ListComp {
                var,
                list,
                filter,
                map,
            } => {
                walk(list, locals, out);
                locals.push(var.clone());
                for e in filter.iter().chain(map.iter()) {
                    walk(e, locals, out);
                }
                locals.pop();
            }
            Expr::Quantifier {
                var,
                list,
                predicate,
                ..
            } => {
                walk(list, locals, out);
                locals.push(var.clone());
                walk(predicate, locals, out);
                locals.pop();
            }
            Expr::Reduce {
                acc,
                init,
                var,
                list,
                expr,
            } => {
                walk(init, locals, out);
                walk(list, locals, out);
                locals.push(acc.clone());
                locals.push(var.clone());
                walk(expr, locals, out);
                locals.pop();
                locals.pop();
            }
            Expr::Exists(pattern) => {
                let mut names = vec![];
                pattern_vars(pattern, &mut names);
                for name in names {
                    if !locals.contains(&name) {
                        out.insert(name);
                    }
                }
                for (_, e) in pattern_props(pattern) {
                    walk(e, locals, out);
                }
            }
            other => {
                for child in other.children() {
                    walk(child, locals, out);
                }
            }
        }
    }
    walk(expr, &mut vec![], out);
}

fn pattern_vars(pattern: &Pattern, out: &mut Vec<String>) {
    out.extend(pattern.start.var.clone());
    for (rel, node) in &pattern.hops {
        out.extend(rel.var.clone());
        out.extend(node.var.clone());
    }
}

fn pattern_props(pattern: &Pattern) -> Vec<&(String, Expr)> {
    let mut out: Vec<&(String, Expr)> = pattern.start.props.iter().collect();
    for (rel, node) in &pattern.hops {
        out.extend(rel.props.iter());
        out.extend(node.props.iter());
    }
    out
}

/// Local variables introduced inside expressions need slots too.
fn local_vars(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::ListComp { var, .. } | Expr::Quantifier { var, .. } => out.push(var.clone()),
        Expr::Reduce { acc, var, .. } => {
            out.push(acc.clone());
            out.push(var.clone());
        }
        Expr::Exists(pattern) => pattern_vars(pattern, out),
        _ => {}
    }
    for child in expr.children() {
        local_vars(child, out);
    }
}

fn conjuncts(expr: Expr, out: &mut Vec<Expr>) {
    match expr {
        Expr::And(a, b) => {
            conjuncts(*a, out);
            conjuncts(*b, out);
        }
        other => out.push(other),
    }
}

pub(super) fn plan(query: &Query, ctx: &Context<'_, '_>) -> Result<Plan, ExecError> {
    let mut planner = Planner {
        ctx,
        vars: Vars::default(),
        hidden: 0,
    };
    let mut parts = vec![planner.part(&query.clauses, query.unions.is_empty())?];
    let mut union_all = vec![];
    for (all, clauses) in &query.unions {
        let part = planner.part(clauses, false)?;
        if part.columns != parts[0].columns {
            return Err(semantic(
                "all UNION branches must return the same column names",
            ));
        }
        union_all.push(*all);
        parts.push(part);
    }
    if union_all.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(semantic("UNION and UNION ALL cannot be mixed in one query"));
    }
    if parts.len() > 1 && parts.iter().any(|p| p.ops.iter().any(Op::is_write)) {
        return Err(unsupported("writes inside UNION"));
    }
    Ok(Plan {
        parts,
        union_all,
        vars: planner.vars,
    })
}

impl Planner<'_, '_, '_> {
    fn hidden(&mut self, what: &str) -> usize {
        self.hidden += 1;
        let name = format!(" {what}{}", self.hidden);
        self.vars.add(&name)
    }

    fn expr(&mut self, e: &Expr, scope: &Scope, aggregates: bool) -> Result<(), ExecError> {
        let mut locals = vec![];
        local_vars(e, &mut locals);
        for name in &locals {
            self.vars.add(name);
        }
        let mut free = BTreeSet::new();
        free_vars(e, &mut free);
        for name in free {
            if !scope.contains_key(&name) {
                // Pattern predicates may introduce new anonymous-style names.
                if !pattern_names(e).contains(&name) {
                    return Err(semantic(format!("unknown variable {name}")));
                }
            }
        }
        if !aggregates && e.has_aggregate() {
            return Err(unsupported(
                "aggregates are only supported in WITH and RETURN items",
            ));
        }
        self.check_params(e)?;
        self.check_constants(e, false)
    }

    /// Evaluate row-independent subexpressions now, so type errors surface even
    /// when no row reaches them; boolean operands must be boolean or null.
    fn check_constants(&self, e: &Expr, boolean: bool) -> Result<(), ExecError> {
        let mut free = BTreeSet::new();
        free_vars(e, &mut free);
        let mut locals = vec![];
        local_vars(e, &mut locals);
        if free.is_empty() && locals.is_empty() && !e.has_aggregate() {
            let value = eval::eval(e, &[], self.ctx)?;
            if boolean {
                eval::boolean(value)?;
            }
            return Ok(());
        }
        let operands_boolean = matches!(
            e,
            Expr::And(..) | Expr::Or(..) | Expr::Xor(..) | Expr::Not(_)
        );
        for child in e.children() {
            if !matches!(
                e,
                Expr::ListComp { .. }
                    | Expr::Quantifier { .. }
                    | Expr::Reduce { .. }
                    | Expr::Case { .. }
            ) {
                self.check_constants(child, operands_boolean)?;
            }
        }
        Ok(())
    }

    fn predicate(&mut self, e: &Expr, scope: &Scope) -> Result<(), ExecError> {
        self.expr(e, scope, false)?;
        self.check_constants(e, true)
    }

    fn check_params(&self, e: &Expr) -> Result<(), ExecError> {
        if let Expr::Param(name) = e {
            if !self.ctx.params.contains_key(name) {
                return Err(ExecError::ParameterMissing(name.clone()));
            }
        }
        if let Expr::Exists(pattern) = e {
            for (_, value) in pattern_props(pattern) {
                self.check_params(value)?;
            }
        }
        // Lazy forms (coalesce, CASE) check parameters when they are reached.
        if matches!(e, Expr::Call { name, .. } if name == "coalesce")
            || matches!(e, Expr::Case { .. })
        {
            return Ok(());
        }
        for child in e.children() {
            self.check_params(child)?;
        }
        Ok(())
    }

    fn part(&mut self, clauses: &[Clause], single: bool) -> Result<Part, ExecError> {
        let mut scope = Scope::new();
        let mut ops = vec![];
        let mut columns = vec![];
        let mut outputs = vec![];
        let call_only = clauses.len() == 1 && matches!(clauses[0], Clause::Call { .. });
        for (index, clause) in clauses.iter().enumerate() {
            let last = index + 1 == clauses.len();
            match clause {
                Clause::Match {
                    optional,
                    patterns,
                    where_,
                } => ops.push(Op::Match(self.match_clause(
                    *optional,
                    patterns,
                    where_.as_ref(),
                    &mut scope,
                )?)),
                Clause::Unwind { expr, var } => {
                    self.expr(expr, &scope, false)?;
                    if scope.contains_key(var) {
                        return Err(semantic(format!("variable {var} is already bound")));
                    }
                    let slot = self.vars.add(var);
                    scope.insert(var.clone(), Kind::Any);
                    ops.push(Op::Unwind {
                        expr: expr.clone(),
                        slot,
                    });
                }
                Clause::Call {
                    name,
                    args,
                    yields,
                    where_,
                } => {
                    for argument in args {
                        self.expr(argument, &Scope::new(), false)?;
                    }
                    let introspection = matches!(
                        name.as_str(),
                        "db.labels" | "db.relationshipTypes" | "db.propertyKeys"
                    );
                    if !introspection && !is_admin_call(name) && index != 0 {
                        return Err(unsupported(format!(
                            "{name} must be the first clause of its query"
                        )));
                    }
                    if !introspection
                        && clauses.iter().any(|c| {
                            !matches!(
                                c,
                                Clause::Call { .. }
                                    | Clause::Return(_)
                                    | Clause::With(_)
                                    | Clause::Match { .. }
                                    | Clause::Unwind { .. }
                            )
                        })
                    {
                        return Err(unsupported(format!(
                            "{name} cannot share a query with writes"
                        )));
                    }
                    if call_only && !yields.is_empty() {
                        return Err(semantic(format!("CALL {name} YIELD needs a RETURN")));
                    }
                    let mut yields = yields.clone();
                    if yields.is_empty() && !is_admin_call(name) {
                        if !call_only {
                            return Err(semantic(format!(
                                "CALL {name} needs YIELD inside a larger query"
                            )));
                        }
                        yields = procedures::fields(name)?
                            .iter()
                            .map(|f| YieldItem {
                                field: f.to_string(),
                                alias: f.to_string(),
                            })
                            .collect();
                    }
                    if is_admin_call(name) {
                        if !call_only {
                            return Err(semantic(format!(
                                "{name} must be the only clause of its query"
                            )));
                        }
                    } else {
                        procedures::validate(name, args, &yields, self.ctx)?;
                    }
                    let mut slots = vec![];
                    for item in &yields {
                        if scope.contains_key(&item.alias) {
                            return Err(semantic(format!(
                                "variable {} is already bound",
                                item.alias
                            )));
                        }
                        slots.push(self.vars.add(&item.alias));
                        let node = item.field == "node"
                            || (item.field == "component"
                                && matches!(name.as_str(), "graph.wcc" | "graph.scc"));
                        scope.insert(
                            item.alias.clone(),
                            if node { Kind::Node } else { Kind::Any },
                        );
                    }
                    if let Some(predicate) = where_ {
                        self.predicate(predicate, &scope)?;
                    }
                    if call_only {
                        columns = yields.iter().map(|y| y.alias.clone()).collect();
                        outputs = slots.clone();
                    }
                    ops.push(Op::Call {
                        name: name.clone(),
                        args: args.clone(),
                        yields,
                        slots,
                        where_: where_.clone(),
                    });
                }
                Clause::With(projection) => {
                    let (plan, next, _, _) = self.projection(projection, &scope, false)?;
                    scope = next;
                    ops.push(Op::Project(Box::new(plan)));
                }
                Clause::Return(projection) => {
                    if !last {
                        return Err(unsupported("RETURN must be final"));
                    }
                    let (plan, _, names, slots) = self.projection(projection, &scope, true)?;
                    columns = names;
                    outputs = slots;
                    ops.push(Op::Project(Box::new(plan)));
                }
                Clause::Create(patterns) => {
                    let mut plans = vec![];
                    for pattern in patterns {
                        plans.push(self.write_pattern(pattern, &mut scope, true)?);
                    }
                    ops.push(Op::Create(plans));
                }
                Clause::Merge {
                    pattern,
                    on_create,
                    on_match,
                } => {
                    let pattern = self.write_pattern(pattern, &mut scope, false)?;
                    let on_create = self.set_items(on_create, &scope)?;
                    let on_match = self.set_items(on_match, &scope)?;
                    ops.push(Op::Merge {
                        pattern,
                        on_create,
                        on_match,
                    });
                }
                Clause::Set(items) | Clause::Remove(items) => {
                    let items = self.set_items(items, &scope)?;
                    ops.push(Op::Set(items));
                }
                Clause::Delete { detach, targets } => {
                    for target in targets {
                        self.expr(target, &scope, false)?;
                    }
                    ops.push(Op::Delete {
                        detach: *detach,
                        targets: targets.clone(),
                    });
                }
            }
        }
        let returns = matches!(clauses.last(), Some(Clause::Return(_)));
        let writes = ops.iter().any(Op::is_write);
        if !returns && !writes && !call_only {
            return Err(semantic(
                "a query must end with RETURN, a write or a procedure call",
            ));
        }
        if !single && !returns {
            return Err(semantic("every UNION branch must end with RETURN"));
        }
        Ok(Part {
            ops,
            columns,
            outputs,
        })
    }

    fn set_items(&mut self, items: &[SetItem], scope: &Scope) -> Result<Vec<SetPlan>, ExecError> {
        let mut out = vec![];
        for item in items {
            let (var, plan) = match item {
                SetItem::Prop { var, prop, value } => {
                    if prop == "entity_id" {
                        return Err(semantic("SET cannot change entity_id"));
                    }
                    self.expr(value, scope, false)?;
                    (
                        var,
                        SetPlan::Prop {
                            slot: 0,
                            prop: prop.clone(),
                            value: value.clone(),
                        },
                    )
                }
                SetItem::Replace { var, value } => {
                    self.expr(value, scope, false)?;
                    (
                        var,
                        SetPlan::Replace {
                            slot: 0,
                            value: value.clone(),
                        },
                    )
                }
                SetItem::Merge { var, value } => {
                    self.expr(value, scope, false)?;
                    (
                        var,
                        SetPlan::Merge {
                            slot: 0,
                            value: value.clone(),
                        },
                    )
                }
                SetItem::Labels { var, labels } => (
                    var,
                    SetPlan::Labels {
                        slot: 0,
                        labels: labels.clone(),
                    },
                ),
            };
            match scope.get(var) {
                Some(Kind::Node | Kind::Rel | Kind::Any) => {}
                _ => {
                    return Err(semantic(format!(
                        "{var} is not a bound node or relationship"
                    )))
                }
            }
            let slot = self.vars.slot(var).expect("scoped variable has a slot");
            out.push(match plan {
                SetPlan::Prop { prop, value, .. } => SetPlan::Prop { slot, prop, value },
                SetPlan::Replace { value, .. } => SetPlan::Replace { slot, value },
                SetPlan::Merge { value, .. } => SetPlan::Merge { slot, value },
                SetPlan::Labels { labels, .. } => SetPlan::Labels { slot, labels },
            });
        }
        Ok(out)
    }

    fn write_pattern(
        &mut self,
        pattern: &Pattern,
        scope: &mut Scope,
        create: bool,
    ) -> Result<WritePattern, ExecError> {
        let mut nodes = vec![];
        let mut rels = vec![];
        let node = |planner: &mut Self,
                    pat: &NodePat,
                    scope: &mut Scope|
         -> Result<WriteNode, ExecError> {
            let bound = pat.var.as_ref().is_some_and(|v| scope.contains_key(v));
            if bound {
                if !pat.labels.is_empty() || !pat.props.is_empty() {
                    if pattern.hops.is_empty() {
                        return Err(unsupported(
                            "node writes cannot rebind an existing variable",
                        ));
                    }
                    return Err(unsupported(
                        "bound nodes in a write pattern cannot repeat labels or properties",
                    ));
                }
                let var = pat.var.as_ref().expect("bound var");
                if scope.get(var) != Some(&Kind::Node) && scope.get(var) != Some(&Kind::Any) {
                    return Err(semantic(format!("{var} is not a bound node")));
                }
            } else {
                if pat.labels.len() != 1 {
                    return Err(unsupported("node writes need exactly one label"));
                }
                if planner.ctx.catalog.node_type(&pat.labels[0]).is_none() {
                    return Err(semantic(format!("unknown label {}", pat.labels[0])));
                }
                let mut names = BTreeSet::new();
                for (name, value) in &pat.props {
                    if !names.insert(name) {
                        return Err(semantic(format!("duplicate property {name}")));
                    }
                    planner.expr(value, scope, false)?;
                }
            }
            let slot = match &pat.var {
                Some(var) => planner.vars.add(var),
                None => planner.hidden("n"),
            };
            if let Some(var) = &pat.var {
                scope.insert(var.clone(), Kind::Node);
            }
            Ok(WriteNode {
                slot,
                bound,
                label: pat.labels.first().cloned(),
                props: pat.props.clone(),
            })
        };
        nodes.push(node(self, &pattern.start, scope)?);
        for (rel, end) in &pattern.hops {
            nodes.push(node(self, end, scope)?);
            if rel.types.len() != 1 {
                return Err(unsupported("relationship writes need exactly one type"));
            }
            if self.ctx.catalog.edge_type(&rel.types[0]).is_none() {
                return Err(semantic(format!("unknown edge type {}", rel.types[0])));
            }
            if rel.length.is_some() {
                return Err(unsupported("variable-length relationships in writes"));
            }
            if rel.direction == Direction::Both && create {
                return Err(semantic("CREATE needs a relationship direction"));
            }
            if rel.var.as_ref().is_some_and(|v| scope.contains_key(v)) {
                return Err(unsupported(
                    "relationship writes cannot rebind an existing variable",
                ));
            }
            for (_, value) in &rel.props {
                self.expr(value, scope, false)?;
            }
            let slot = match &rel.var {
                Some(var) => self.vars.add(var),
                None => self.hidden("r"),
            };
            if let Some(var) = &rel.var {
                scope.insert(var.clone(), Kind::Rel);
            }
            rels.push(WriteRel {
                slot,
                rel_type: rel.types[0].clone(),
                direction: rel.direction,
                props: rel.props.clone(),
            });
        }
        let path = pattern.path.as_ref().map(|name| {
            scope.insert(name.clone(), Kind::Path);
            self.vars.add(name)
        });
        Ok(WritePattern { nodes, rels, path })
    }

    #[allow(clippy::type_complexity)]
    fn projection(
        &mut self,
        projection: &Projection,
        scope: &Scope,
        is_return: bool,
    ) -> Result<(ProjectPlan, Scope, Vec<String>, Vec<usize>), ExecError> {
        let mut items: Vec<ReturnItem> = vec![];
        if projection.star {
            let visible: Vec<&String> = scope.keys().filter(|k| !k.starts_with(' ')).collect();
            if visible.is_empty() {
                return Err(semantic("RETURN * needs variables in scope"));
            }
            for name in visible {
                items.push(ReturnItem {
                    expr: Expr::Var(name.clone()),
                    alias: None,
                    text: name.clone(),
                });
            }
        }
        items.extend(projection.items.iter().cloned());
        let mut names = BTreeSet::new();
        for item in &items {
            if !is_return && item.alias.is_none() && !matches!(item.expr, Expr::Var(_)) {
                return Err(semantic(format!(
                    "expression in WITH must be aliased (use AS): {}",
                    item.text
                )));
            }
            // RETURN may repeat an identical column; it shares one slot.
            let repeated = is_return
                && items
                    .iter()
                    .take_while(|other| !std::ptr::eq(*other, item))
                    .any(|other| other.name() == item.name() && other.expr == item.expr);
            if !names.insert(item.name().to_string()) && !repeated {
                return Err(semantic(format!("duplicate column name {}", item.name())));
            }
            self.expr(&item.expr, scope, true)?;
        }
        let aggregating = items.iter().any(|i| i.expr.has_aggregate());
        let mut next = Scope::new();
        let mut plan_items = vec![];
        for item in &items {
            let kind = match &item.expr {
                Expr::Var(v) => scope.get(v).copied().unwrap_or(Kind::Any),
                _ => Kind::Any,
            };
            let slot = self.vars.add(item.name());
            next.insert(item.name().to_string(), kind);
            plan_items.push((item.expr.clone(), slot));
        }
        // ORDER BY sees the projected names, plus (without aggregation or
        // DISTINCT) every variable that was in scope before the projection.
        let mut order_scope = next.clone();
        if !aggregating && !projection.distinct {
            for (k, v) in scope {
                order_scope.entry(k.clone()).or_insert(*v);
            }
        }
        let mut aggregate = None;
        if aggregating {
            let mut aggs = vec![];
            let mut keys = vec![];
            for (index, (expr, _)) in plan_items.iter_mut().enumerate() {
                if expr.has_aggregate() {
                    *expr = self.extract_aggregates(expr.clone(), &mut aggs, scope)?;
                } else {
                    keys.push(index);
                }
            }
            aggregate = Some(AggPlan { keys, aggs });
        }
        let mut order = vec![];
        for sort in &projection.order {
            // An ORDER BY expression equal to an item sorts by that column.
            let by_item = items.iter().position(|i| {
                i.expr == sort.expr || matches!(&sort.expr, Expr::Var(v) if v == i.name())
            });
            let expr = match by_item {
                Some(index) => Expr::Var(items[index].name().to_string()),
                None if sort.expr.has_aggregate() => {
                    let Some(agg) = aggregate.as_mut() else {
                        unreachable!("aggregate ORDER BY implies an aggregating projection")
                    };
                    self.extract_aggregates(sort.expr.clone(), &mut agg.aggs, scope)?
                }
                None => {
                    self.expr(&sort.expr, &order_scope, false)?;
                    sort.expr.clone()
                }
            };
            if aggregating {
                let mut free = BTreeSet::new();
                free_vars(&expr, &mut free);
                if free
                    .iter()
                    .any(|v| !next.contains_key(v) && !v.starts_with(' '))
                {
                    return Err(semantic(
                        "ORDER BY after aggregation can only use projected columns or aggregates",
                    ));
                }
            }
            order.push((expr, sort.descending));
        }
        for (e, what) in [(&projection.skip, "SKIP"), (&projection.limit, "LIMIT")] {
            if let Some(e) = e {
                self.expr(e, &Scope::new(), false)
                    .map_err(|_| semantic(format!("{what} must be row-independent")))?;
                eval::limit(e, self.ctx)?;
            }
        }
        if let Some(predicate) = &projection.where_ {
            self.predicate(predicate, &order_scope)?;
        }
        let columns: Vec<String> = items.iter().map(|i| i.name().to_string()).collect();
        let outputs = plan_items.iter().map(|(_, slot)| *slot).collect();
        if !is_return {
            for name in next.keys() {
                if name.starts_with(' ') {
                    return Err(semantic("invalid variable name"));
                }
            }
        }
        Ok((
            ProjectPlan {
                items: plan_items,
                aggregate,
                distinct: projection.distinct,
                order,
                skip: projection.skip.clone(),
                limit: projection.limit.clone(),
                where_: projection.where_.clone(),
            },
            next,
            columns,
            outputs,
        ))
    }

    /// Replace each aggregate call with a hidden variable holding its result.
    fn extract_aggregates(
        &mut self,
        expr: Expr,
        aggs: &mut Vec<AggSpec>,
        scope: &Scope,
    ) -> Result<Expr, ExecError> {
        Ok(match expr {
            Expr::CountStar => {
                let slot = self.hidden("agg");
                aggs.push(AggSpec {
                    func: "count".into(),
                    distinct: false,
                    arg: None,
                    slot,
                });
                Expr::Var(self.slot_name(slot))
            }
            Expr::Call {
                name,
                distinct,
                mut args,
            } if crate::ast::AGGREGATES.contains(&name.as_str()) => {
                if args.len() != 1 {
                    return Err(semantic(format!("{name} takes one argument")));
                }
                let arg = args.remove(0);
                if arg.has_aggregate() {
                    return Err(semantic("aggregates cannot be nested"));
                }
                self.expr(&arg, scope, false)?;
                let slot = self.hidden("agg");
                aggs.push(AggSpec {
                    func: name,
                    distinct,
                    arg: Some(arg),
                    slot,
                });
                Expr::Var(self.slot_name(slot))
            }
            Expr::Var(name) => {
                return Err(semantic(format!(
                    "{name} must be a grouping key to be used next to an aggregate"
                )))
            }
            Expr::Prop(base, key) if !base.has_aggregate() => {
                return Err(semantic(format!(
                    "{}.{key} must be a grouping key to be used next to an aggregate",
                    match *base {
                        Expr::Var(name) => name,
                        _ => "expression".into(),
                    }
                )))
            }
            other => map_children(other, &mut |child| {
                self.extract_aggregates(child, aggs, scope)
            })?,
        })
    }

    fn slot_name(&self, slot: usize) -> String {
        self.vars
            .names
            .iter()
            .find(|(_, s)| **s == slot)
            .map(|(name, _)| name.clone())
            .expect("slot has a name")
    }

    fn match_clause(
        &mut self,
        optional: bool,
        patterns: &[Pattern],
        where_: Option<&Expr>,
        scope: &mut Scope,
    ) -> Result<MatchPlan, ExecError> {
        let before = scope.clone();
        // Bind every pattern variable into the scope first, checking kinds.
        for pattern in patterns {
            let declare =
                |name: &Option<String>, kind: Kind, scope: &mut Scope| -> Result<(), ExecError> {
                    if let Some(name) = name {
                        match scope.get(name) {
                            Some(existing) if *existing != kind && *existing != Kind::Any => {
                                return Err(semantic(format!(
                                    "{name} is already bound as a different kind"
                                )))
                            }
                            Some(_) => {}
                            None => {
                                scope.insert(name.clone(), kind);
                            }
                        }
                    }
                    Ok(())
                };
            declare(&pattern.start.var, Kind::Node, scope)?;
            for (rel, node) in &pattern.hops {
                let kind = if rel.length.is_some() {
                    Kind::RelList
                } else {
                    Kind::Rel
                };
                declare(&rel.var, kind, scope)?;
                declare(&node.var, Kind::Node, scope)?;
            }
            if let Some(path) = &pattern.path {
                if before.contains_key(path) {
                    return Err(semantic(format!("variable {path} is already bound")));
                }
                scope.insert(path.clone(), Kind::Path);
            }
        }
        let mut conjunct_list = vec![];
        if let Some(predicate) = where_ {
            self.predicate(predicate, scope)?;
            conjuncts(predicate.clone(), &mut conjunct_list);
        }
        let mut steps: Vec<Step> = vec![];
        let mut bound: BTreeSet<String> = before.keys().cloned().collect();
        let mut binds = vec![];
        let mut clause_rels: Vec<usize> = vec![];
        for pattern in patterns {
            self.pattern_steps(
                pattern,
                &before,
                &mut bound,
                &conjunct_list,
                &mut steps,
                &mut binds,
                &mut clause_rels,
            )?;
        }
        // Place each conjunct after the first step that binds all its variables.
        let mut pre = vec![];
        for conjunct in conjunct_list {
            let mut free = BTreeSet::new();
            free_vars(&conjunct, &mut free);
            let needed: Vec<usize> = free
                .iter()
                .filter(|v| !before.contains_key(*v))
                .filter_map(|v| self.vars.slot(v))
                .collect();
            let position = steps.iter().position(|_| true).and_then(|_| {
                let mut seen = BTreeSet::new();
                for (index, step) in steps.iter().enumerate() {
                    for slot in step_binds(step) {
                        seen.insert(slot);
                    }
                    if needed.iter().all(|s| seen.contains(s)) {
                        return Some(index);
                    }
                }
                None
            });
            match position {
                _ if needed.is_empty() => pre.push(conjunct),
                Some(index) => steps[index].filters.push(conjunct),
                None => steps
                    .last_mut()
                    .expect("a pattern step")
                    .filters
                    .push(conjunct),
            }
        }
        Ok(MatchPlan {
            optional,
            pre,
            steps,
            binds,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn pattern_steps(
        &mut self,
        pattern: &Pattern,
        before: &Scope,
        bound: &mut BTreeSet<String>,
        conjunct_list: &[Expr],
        steps: &mut Vec<Step>,
        binds: &mut Vec<usize>,
        clause_rels: &mut Vec<usize>,
    ) -> Result<(), ExecError> {
        let chain: Vec<&NodePat> = std::iter::once(&pattern.start)
            .chain(pattern.hops.iter().map(|(_, n)| n))
            .collect();
        for node in &chain {
            for label in &node.labels {
                if self.ctx.catalog.node_type(label).is_none() {
                    return Err(semantic(format!("unknown label {label}")));
                }
            }
            let mut names = BTreeSet::new();
            for (name, value) in &node.props {
                if !names.insert(name) {
                    return Err(semantic(format!("duplicate property {name}")));
                }
                self.expr(value, before, false).map_err(|e| match e {
                    ExecError::Semantic(m) if m.starts_with("unknown variable") => {
                        semantic(format!(
                            "{m}: pattern properties may only use variables bound before the MATCH"
                        ))
                    }
                    other => other,
                })?;
            }
        }
        let mut node_slots = vec![];
        for node in &chain {
            node_slots.push(match &node.var {
                Some(var) => self.vars.add(var),
                None => self.hidden("n"),
            });
        }
        let mut rel_slots = vec![];
        let mut types = vec![];
        for (rel, _) in &pattern.hops {
            rel_slots.push(match &rel.var {
                Some(var) => self.vars.add(var),
                None => self.hidden("r"),
            });
            let mut ids = vec![];
            for name in &rel.types {
                match self.ctx.catalog.edge_type(name) {
                    Some(ty) => ids.push(ty.id),
                    None => return Err(semantic(format!("unknown edge type {name}"))),
                }
            }
            types.push(ids);
            for (_, value) in &rel.props {
                self.expr(value, before, false)?;
            }
        }
        // Choose the anchor: the cheapest access among the chain's nodes.
        let mut best: Option<(u8, usize, Access)> = None;
        for (index, node) in chain.iter().enumerate() {
            let access = self.access(node, bound, before, conjunct_list);
            let score = access.score();
            if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
                best = Some((score, index, access));
            }
        }
        let (_, anchor, access) = best.expect("a pattern has a node");
        let is_bound = |node: &NodePat, bound: &BTreeSet<String>| {
            node.var.as_ref().is_some_and(|v| bound.contains(v))
        };
        let anchor_bound = is_bound(chain[anchor], bound);
        steps.push(Step {
            kind: StepKind::Anchor(NodeStep {
                slot: node_slots[anchor],
                bound: anchor_bound,
                labels: chain[anchor].labels.clone(),
                props: chain[anchor].props.clone(),
                access: if anchor_bound { Access::Bound } else { access },
            }),
            filters: vec![],
        });
        if !anchor_bound {
            binds.push(node_slots[anchor]);
        }
        if let Some(var) = &chain[anchor].var {
            bound.insert(var.clone());
        }
        // Expand right, then left, from the anchor.
        let order: Vec<(usize, usize, bool)> = (anchor..pattern.hops.len())
            .map(|hop| (hop, hop + 1, false))
            .chain((0..anchor).rev().map(|hop| (hop, hop, true)))
            .collect();
        for (hop, target, reversed) in order {
            let (rel, _) = &pattern.hops[hop];
            let from = if reversed {
                node_slots[hop + 1]
            } else {
                node_slots[hop]
            };
            let direction = match (rel.direction, reversed) {
                (Direction::Both, _) => Direction::Both,
                (Direction::Out, false) | (Direction::In, true) => Direction::Out,
                _ => Direction::In,
            };
            let node = chain[target];
            let to_bound = is_bound(node, bound);
            let rel_bound = rel.var.as_ref().is_some_and(|v| before.contains_key(v));
            if rel_bound && rel.length.is_some() {
                return Err(unsupported("a bound variable-length relationship"));
            }
            steps.push(Step {
                kind: StepKind::Expand(Expand {
                    from,
                    rel: rel_slots[hop],
                    rel_bound,
                    types: types[hop].clone(),
                    direction,
                    to: NodeStep {
                        slot: node_slots[target],
                        bound: to_bound,
                        labels: node.labels.clone(),
                        props: node.props.clone(),
                        access: Access::Bound,
                    },
                    rel_props: rel.props.clone(),
                    length: rel.length,
                    reversed,
                    distinct_from: clause_rels.clone(),
                }),
                filters: vec![],
            });
            clause_rels.push(rel_slots[hop]);
            if !rel_bound {
                binds.push(rel_slots[hop]);
            }
            if !to_bound {
                binds.push(node_slots[target]);
            }
            if let Some(var) = &node.var {
                bound.insert(var.clone());
            }
            if let Some(var) = &rel.var {
                bound.insert(var.clone());
            }
        }
        if let Some(path) = &pattern.path {
            let slot = self.vars.add(path);
            binds.push(slot);
            steps.push(Step {
                kind: StepKind::Path {
                    slot,
                    nodes: node_slots,
                    rels: rel_slots,
                },
                filters: vec![],
            });
            bound.insert(path.clone());
        }
        Ok(())
    }

    /// The best index or id access for one node pattern given the conjuncts.
    fn access(
        &self,
        node: &NodePat,
        bound: &BTreeSet<String>,
        before: &Scope,
        conjunct_list: &[Expr],
    ) -> Access {
        if node.var.as_ref().is_some_and(|v| bound.contains(v)) {
            return Access::Bound;
        }
        let ty = match node.labels.as_slice() {
            [label] => self.ctx.catalog.node_type(label),
            _ => None,
        };
        let indexed = |field: &str| ty.is_some_and(|ty| ty.indexed.iter().any(|f| f == field));
        let var = node.var.as_deref();
        // Values usable for lookups may only depend on earlier rows.
        let usable = |e: &Expr| {
            let mut free = BTreeSet::new();
            free_vars(e, &mut free);
            free.iter().all(|v| before.contains_key(v)) && !e.has_aggregate()
        };
        let is_prop = |e: &Expr| -> Option<String> {
            match e {
                Expr::Prop(base, field) if matches!(base.as_ref(), Expr::Var(name) if Some(name.as_str()) == var) => {
                    Some(field.clone())
                }
                _ => None,
            }
        };
        let is_id = |e: &Expr| {
            matches!(e, Expr::Call { name, args, .. }
            if (name == "id" || name == "elementid") && args.len() == 1 && matches!(&args[0], Expr::Var(n) if Some(n.as_str()) == var))
        };
        let mut best = Access::Scan;
        let consider = |candidate: Access, best: &mut Access| {
            if candidate.score() < best.score() {
                *best = candidate;
            }
        };
        let mut ranges: BTreeMap<String, RangeBounds> = BTreeMap::new();
        for (field, value) in &node.props {
            if !usable(value) {
                continue;
            }
            if field == "entity_id" && ty.is_some() {
                consider(Access::EntityEq(value.clone()), &mut best);
            } else if indexed(field) {
                consider(Access::IndexEq(field.clone(), value.clone()), &mut best);
            }
        }
        if var.is_some() {
            for conjunct in conjunct_list {
                match conjunct {
                    Expr::Cmp(a, op, b) => {
                        for (left, right, op) in [(a, b, *op), (b, a, flip(*op))] {
                            if !usable(right) {
                                continue;
                            }
                            if op == CmpOp::Eq && is_id(left) {
                                consider(
                                    Access::NodeId(Expr::Call {
                                        name: match left.as_ref() {
                                            Expr::Call { name, .. } => name.clone(),
                                            _ => unreachable!(),
                                        },
                                        distinct: false,
                                        args: vec![(**right).clone()],
                                    }),
                                    &mut best,
                                );
                                continue;
                            }
                            let Some(field) = is_prop(left) else { continue };
                            match op {
                                CmpOp::Eq if field == "entity_id" && ty.is_some() => {
                                    consider(Access::EntityEq((**right).clone()), &mut best)
                                }
                                CmpOp::Eq if indexed(&field) => {
                                    consider(Access::IndexEq(field, (**right).clone()), &mut best)
                                }
                                CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le
                                    if (field == "entity_id" && ty.is_some())
                                        || indexed(&field) =>
                                {
                                    let entry = ranges.entry(field).or_default();
                                    let bound =
                                        ((**right).clone(), matches!(op, CmpOp::Ge | CmpOp::Le));
                                    if matches!(op, CmpOp::Gt | CmpOp::Ge) {
                                        entry.0.get_or_insert(bound);
                                    } else {
                                        entry.1.get_or_insert(bound);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Expr::In(a, list) if usable(list) => {
                        if let Some(field) = is_prop(a) {
                            if field == "entity_id" && ty.is_some() {
                                consider(Access::EntityIn((**list).clone()), &mut best);
                            } else if indexed(&field) {
                                consider(Access::IndexIn(field, (**list).clone()), &mut best);
                            }
                        }
                    }
                    Expr::Text(a, TextOp::StartsWith, prefix) if usable(prefix) => {
                        if let Some(field) = is_prop(a) {
                            if field == "entity_id" && ty.is_some() {
                                consider(
                                    Access::EntityRange {
                                        lower: None,
                                        upper: None,
                                        prefix: Some((**prefix).clone()),
                                    },
                                    &mut best,
                                );
                            } else if indexed(&field) {
                                consider(Access::IndexPrefix(field, (**prefix).clone()), &mut best);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (field, (lower, upper)) in ranges {
            if field == "entity_id" {
                consider(
                    Access::EntityRange {
                        lower,
                        upper,
                        prefix: None,
                    },
                    &mut best,
                );
            } else {
                consider(
                    Access::IndexRange {
                        field,
                        lower,
                        upper,
                    },
                    &mut best,
                );
            }
        }
        best
    }
}

/// Lower and upper bound expressions (and whether each is inclusive).
type RangeBounds = (Option<(Expr, bool)>, Option<(Expr, bool)>);

fn flip(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

fn pattern_names(e: &Expr) -> Vec<String> {
    let mut out = vec![];
    fn walk(e: &Expr, out: &mut Vec<String>) {
        if let Expr::Exists(pattern) = e {
            pattern_vars(pattern, out);
        }
        for child in e.children() {
            walk(child, out);
        }
    }
    walk(e, &mut out);
    out
}

pub(super) fn step_binds(step: &Step) -> Vec<usize> {
    match &step.kind {
        StepKind::Anchor(node) => vec![node.slot],
        StepKind::Expand(expand) => vec![expand.rel, expand.to.slot],
        StepKind::Path { slot, .. } => vec![*slot],
    }
}

/// Rebuild an expression with each direct child transformed.
fn map_children(
    expr: Expr,
    f: &mut dyn FnMut(Expr) -> Result<Expr, ExecError>,
) -> Result<Expr, ExecError> {
    let mut b = |e: Box<Expr>| -> Result<Box<Expr>, ExecError> { Ok(Box::new(f(*e)?)) };
    Ok(match expr {
        Expr::Prop(e, k) => Expr::Prop(b(e)?, k),
        Expr::Neg(e) => Expr::Neg(b(e)?),
        Expr::Not(e) => Expr::Not(b(e)?),
        Expr::IsNull(e, n) => Expr::IsNull(b(e)?, n),
        Expr::HasLabels(e, l) => Expr::HasLabels(b(e)?, l),
        Expr::Cmp(x, op, y) => Expr::Cmp(b(x)?, op, b(y)?),
        Expr::Arith(x, op, y) => Expr::Arith(b(x)?, op, b(y)?),
        Expr::Text(x, op, y) => Expr::Text(b(x)?, op, b(y)?),
        Expr::In(x, y) => Expr::In(b(x)?, b(y)?),
        Expr::And(x, y) => Expr::And(b(x)?, b(y)?),
        Expr::Or(x, y) => Expr::Or(b(x)?, b(y)?),
        Expr::Xor(x, y) => Expr::Xor(b(x)?, b(y)?),
        Expr::Index(x, y) => Expr::Index(b(x)?, b(y)?),
        Expr::Slice(x, y, z) => Expr::Slice(
            b(x)?,
            y.map(&mut b).transpose()?,
            z.map(&mut b).transpose()?,
        ),
        Expr::Call {
            name,
            distinct,
            args,
        } => Expr::Call {
            name,
            distinct,
            args: args.into_iter().map(&mut *f).collect::<Result<_, _>>()?,
        },
        Expr::List(items) => Expr::List(items.into_iter().map(&mut *f).collect::<Result<_, _>>()?),
        Expr::Map(items) => Expr::Map(
            items
                .into_iter()
                .map(|(k, v)| Ok((k, f(v)?)))
                .collect::<Result<_, ExecError>>()?,
        ),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => Expr::Case {
            operand: operand.map(&mut b).transpose()?,
            whens: whens
                .into_iter()
                .map(|(w, t)| Ok((*b(Box::new(w))?, *b(Box::new(t))?)))
                .collect::<Result<_, ExecError>>()?,
            otherwise: otherwise.map(&mut b).transpose()?,
        },
        other => other,
    })
}
