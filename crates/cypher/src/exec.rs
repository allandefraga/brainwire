//! Atomic Cypher execution with bounded row-at-a-time read pipelines.
//!
//! A query transaction owns one snapshot and write overlay across its runs.
//! All output is evaluated before commit; any query error discards every staged
//! graph write. `execute` provides the same atomic behavior for one query.
//! Read-only MATCH/UNWIND pipelines stream into bounded output or simple counts.
//! Writes, grouped aggregates and CALL remain eager. A read returning graph entities
//! can keep them as handles and materialize them per page (`QueryTransaction::fetch`).
//! Conservative guards and an optional shared pool are not process RSS caps.
mod budget;
mod eval;
use budget::Budget;
pub use budget::{ExecutionLimits, QueryMemoryPool, QueryMemoryUsage};
mod graph;
mod parallel;
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
/// What a RUN answers on the streaming path.
#[derive(Debug)]
pub enum RunOutput {
    /// Fully materialized output (writes, scalar-only results, procedures).
    Ready(QueryOutput),
    /// Row handles still to materialize with [`QueryTransaction::fetch`].
    Stream(QueryStream),
}

/// What a lazy RUN did (`QueryTransaction::run_lazy_with_access`).
#[derive(Debug)]
pub enum LazyRun {
    /// Every row went to the sink as it was produced (or the sink stopped it).
    Streamed,
    /// The plan needs all rows first; this is its ordinary streaming-path output.
    Output(RunOutput),
}

/// The projected rows of a read, entities kept as handles (`NodeId`,
/// relationship identity). Only the transaction that ran it may fetch it.
#[derive(Debug)]
pub struct QueryStream {
    fields: Vec<String>,
    rows: std::collections::VecDeque<run::Row>,
    /// Retained bytes of the handles still held (`Budget::row_bytes`).
    bytes: usize,
}
impl QueryStream {
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
    /// Rows not fetched yet.
    pub fn remaining(&self) -> usize {
        self.rows.len()
    }
    /// Drop up to *n* rows without materializing them (Bolt DISCARD).
    pub fn discard(&mut self, n: usize) {
        let take = n.min(self.rows.len());
        for row in self.rows.drain(..take) {
            self.bytes = self.bytes.saturating_sub(Budget::row_bytes(&row));
        }
    }
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
    #[error(
        "shared query memory pool exhausted: requested {requested} additional bytes, {reserved} reserved, limit {limit}"
    )]
    MemoryPoolExhausted {
        limit: usize,
        reserved: usize,
        requested: usize,
    },
    #[error(
        "process memory ceiling reached: requested {requested} additional bytes, {available} available"
    )]
    ProcessMemoryLimit { requested: usize, available: usize },
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
            GraphError::UnknownLabel(s)
            | GraphError::UnknownEdgeType(s)
            | GraphError::Catalog(s) => Self::Semantic(s),
            // Caller mistakes, not storage failures: surface as client errors.
            e @ (GraphError::BadSignature(_)
            | GraphError::ReservedProperty(_)
            | GraphError::Acl(_)
            | GraphError::NodeNotFound(_)
            | GraphError::Id(brainwire_core::ids::IdError::BadElementId)) => {
                Self::Semantic(e.to_string())
            }
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
    /// A parallel worker's share of one label scan: (anchor slot, lo, hi).
    partition: Option<(usize, NodeId, NodeId)>,
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
/// Keeps materialized result accounting alive after the storage transaction expires.
/// The transport drops this lease only after discarding its buffered rows.
#[derive(Clone)]
pub struct QueryMemoryLease {
    _reservation: Option<budget::Reservation>,
    limit: usize,
}

