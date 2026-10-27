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
mod plan;
mod procedures;
mod run;
mod val;
mod write;

use crate::{ast::*, parse, ParseError};
use brainwire_core::{acl::ReadVisibility, ids::NodeId, schema::Catalog, value::Value};
use brainwire_store::{
    db::{Database, StoreError},
    graph::GraphError,
    txn::{Cf, Txn, TxnError},
};
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
    Relationship(RelationshipOut),
    Path(PathOut),
    /// A list or map holding nodes, relationships or paths.
    List(Vec<Out>),
    Map(BTreeMap<String, Out>),
}
#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipOut {
    /// Stable 63-bit id derived from the relationship's identity.
    pub id: i64,
    pub element_id: String,
    pub start: NodeId,
    pub end: NodeId,
    pub rel_type: String,
    pub props: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct PathOut {
    pub nodes: Vec<NodeOut>,
    /// `rels[i]` joins `nodes[i]` and `nodes[i + 1]`.
    pub rels: Vec<RelationshipOut>,
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
impl From<brainwire_store::replicated_apply::ApplyError> for ExecError {
    fn from(error: brainwire_store::replicated_apply::ApplyError) -> Self {
        match error {
            brainwire_store::replicated_apply::ApplyError::Transaction(error) => error.into(),
            error => StoreError::Corrupt(format!("replication preparation: {error}")).into(),
        }
    }
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
/// Procedure rows by YIELD alias.
type Binding = val::Val;
type Bindings = BTreeMap<String, Binding>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Relationship {
    source: NodeId,
    edge_type: brainwire_core::ids::EdgeTypeId,
    target: NodeId,
    id: brainwire_core::ids::EdgeId,
}
impl Relationship {
    fn element_id(&self) -> String {
        brainwire_core::ids::edge_element_id(self.source, self.edge_type, self.target, self.id)
    }
    /// A stable nonnegative integer id (FNV-1a over the identity).
    fn legacy_id(&self) -> i64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in self
            .source
            .0
            .to_be_bytes()
            .into_iter()
            .chain(self.edge_type.0.to_be_bytes())
            .chain(self.target.0.to_be_bytes())
            .chain(self.id.0.to_be_bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        (hash >> 1) as i64
    }
}
struct Context<'a, 'db> {
    txn: &'a Txn<'db>,
    catalog: &'a Catalog,
    params: &'a BTreeMap<String, Value>,
    budget: &'a Budget<'a>,
    visibility: Option<&'a ReadVisibility>,
    checkpoint_dir: Option<&'a std::path::Path>,
    vars: &'a plan::Vars,
    /// The node a scan is positioned on, with its stored bytes.
    cache: Option<&'a NodeCache>,
    /// Parameters converted once per query.
    param_vals: Option<&'a BTreeMap<String, val::Val>>,
}

/// Lets property reads reuse the value a label scan already holds.
#[derive(Default)]
struct NodeCache(std::cell::RefCell<Option<(NodeId, Vec<u8>)>>);

/// Trusted prepared bytes; the caller must obtain quorum and apply before acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedReplication {
    pub expected_revision: u64,
    pub batch: Vec<u8>,
}

/// Lifecycle of a shared query transaction. Terminal states cannot be reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTransactionState {
    Active,
    /// Overlay exported for quorum; this state is not a durable commit.
    Prepared,
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
        for clause in query
            .clauses
            .iter()
            .chain(query.unions.iter().flat_map(|(_, part)| part))
        {
            match clause {
                Clause::Create(_)
                | Clause::Merge { .. }
                | Clause::Set(_)
                | Clause::Remove(_)
                | Clause::Delete { .. } => {
                    if !self.write {
                        return Err(ExecError::Forbidden("graph write access required"));
                    }
                }
                Clause::Call { name, .. } => match name.as_str() {
                    "db.schema" | "db.indexes" | "db.commitState" | "graph.project"
                    | "graph.degree" | "graph.reachable" | "graph.shortestPath"
                    | "graph.pagerank" | "graph.wcc" | "graph.scc" | "knowledge.rank" => {}
                    "db.setNodeAcl" | "db.setNodeAcls" => {
                        if !self.write || !self.instance_admin {
                            return Err(ExecError::Forbidden("node ACL updates require graph write and instance administrator access"));
                        }
                    }
                    "db.createIndex"
                    | "db.dropIndex"
                    | "db.createLabel"
                    | "db.createRelationshipType"
                    | "db.dropProperty" => {
                        if !self.write || !self.instance_admin {
                            return Err(ExecError::Forbidden(
                                "catalog changes require graph write and instance administrator access",
                            ));
                        }
                    }
                    "db.labels"
                    | "db.relationshipTypes"
                    | "db.propertyKeys"
                    | "dbms.components" => {}
                    "db.memory" | "db.checkpoint" | "db.nodeAcls" => {
                        if !self.instance_admin {
                            return Err(ExecError::Forbidden(
                                "instance administrator access required",
                            ));
                        }
                    }
                    _ => return Err(ExecError::Forbidden("procedure is not authorized")),
                },
                Clause::Match { .. }
                | Clause::Unwind { .. }
                | Clause::With(_)
                | Clause::Return(_) => {}
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
    visibility: Option<ReadVisibility>,
    ran: bool,
    replicated_mode: bool,
    checkpoint_dir: Option<std::path::PathBuf>,
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
        let txn = db.begin();
        // A plain read: read-only transactions must not join the committer's
        // writer set. Writers add the catalog dependency before commit.
        let catalog = match txn.get(Cf::Meta, b"catalog")? {
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
            visibility: None,
            ran: false,
            replicated_mode: false,
            checkpoint_dir: None,
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

    /// Select transaction-local IDs before the first RUN. Direct commit is refused;
    /// the caller must prepare, obtain quorum and apply the materialized entry.
    pub fn with_replicated_ids(mut self) -> Self {
        if self.ran || self.state != QueryTransactionState::Active {
            self.fail();
            return self;
        }
        self.txn = self.txn.take().map(Txn::with_replicated_ids);
        self.replicated_mode = true;
        self
    }

    /// Attach a trusted immutable read policy before the first RUN.
    /// User-supplied Cypher options cannot change this transaction policy.
    pub fn with_read_visibility(mut self, policy: ReadVisibility) -> Result<Self, ExecError> {
        self.require_active()?;
        if self.ran || self.visibility.is_some() {
            return Err(ExecError::Semantic(
                "read visibility must be attached once before the first RUN".into(),
            ));
        }
        policy
            .validate()
            .map_err(|error| ExecError::Semantic(error.to_string()))?;
        self.visibility = Some(policy);
        Ok(self)
    }

    /// Enable `CALL db.checkpoint(name)` below this operator-owned directory.
    pub fn with_checkpoint_dir(mut self, root: std::path::PathBuf) -> Self {
        self.checkpoint_dir = Some(root);
        self
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
        self.ran = true;
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

    /// A checkpoint is a non-transactional side effect: it captures committed
    /// data only, so refuse forms where it would run per row, beside staged
    /// writes, on one replica of a quorum group or under row-level visibility.
    fn checkpoint_preconditions(&self, query: &Query) -> Result<(), ExecError> {
        let position = query.clauses.iter().position(
            |clause| matches!(clause, Clause::Call { name, .. } if name == "db.checkpoint"),
        );
        let Some(position) = position else {
            return Ok(());
        };
        if position != 0 {
            return Err(ExecError::Semantic(
                "db.checkpoint must be the first clause".into(),
            ));
        }
        if self.wrote
            || query.clauses.iter().any(|clause| {
                matches!(
                    clause,
                    Clause::Create(_)
                        | Clause::Merge { .. }
                        | Clause::Set(_)
                        | Clause::Remove(_)
                        | Clause::Delete { .. }
                        | Clause::Call { .. }
                ) && !matches!(clause, Clause::Call { name, .. } if name == "db.checkpoint")
            })
        {
            return Err(ExecError::Semantic(
                "db.checkpoint cannot share a transaction with writes or other procedures".into(),
            ));
        }
        if self.replicated_mode || self.visibility.is_some() {
            return Err(ExecError::Forbidden(
                "db.checkpoint is unavailable for replicated or visibility-restricted transactions",
            ));
        }
        Ok(())
    }

    fn run_parsed(
        &mut self,
        query: &Query,
        params: &BTreeMap<String, Value>,
        source_bytes: usize,
    ) -> Result<QueryOutput, ExecError> {
        self.ran = true;
        if let Err(error) = self.checkpoint_preconditions(query) {
            self.fail();
            return Err(error);
        }
        let result = execute_clauses(
            self.txn.as_mut().expect("active transaction"),
            &self.catalog,
            query,
            params,
            self.limits,
            source_bytes,
            self.reservation.as_ref(),
            self.visibility.as_ref(),
            self.checkpoint_dir.as_deref(),
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
        let mut txn = self.txn.take().expect("active transaction");
        let result = if self.wrote {
            // Rejects a catalog migration committed since this snapshot.
            txn.depend(Cf::Meta, b"catalog");
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

    /// Validate MVCC and export the overlay without committing locally.
    /// None completes a read-only request. Prepared writes remain nondurable
    /// until the caller's consensus/apply layer succeeds.
    pub fn prepare_replication(&mut self) -> Result<Option<PreparedReplication>, ExecError> {
        self.require_active()?;
        if !self.replicated_mode {
            self.fail();
            return Err(ExecError::Semantic(
                "replicated IDs must be enabled before RUN".into(),
            ));
        }
        if self.wrote {
            let estimate = self
                .txn
                .as_ref()
                .expect("active transaction")
                .estimated_memory_bytes()
                .checked_mul(3)
                .ok_or(ExecError::MemoryLimit {
                    limit: self.limits.memory_bytes,
                    attempted: usize::MAX,
                });
            let guard = estimate.and_then(|estimate| {
                Budget::new(self.limits, self.reservation.as_ref()).charge(estimate)
            });
            if let Err(error) = guard {
                self.fail();
                return Err(error);
            }
        }
        let mut txn = self.txn.take().expect("active transaction");
        if self.wrote {
            txn.depend(Cf::Meta, b"catalog");
        }
        let result = txn
            .prepare_replication(brainwire_store::replicated_apply::ApplyLimits::default())
            .and_then(|batch| {
                batch
                    .map(|batch| {
                        let bytes = batch.canonical_bytes(
                            brainwire_store::replicated_apply::ApplyLimits::default(),
                        )?;
                        Ok(PreparedReplication {
                            expected_revision: batch.index - 1,
                            batch: bytes,
                        })
                    })
                    .transpose()
            })
            .map_err(ExecError::from);
        self.reservation.take();
        self.state = match &result {
            Ok(Some(_)) => QueryTransactionState::Prepared,
            Ok(None) => QueryTransactionState::Committed,
            Err(_) => QueryTransactionState::Failed,
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

#[allow(clippy::too_many_arguments)]
fn execute_clauses(
    txn: &mut Txn<'_>,
    catalog: &Catalog,
    query: &Query,
    params: &BTreeMap<String, Value>,
    limits: ExecutionLimits,
    source_bytes: usize,
    reservation: Option<&budget::Reservation>,
    visibility: Option<&ReadVisibility>,
    checkpoint_dir: Option<&std::path::Path>,
) -> Result<(QueryOutput, bool), ExecError> {
    let clauses = query
        .clauses
        .iter()
        .chain(query.unions.iter().flat_map(|(_, part)| part));
    let mutates = |clause: &Clause| {
        matches!(
            clause,
            Clause::Create(_)
                | Clause::Merge { .. }
                | Clause::Set(_)
                | Clause::Remove(_)
                | Clause::Delete { .. }
        ) || matches!(clause, Clause::Call { name, .. } if name == "db.setNodeAcl" || name == "db.setNodeAcls" || plan::is_admin_call(name))
    };
    if visibility.is_some() && clauses.clone().any(mutates) {
        return Err(ExecError::Forbidden(
            "read visibility transactions cannot mutate the graph or ACL metadata",
        ));
    }
    let budget = Budget::new(limits, reservation);
    budget.charge(source_bytes)?;
    budget.charge(budget::map_bytes(params))?;
    budget.charge(txn.estimated_memory_bytes())?;
    let empty = plan::Vars::default();
    let plan = plan::plan(
        query,
        &Context {
            txn,
            catalog,
            params,
            budget: &budget,
            visibility,
            checkpoint_dir,
            vars: &empty,
            cache: None,
            param_vals: None,
        },
    )?;
    let param_vals = params
        .iter()
        .map(|(k, v)| (k.clone(), val::Val::from(v)))
        .collect();
    let env = run::Env {
        catalog,
        params,
        budget: &budget,
        visibility,
        checkpoint_dir,
        vars: &plan.vars,
        cache: NodeCache::default(),
        param_vals: &param_vals,
    };
    run::execute(txn, &env, &plan)
}
