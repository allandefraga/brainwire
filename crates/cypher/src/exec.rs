//! Atomic Cypher execution with bounded row-at-a-time read pipelines.
//!
//! A query transaction owns one snapshot and write overlay across its runs.
//! All output is evaluated before commit; any query error discards every staged
//! graph write. `execute` provides the same atomic behavior for one query.
//! Read-only MATCH/UNWIND pipelines stream into bounded output or simple counts.
//! Writes, grouped aggregates and CALL remain eager; Bolt output is materialized.
//! Conservative guards and an optional shared pool are not process RSS caps.
mod budget;
mod eval;
use budget::Budget;
pub use budget::{ExecutionLimits, QueryMemoryPool, QueryMemoryUsage};
mod graph;
mod procedures;
mod stream;
mod validate;

use crate::{ast::*, parse, ParseError};
use brainwire_core::{ids::NodeId, schema::Catalog, value::Value};
use brainwire_store::{
    db::{Database, StoreError},
    graph::GraphError,
    txn::{Cf, Txn, TxnError},
};
use eval::{eval, scalar};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub struct QueryOutput {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<Out>>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Value(Value),
    Node(NodeOut),
}
#[derive(Debug, Clone, PartialEq)]
pub struct NodeOut {
    pub id: NodeId,
    pub label: String,
    pub props: BTreeMap<String, Value>,
}
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Syntax(#[from] ParseError),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("semantic error: {0}")]
    Semantic(String),
    #[error("missing parameter: {0}")]
    ParameterMissing(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    #[error("query allocation guard exceeded: attempted {attempted} bytes, limit {limit} bytes")]
    MemoryLimit { limit: usize, attempted: usize },
    #[error("query execution deadline exceeded")]
    TimedOut,
    #[error("shared query memory pool exhausted: requested {requested} additional bytes, {reserved} reserved, limit {limit}")]
    MemoryPoolExhausted {
        limit: usize,
        reserved: usize,
        requested: usize,
    },
    #[error("transaction conflict")]
    Conflict,
    #[error(transparent)]
    Graph(GraphError),
}
impl From<GraphError> for ExecError {
    fn from(e: GraphError) -> Self {
        match e {
            GraphError::Txn(TxnError::Conflict) => Self::Conflict,
            GraphError::UnknownLabel(s) | GraphError::UnknownEdgeType(s) => Self::Semantic(s),
            e => Self::Graph(e),
        }
    }
}
impl From<TxnError> for ExecError {
    fn from(e: TxnError) -> Self {
        Self::from(GraphError::Txn(e))
    }
}
impl From<StoreError> for ExecError {
    fn from(e: StoreError) -> Self {
        Self::Graph(GraphError::Store(e))
    }
}
#[derive(Debug, Clone, PartialEq)]
enum Binding {
    Node(NodeId),
    Value(Value),
}
type Bindings = BTreeMap<String, Binding>;
struct Context<'a, 'db> {
    txn: &'a Txn<'db>,
    catalog: &'a Catalog,
    params: &'a BTreeMap<String, Value>,
    budget: &'a Budget<'a>,
}

/// Lifecycle of a shared query transaction. Terminal states cannot be reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTransactionState {
    Active,
    Failed,
    Committed,
    RolledBack,
}

/// Operation permissions applied to every clause before query execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryAccess {
    pub write: bool,
    pub instance_admin: bool,
}

impl QueryAccess {
    pub const UNRESTRICTED: Self = Self {
        write: true,
        instance_admin: true,
    };

    fn authorize(self, query: &Query) -> Result<(), ExecError> {
        for clause in &query.clauses {
            match clause {
                Clause::Create(_) | Clause::Merge(_) | Clause::Set(_) | Clause::DetachDelete(_) => {
                    if !self.write {
                        return Err(ExecError::Forbidden("graph write access required"));
                    }
                }
                Clause::Call { name, .. } => match name.as_str() {
                    "db.schema" | "db.indexes" | "db.commitState" | "graph.project"
                    | "graph.degree" | "graph.reachable" | "graph.shortestPath"
                    | "graph.pagerank" | "graph.wcc" | "graph.scc" | "knowledge.rank" => {}
                    "db.memory" => {
                        if !self.instance_admin {
                            return Err(ExecError::Forbidden(
                                "instance administrator access required",
                            ));
                        }
                    }
                    _ => return Err(ExecError::Forbidden("procedure is not authorized")),
                },
                Clause::Match { .. } | Clause::Unwind { .. } | Clause::Return { .. } => {}
            }
        }
        Ok(())
    }
}