/// Accounting for output bytes that outlive the producer's temporary row scope.
/// The transport owns this until its records have been sent or discarded.
pub struct QueryOutputLease {
    _reservation: Option<budget::OutputReservation>,
    page_bytes: usize,
}
impl QueryOutputLease {
    /// Transport buffers must fit the quota of this row's producer.
    pub fn output_page_bytes(&self) -> usize {
        self.page_bytes
    }
}
impl QueryMemoryLease {
    pub fn output_page_bytes(&self) -> usize {
        (self.limit / 32).clamp(1, 4 << 20)
    }
    pub fn reserve_output(&self, bytes: usize) -> Result<QueryOutputLease, ExecError> {
        if bytes > self.limit {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted: bytes,
            });
        }
        let reservation = self
            ._reservation
            .as_ref()
            .map(|reservation| reservation.reserve_output(bytes, self.limit))
            .transpose()?;
        Ok(QueryOutputLease {
            _reservation: reservation,
            page_bytes: self.output_page_bytes(),
        })
    }
}

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
                            return Err(ExecError::Forbidden(
                                "node ACL updates require graph write and instance administrator access",
                            ));
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
/// its writes. No savepoints are provided; `run_streaming_with_access` and `fetch`
/// materialize an entity result page by page against this snapshot.
/// Errors from the experimental storage-identified commit. Keep storage write
/// ambiguity separate from ordinary execution validation or rollback failures.
#[derive(Debug, thiserror::Error)]
pub enum IdentifiedCommitError {
    #[error(transparent)]
    Execution(#[from] ExecError),
    #[error(transparent)]
    Ledger(#[from] brainwire_store::db::request_ledger::LedgerError),
}

pub struct QueryTransaction<'db> {
    txn: Option<Txn<'db>>,
    catalog: Catalog,
    wrote: bool,
    state: QueryTransactionState,
    limits: ExecutionLimits,
    transaction_deadline: Option<std::time::Instant>,
    reservation: Option<budget::Reservation>,
    visibility: Option<ReadVisibility>,
    ran: bool,
    replicated_mode: bool,
    identified_write: bool,
    checkpoint_dir: Option<std::path::PathBuf>,
}

impl<'db> QueryTransaction<'db> {
    /// Capture one storage snapshot and load its catalog.
    pub fn begin(db: &'db Database) -> Result<Self, ExecError> {
        Self::begin_with_limits(db, ExecutionLimits::default())
    }

    /// Embedded transactions retain conservative execution high-water and live
    /// output credits until their owners drop, across all RUNs in this transaction.
    /// Their private accounting is not exposed as server-wide `db.memory()` telemetry.
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
        // A plain read: read-only transactions must not join the committer's
        // writer set. Writers add the catalog dependency before commit.
        let catalog = load_catalog(&mut txn)?;
        Ok(Self {
            txn: Some(txn),
            catalog,
            wrote: false,
            state: QueryTransactionState::Active,
            limits,
            transaction_deadline: None,
            // Embedded callers still share one transaction budget across output
            // leases; a server-provided pool replaces this private pool below.
            reservation: Some(budget::Reservation::new(std::sync::Arc::new(
                QueryMemoryPool::new_private(limits.memory_bytes),
            ))),
            visibility: None,
            ran: false,
            replicated_mode: false,
            identified_write: false,
            checkpoint_dir: None,
        })
    }

    /// Experimental capability attached by a trusted server before any RUN.
    /// CALL procedures are refused to exclude effects outside the atomic batch.
    #[doc(hidden)]
    pub fn with_identified_write(mut self) -> Result<Self, ExecError> {
        self.require_active()?;
        if self.ran || self.replicated_mode || self.visibility.is_some() || self.identified_write {
            self.fail();
            return Err(ExecError::Forbidden(
                "identified writes require a fresh unrestricted standalone transaction",
            ));
        }
        self.identified_write = true;
        Ok(self)
    }

    /// Apply an absolute deadline shared by every RUN and transaction completion.
    /// Cooperative checks cannot interrupt a blocking storage or durable commit call.
    pub fn with_deadline(mut self, deadline: std::time::Instant) -> Self {
        self.transaction_deadline = Some(deadline);
        self
    }

    pub fn check_transaction_deadline(&mut self) -> Result<(), ExecError> {
        self.require_active()?;
        if self
            .transaction_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            self.fail();
            return Err(ExecError::TimedOut);
        }
        Ok(())
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
        if self.ran || self.identified_write || self.state != QueryTransactionState::Active {
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
        if self.ran || self.identified_write || self.visibility.is_some() {
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
        self.run_authorized(query, params, access, run::execute)
    }

    /// `run_with_access`, but a read whose rows hold graph entities answers with a
    /// [`QueryStream`] of row handles; [`Self::fetch`] materializes it page by page
    /// against this transaction's snapshot and visibility. Any other query answers
    /// with its output, as `run_with_access` does.
    pub fn run_streaming_with_access(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
        access: QueryAccess,
    ) -> Result<RunOutput, ExecError> {
        self.run_authorized(query, params, access, finish_streaming)
    }

    /// Up to *n* more rows of *stream*, materialized now (entity records and
    /// properties, under the read visibility and with the per-RUN memory guard and
    /// deadline). Handles still held stay charged; an error fails the transaction.
    pub fn fetch(
        &mut self,
        stream: &mut QueryStream,
        n: usize,
    ) -> Result<Vec<Vec<Out>>, ExecError> {
        self.check_transaction_deadline()?;
        let mut limits = self.limits;
        if let Some(deadline) = self.transaction_deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            limits.timeout = Some(limits.timeout.map_or(remaining, |own| own.min(remaining)));
        }
        let take = n.min(stream.rows.len());
        let rows: Vec<run::Row> = stream.rows.drain(..take).collect();
        let page = rows.iter().fold(0usize, |sum, row| {
            sum.saturating_add(Budget::row_bytes(row))
        });
        stream.bytes = stream.bytes.saturating_sub(page);
        let result = {
            let budget = Budget::new(limits, self.reservation.as_ref());
            let empty_params = BTreeMap::new();
            let vars = plan::Vars::default();
            budget.retain(stream.bytes).and_then(|()| {
                let ctx = Context {
                    txn: self.txn.as_ref().expect("active transaction"),
                    catalog: &self.catalog,
                    params: &empty_params,
                    budget: &budget,
                    visibility: self
                        .visibility
                        .as_ref()
                        .filter(|policy| !policy.sees_everything()),
                    checkpoint_dir: None,
                    vars: &vars,
                    cache: None,
                    param_vals: None,
                    partition: None,
                };
                run::output_rows(&ctx, &budget, Vec::new(), rows).map(|output| output.rows)
            })
        };
        if result.is_err() {
            stream.rows.clear();
            stream.bytes = 0;
            self.fail();
        }
        result
    }

    /// A read whose plan streams (`run::lazy_part`) and whose rows hold plain values
    /// (not nodes, relationships or paths) calls *fields* with its columns,
    /// then *rows* with each row (and its estimated bytes) as it is produced, materialized under this
    /// transaction's visibility and never held: memory does not grow with the result.
    /// Either callback returns false to stop. Any other query behaves as
    /// [`Self::run_streaming_with_access`] and comes back as [`LazyRun::Output`].
    pub fn run_lazy_with_access(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
        access: QueryAccess,
        fields: &mut dyn FnMut(&[String]) -> bool,
        rows: &mut dyn FnMut(Vec<Out>, usize) -> bool,
    ) -> Result<LazyRun, ExecError> {
        self.run_lazy_leased_with_access(
            query,
            params,
            access,
            fields,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            &mut |row, bytes, _lease| rows(row, bytes),
        )
    }

    /// Lazy transport integration: each returned row carries its owned credit.
    pub fn run_lazy_leased_with_access(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
        access: QueryAccess,
        fields: &mut dyn FnMut(&[String]) -> bool,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
        rows: &mut dyn FnMut(Vec<Out>, usize, QueryOutputLease) -> bool,
    ) -> Result<LazyRun, ExecError> {
        self.run_authorized(query, params, access, |txn, env, plan| {
            let Some(part) = run::lazy_part(plan) else {
                return finish_streaming(txn, env, plan)
                    .map(|(output, wrote)| (LazyRun::Output(output), wrote));
            };
            env.budget
                .set_cancellation(std::sync::Arc::clone(&cancellation));
            if let Some(scan) = parallel::streaming_scan(&env.ctx(txn), &part.ops)? {
                if fields(&part.columns) {
                    parallel::stream_leased(&env.ctx(txn), part, scan, &cancellation, rows)?;
                }
                return Ok((LazyRun::Streamed, false));
            }
            // Rows holding graph entities keep the handle path: its parallel scan was
            // measured faster under concurrent readers, and handles are small. The first
            // row decides; only that row's work is repeated.
            let mut first = true;
            let mut entities = false;
            let mut open = true;
            let streamed =
                run::stream_part_leased(&env.ctx(txn), env, part, &mut |row, bytes, lease| {
                    if std::mem::take(&mut first) {
                        entities = row.iter().any(|out| !matches!(out, Out::Value(_)));
                        open = !entities && fields(&part.columns);
                    }
                    open && rows(row, bytes, lease)
                });
            if !cancellation.load(std::sync::atomic::Ordering::Acquire) {
                streamed?;
            }
            if entities {
                return finish_streaming(txn, env, plan)
                    .map(|(output, wrote)| (LazyRun::Output(output), wrote));
            }
            if first {
                fields(&part.columns);
            }
            Ok((LazyRun::Streamed, false))
        })
    }