/// A borrowed storage snapshot and write overlay shared by multiple queries.
///
/// Each `run` has fresh bindings and parameters, but sees earlier staged writes.
/// Any query or commit error transitions to `Failed` and drops the storage
/// transaction immediately. Successful commit transitions to `Committed`;
/// rollback of an active context transitions to `RolledBack`. Rollback is
/// idempotent and preserves terminal states. Dropping an active context discards
/// its writes. No savepoints or incremental Bolt result cursor are provided.
pub struct QueryTransaction<'db> {
    txn: Option<Txn<'db>>,
    catalog: Catalog,
    wrote: bool,
    state: QueryTransactionState,
    limits: ExecutionLimits,
    reservation: Option<budget::Reservation>,
}

impl<'db> QueryTransaction<'db> {
    /// Capture one storage snapshot and load its catalog.
    pub fn begin(db: &'db Database) -> Result<Self, ExecError> {
        Self::begin_with_limits(db, ExecutionLimits::default())
    }

    pub fn begin_with_limits(
        db: &'db Database,
        limits: ExecutionLimits,
    ) -> Result<Self, ExecError> {
        if limits.memory_bytes == 0 {
            return Err(ExecError::Semantic(
                "query allocation limit must be positive".into(),
            ));
        }
        let mut txn = db.begin();
        // This dependency also rejects a concurrent catalog migration when writing.
        let catalog = match txn.get_for_update(Cf::Meta, b"catalog")? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| StoreError::Catalog(e.to_string()))?
            }
            None => Catalog {
                version: 0,
                node_types: vec![],
                edge_types: vec![],
                streams: vec![],
            },
        };
        Ok(Self {
            txn: Some(txn),
            catalog,
            wrote: false,
            state: QueryTransactionState::Active,
            limits,
            reservation: None,
        })
    }

    /// Share conservative transaction reservations across connections/databases.
    /// The reservation lasts through commit, rollback, failure or drop, including
    /// time waiting for a Bolt client to consume its materialized result.
    pub fn begin_with_pool(
        db: &'db Database,
        limits: ExecutionLimits,
        pool: std::sync::Arc<QueryMemoryPool>,
    ) -> Result<Self, ExecError> {
        let mut transaction = Self::begin_with_limits(db, limits)?;
        transaction.reservation = Some(budget::Reservation::new(pool));
        Ok(transaction)
    }

    pub fn state(&self) -> QueryTransactionState {
        self.state
    }

    fn require_active(&self) -> Result<(), ExecError> {
        if self.state != QueryTransactionState::Active {
            return Err(ExecError::Semantic(format!(
                "query transaction is {:?}; begin a new transaction",
                self.state
            )));
        }
        Ok(())
    }

    fn fail(&mut self) {
        self.txn.take();
        self.reservation.take();
        self.state = QueryTransactionState::Failed;
    }

    /// Evaluate a query without committing. An error discards all prior runs.
    pub fn run(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<QueryOutput, ExecError> {
        self.run_with_access(query, params, QueryAccess::UNRESTRICTED)
    }

    /// Authorize the entire query before execution. A denial discards all prior runs.
    pub fn run_with_access(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
        access: QueryAccess,
    ) -> Result<QueryOutput, ExecError> {
        self.require_active()?;
        if query.len() > self.limits.memory_bytes {
            let error = ExecError::MemoryLimit {
                limit: self.limits.memory_bytes,
                attempted: query.len(),
            };
            self.fail();
            return Err(error);
        }
        let source_bytes = query.len();
        match parse(query) {
            Ok(query) => {
                if let Err(error) = access.authorize(&query) {
                    self.fail();
                    return Err(error);
                }
                self.run_parsed(&query, params, source_bytes)
            }
            Err(error) => {
                self.fail();
                Err(error.into())
            }
        }
    }

    fn run_parsed(
        &mut self,
        query: &Query,
        params: &BTreeMap<String, Value>,
        source_bytes: usize,
    ) -> Result<QueryOutput, ExecError> {
        let result = execute_clauses(
            self.txn.as_mut().expect("active transaction"),
            &self.catalog,
            query,
            params,
            self.limits,
            source_bytes,
            self.reservation.as_ref(),
        );
        match result {
            Ok((output, wrote)) => {
                self.wrote |= wrote;
                Ok(output)
            }
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }

    /// Commit once, returning a store sequence for a write transaction, or
    /// `None` for read-only completion. Write sequences are durable per store;
    /// they are not global, fork-aware bookmarks or instance Raft indexes.
    /// Read-only completion drops the snapshot without checking write conflicts,
    /// preserving the standalone executor's read-only behavior.
    pub fn commit(&mut self) -> Result<Option<u64>, ExecError> {
        self.require_active()?;
        let txn = self.txn.take().expect("active transaction");
        let result = if self.wrote {
            txn.commit().map(Some).map_err(ExecError::from)
        } else {
            drop(txn);
            Ok(None)
        };
        self.reservation.take();
        self.state = if result.is_ok() {
            QueryTransactionState::Committed
        } else {
            QueryTransactionState::Failed
        };
        result
    }

    /// Discard active writes and release the snapshot. Safe to call repeatedly.
    pub fn rollback(&mut self) {
        if self.state == QueryTransactionState::Active {
            self.txn.take();
            self.reservation.take();
            self.state = QueryTransactionState::RolledBack;
        }
    }
}

/// Execute and atomically commit a single query.
pub fn execute(
    db: &Database,
    query: &str,
    params: &BTreeMap<String, Value>,
) -> Result<QueryOutput, ExecError> {
    execute_with_limits(db, query, params, ExecutionLimits::default())
}

/// Conservative per-RUN allocation guard; limits are not a hard RSS ceiling.
pub fn execute_with_limits(
    db: &Database,
    query: &str,
    params: &BTreeMap<String, Value>,
    limits: ExecutionLimits,
) -> Result<QueryOutput, ExecError> {
    if query.len() > limits.memory_bytes {
        return Err(ExecError::MemoryLimit {
            limit: limits.memory_bytes,
            attempted: query.len(),
        });
    }
    // Preserve syntax-error precedence over catalog loading for this API.
    let source_bytes = query.len();
    let query = parse(query)?;
    let mut transaction = QueryTransaction::begin_with_limits(db, limits)?;
    let output = transaction.run_parsed(&query, params, source_bytes)?;
    transaction.commit()?;
    Ok(output)
}

fn execute_clauses(
    txn: &mut Txn<'_>,
    catalog: &Catalog,
    query: &Query,
    params: &BTreeMap<String, Value>,
    limits: ExecutionLimits,
    source_bytes: usize,
    reservation: Option<&budget::Reservation>,
) -> Result<(QueryOutput, bool), ExecError> {
    let budget = Budget::new(limits, reservation);
    budget.charge(source_bytes)?;
    budget.charge(budget::map_bytes(params))?;
    budget.charge(txn.estimated_memory_bytes())?;
    validate::query(
        query,
        &Context {
            txn,
            catalog,
            params,
            budget: &budget,
        },
    )?;
    if stream::supports(query) {
        return stream::run(
            &Context {
                txn,
                catalog,
                params,
                budget: &budget,
            },
            query,
        )
        .map(|output| (output, false));
    }
    let mut rows = vec![Bindings::new()];
    let mut output = QueryOutput {
        fields: vec![],
        rows: vec![],
    };
    let mut wrote = false;
    let mut overlay_bytes = txn.estimated_memory_bytes();
    for clause in &query.clauses {
        let ctx = Context {
            txn,
            catalog,
            params,
            budget: &budget,
        };
        match clause {
            Clause::Call {
                name,
                args,
                yields,
                where_,
            } => {
                rows = procedures::run(name, args, yields, &ctx)?;
                if let Some(predicate) = where_ {
                    let mut filtered = Vec::new();
                    for row in rows {
                        if eval::boolean(eval(predicate, &row, &ctx)?)? == Some(true) {
                            filtered.push(row);
                        }
                    }
                    rows = filtered;
                }
            }
            Clause::Unwind { expr, var } => {
                let mut next = vec![];
                for row in rows {
                    match scalar(eval(expr, &row, &ctx)?)? {
                        Value::Null => {}
                        Value::List(values) => {
                            for value in values {
                                let mut row = row.clone();
                                row.insert(var.clone(), Binding::Value(value));
                                budget.bindings(&row)?;
                                next.push(row);
                            }
                        }
                        _ => return Err(ExecError::Semantic("UNWIND needs a list or null".into())),
                    }
                }
                rows = next;
            }
            Clause::Match { patterns, where_ } => {
                for pattern in patterns {
                    rows = graph::match_pattern(&ctx, rows, pattern, where_.as_ref())?;
                }
                if let Some(expr) = where_ {
                    let mut next = vec![];
                    for row in rows {
                        if eval::boolean(eval(expr, &row, &ctx)?)? == Some(true) {
                            next.push(row);
                        }
                    }
                    rows = next;
                }
            }
            Clause::Merge(pattern) => {
                for row in &mut rows {
                    graph::write_pattern(txn, catalog, params, row, pattern, false, &budget)?;
                    budget.charge(txn.estimated_memory_bytes().saturating_sub(overlay_bytes))?;
                    overlay_bytes = txn.estimated_memory_bytes();
                    wrote = true;
                }
            }
            Clause::Create(patterns) => {
                for row in &mut rows {
                    for pattern in patterns {
                        graph::write_pattern(txn, catalog, params, row, pattern, true, &budget)?;
                        budget
                            .charge(txn.estimated_memory_bytes().saturating_sub(overlay_bytes))?;
                        overlay_bytes = txn.estimated_memory_bytes();
                        wrote = true;
                    }
                }
            }
            Clause::Set(items) => {
                // Evaluate one row's assignments together, then update each node once.
                for row in &rows {
                    let ctx = Context {
                        txn,
                        catalog,
                        params,
                        budget: &budget,
                    };
                    let mut changes: BTreeMap<NodeId, BTreeMap<String, Value>> = BTreeMap::new();
                    for item in items {
                        let id = graph::bound_node(row, &item.var)?;
                        changes
                            .entry(id)
                            .or_default()
                            .insert(item.prop.clone(), scalar(eval(&item.value, row, &ctx)?)?);
                    }
                    for (id, props) in changes {
                        let node = graph::node_out(
                            &Context {
                                txn,
                                catalog,
                                params,
                                budget: &budget,
                            },
                            id,
                        )?;
                        let Value::Str(entity_id) = &node.props["entity_id"] else {
                            unreachable!()
                        };
                        brainwire_store::graph::merge_node(
                            txn,
                            catalog,
                            &node.label,
                            entity_id,
                            &props,
                        )?;
                        budget
                            .charge(txn.estimated_memory_bytes().saturating_sub(overlay_bytes))?;
                        overlay_bytes = txn.estimated_memory_bytes();
                        wrote = true;
                    }
                }
            }
            Clause::DetachDelete(vars) => {
                let mut ids = BTreeSet::new();
                for row in &rows {
                    for var in vars {
                        ids.insert(graph::bound_node(row, var)?);
                    }
                }
                for id in ids {
                    brainwire_store::graph::detach_delete(txn, id)?;
                    budget.charge(txn.estimated_memory_bytes().saturating_sub(overlay_bytes))?;
                    overlay_bytes = txn.estimated_memory_bytes();
                    wrote = true;
                }
            }
            Clause::Return { items, limit } => {
                output = eval::project(&ctx, &rows, items, limit.as_ref())?;
            }
        }
    }
    budget.check_deadline()?;
    Ok((output, wrote))
}