    fn run_authorized<T>(
        &mut self,
        query: &str,
        params: &BTreeMap<String, Value>,
        access: QueryAccess,
        finish: impl FnOnce(&mut Txn<'_>, &run::Env<'_>, &plan::Plan) -> Result<(T, bool), ExecError>,
    ) -> Result<T, ExecError> {
        self.check_transaction_deadline()?;
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
                self.run_parsed_with(&query, params, source_bytes, finish)
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
        self.run_parsed_with(query, params, source_bytes, run::execute)
    }

    fn run_parsed_with<T>(
        &mut self,
        query: &Query,
        params: &BTreeMap<String, Value>,
        source_bytes: usize,
        finish: impl FnOnce(&mut Txn<'_>, &run::Env<'_>, &plan::Plan) -> Result<(T, bool), ExecError>,
    ) -> Result<T, ExecError> {
        self.check_transaction_deadline()?;
        self.ran = true;
        if self.identified_write
            && query
                .clauses
                .iter()
                .chain(query.unions.iter().flat_map(|(_, part)| part))
                .any(|clause| matches!(clause, Clause::Call { .. }))
        {
            self.fail();
            return Err(ExecError::Forbidden("procedures are unavailable in identified transactions; execute transactional Cypher only"));
        }
        if let Err(error) = self.checkpoint_preconditions(query) {
            self.fail();
            return Err(error);
        }
        let changes_catalog = query
            .clauses
            .iter()
            .any(|clause| matches!(clause, Clause::Call { name, .. } if plan::is_admin_call(name)));
        if changes_catalog && self.replicated_mode {
            self.fail();
            return Err(ExecError::Forbidden(
                "catalog changes are not replicated; apply them to a standalone server",
            ));
        }
        let mut limits = self.limits;
        if let Some(deadline) = self.transaction_deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            limits.timeout = Some(
                limits
                    .timeout
                    .map_or(remaining, |server| server.min(remaining)),
            );
        }
        let result = execute_clauses_with(
            self.txn.as_mut().expect("active transaction"),
            &self.catalog,
            query,
            params,
            limits,
            source_bytes,
            self.reservation.as_ref(),
            self.visibility.as_ref(),
            self.checkpoint_dir.as_deref(),
            finish,
        );
        match result {
            Ok((output, wrote)) => {
                self.wrote |= wrote;
                if changes_catalog {
                    // Later runs in this transaction see the staged catalog.
                    match load_catalog(self.txn.as_mut().expect("active transaction")) {
                        Ok(catalog) => self.catalog = catalog,
                        Err(error) => {
                            self.fail();
                            return Err(error);
                        }
                    }
                }
                self.check_transaction_deadline()?;
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
        self.check_transaction_deadline()?;
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

    /// Trusted server integration only. The caller's immutable authenticated
    /// scope and seed must have been accepted before the first RUN.
    #[doc(hidden)]
    pub fn commit_identified(
        &mut self,
        scope: &str,
        seed: &str,
    ) -> Result<brainwire_store::db::request_ledger::CommitReceipt, IdentifiedCommitError> {
        self.check_transaction_deadline()?;
        if !self.identified_write {
            self.fail();
            return Err(ExecError::Forbidden("identified write capability required").into());
        }
        let mut txn = self.txn.take().expect("active transaction");
        if self.wrote {
            txn.depend(Cf::Meta, b"catalog");
        }
        let result = txn
            .commit_identified(scope, seed)
            .map_err(IdentifiedCommitError::Ledger);
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
        self.check_transaction_deadline()?;
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
        let result = if self
            .transaction_deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            Err(ExecError::TimedOut)
        } else {
            result
        };
        self.reservation.take();
        self.state = match &result {
            Ok(Some(_)) => QueryTransactionState::Prepared,
            Ok(None) => QueryTransactionState::Committed,
            Err(_) => QueryTransactionState::Failed,
        };
        result
    }

    /// Account for an engine-owned control procedure using the ordinary RUN budget.
    /// Parameters have already been decoded by the transport at this boundary.
    pub fn admit_control_request(
        &mut self,
        source_bytes: usize,
        params: &BTreeMap<String, Value>,
    ) -> Result<(), ExecError> {
        self.check_transaction_deadline()?;
        self.ran = true;
        let result = (|| {
            let budget = Budget::new(self.limits, self.reservation.as_ref());
            budget.charge(source_bytes)?;
            budget.charge(budget::map_bytes(params))?;
            budget.charge(
                self.txn
                    .as_ref()
                    .expect("active transaction")
                    .estimated_memory_bytes(),
            )
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }

    /// Share the reservation with result owners without charging the bytes again.
    pub fn output_memory_lease(&self) -> QueryMemoryLease {
        QueryMemoryLease {
            _reservation: self.reservation.clone(),
            limit: self.limits.memory_bytes,
        }
    }

    /// Discard storage work while retaining conservative accounting for rows
    /// still owned by the transport. Dropping the lease releases those bytes.
    pub fn rollback_retaining_memory(&mut self) -> QueryMemoryLease {
        let lease = QueryMemoryLease {
            _reservation: self.reservation.take(),
            limit: self.limits.memory_bytes,
        };
        self.rollback();
        lease
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

/// The transaction's catalog. Reading it for update makes a write transaction
/// conflict with any catalog change that commits first.
fn load_catalog(txn: &mut Txn<'_>) -> Result<Catalog, ExecError> {
    Ok(match txn.get(Cf::Meta, b"catalog")? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Catalog(e.to_string()))?
        }
        None => Catalog {
            version: 0,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        },
    })
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

/// The `RunOutput` of a query: rows that hold graph entities and wrote nothing stay
/// handles, materialized one PULL page at a time (`QueryTransaction::fetch`).
fn finish_streaming(
    txn: &mut Txn<'_>,
    env: &run::Env<'_>,
    plan: &plan::Plan,
) -> Result<(RunOutput, bool), ExecError> {
    let (fields, rows, wrote) = run::execute_rows(txn, env, plan)?;
    if wrote || !rows.iter().any(|row| row.iter().any(val::Val::has_entity)) {
        let output = run::output_rows(&env.ctx(txn), env.budget, fields, rows)?;
        return Ok((RunOutput::Ready(output), wrote));
    }
    let bytes = rows.iter().fold(0usize, |sum, row| {
        sum.saturating_add(Budget::row_bytes(row))
    });
    Ok((
        RunOutput::Stream(QueryStream {
            fields,
            rows: rows.into(),
            bytes,
        }),
        wrote,
    ))
}

#[allow(clippy::too_many_arguments)]
fn execute_clauses_with<T>(
    txn: &mut Txn<'_>,
    catalog: &Catalog,
    query: &Query,
    params: &BTreeMap<String, Value>,
    limits: ExecutionLimits,
    source_bytes: usize,
    reservation: Option<&budget::Reservation>,
    visibility: Option<&ReadVisibility>,
    checkpoint_dir: Option<&std::path::Path>,
    finish: impl FnOnce(&mut Txn<'_>, &run::Env<'_>, &plan::Plan) -> Result<(T, bool), ExecError>,
) -> Result<(T, bool), ExecError> {
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
    // Keep procedure-level metadata restrictions even when row visibility is open.
    let calls_procedure = clauses
        .clone()
        .any(|clause| matches!(clause, Clause::Call { .. }));
    // Ordinary allow-all reads skip ACL decoding while retaining the write guard above.
    let visibility = visibility.filter(|policy| !policy.sees_everything() || calls_procedure);
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
            partition: None,
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
    finish(txn, &env, &plan)
}

#[cfg(test)]
mod embedded_output_tests {
    use super::*;
    use brainwire_store::db::{Database, Resources, StoreConfig};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    #[test]
    fn embedded_output_leases_share_capacity_and_release_credit() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let transaction = QueryTransaction::begin_with_limits(
            &database,
            ExecutionLimits {
                memory_bytes: 100,
                timeout: None,
            },
        )
        .unwrap();
        let lease = transaction.output_memory_lease();
        let other = transaction.output_memory_lease();
        let first = lease.reserve_output(60).unwrap();
        assert!(matches!(
            other.reserve_output(60),
            Err(ExecError::MemoryLimit {
                limit: 100,
                attempted: 120
            })
        ));
        drop(first);
        let second = other.reserve_output(60).unwrap();
        drop(second);
        lease.reserve_output(100).unwrap();
    }

    #[test]
    fn embedded_output_reservations_are_bounded_under_concurrency() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let transaction = QueryTransaction::begin_with_limits(
            &database,
            ExecutionLimits {
                memory_bytes: 100,
                timeout: None,
            },
        )
        .unwrap();
        let lease = transaction.output_memory_lease();
        let barrier = Arc::new(Barrier::new(9));
        let accepted = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let lease = lease.clone();
                let barrier = Arc::clone(&barrier);
                let accepted = &accepted;
                scope.spawn(move || {
                    barrier.wait();
                    let output = lease.reserve_output(60);
                    if output.is_ok() {
                        accepted.fetch_add(1, Ordering::Relaxed);
                    }
                    barrier.wait();
                    barrier.wait();
                    drop(output);
                });
            }
            barrier.wait();
            barrier.wait();
            let count = accepted.load(Ordering::Relaxed);
            barrier.wait();
            assert_eq!(count, 1);
        });
        lease.reserve_output(100).unwrap();
    }

    #[test]
    fn private_output_budget_spans_runs_and_survives_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let mut transaction = QueryTransaction::begin_with_limits(
            &database,
            ExecutionLimits {
                memory_bytes: 32_768,
                timeout: None,
            },
        )
        .unwrap();
        let params = BTreeMap::from([("text".into(), Value::Str("x".repeat(1500)))]);
        let first = transaction.run("RETURN $text", &params).unwrap();
        let lease = transaction.output_memory_lease();
        let first_credit = lease.reserve_output(20_000).unwrap();
        let second = transaction.run("RETURN $text", &params).unwrap();
        assert_eq!(first.rows, second.rows);
        transaction.rollback();
        assert_eq!(transaction.state(), QueryTransactionState::RolledBack);
        drop(transaction);
        assert!(lease.reserve_output(20_000).is_err());
        drop(first_credit);
        lease.reserve_output(20_000).unwrap();
        // The result owner keeps the execution high-water until its lease drops.
        assert!(lease.reserve_output(32_768).is_err());
    }

    #[test]
    fn private_pool_does_not_change_shared_pool_observability() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            Database::open(directory.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let query = "CALL db.memory() YIELD poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections,processHeadroomBytes RETURN poolLimitBytes,poolReservedBytes,poolPeakBytes,poolRejections,processHeadroomBytes";
        for limits in [
            ExecutionLimits::default(),
            ExecutionLimits {
                memory_bytes: 32_768,
                timeout: None,
            },
        ] {
            let mut transaction = QueryTransaction::begin_with_limits(&database, limits).unwrap();
            let output = transaction.run(query, &BTreeMap::new()).unwrap();
            assert_eq!(output.rows, vec![vec![Out::Value(Value::Null); 5]]);
        }
        let pool = Arc::new(QueryMemoryPool::new(1 << 20));
        let mut transaction =
            QueryTransaction::begin_with_pool(&database, ExecutionLimits::default(), pool).unwrap();
        let output = transaction.run(query, &BTreeMap::new()).unwrap();
        assert_eq!(output.rows[0][0], Out::Value(Value::Int(1 << 20)));
    }
}
