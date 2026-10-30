//! The Bolt 5.1+ server state machine, independent of the network.

use std::collections::VecDeque;
use std::time::Duration;

use crate::auth::AccessContext;
pub use crate::auth::AuthMode;
use crate::message::{Request, Response};
use crate::packstream::PackValue;
use brainwire_core::acl::ReadVisibility;

/// The `server` value of the HELLO answer. Driver 5.x refuses a server whose
/// agent does not start with `Neo4j/`; the rest says what Brainwire is, so it
/// never passes for Neo4j itself (Memgraph uses the same form).
const SERVER_AGENT: &str = "Neo4j/5.26.0 compatible graph database server - Brainwire";

/// Opaque accounting ownership shared with an engine without a transport dependency.
#[derive(Clone)]
pub struct OutputMemory {
    _owner: std::sync::Arc<dyn Send + Sync>,
}
impl OutputMemory {
    pub fn new(value: impl Send + Sync + 'static) -> Self {
        Self {
            _owner: std::sync::Arc::new(value),
        }
    }
}
impl std::fmt::Debug for OutputMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputMemory")
    }
}

/// Responses retain their result accounting until consumed or dropped.
/// The owned iterator must stay alive while each yielded response is processed.
#[derive(Debug)]
pub struct ResponseBatch {
    responses: Vec<Response>,
    memory: Option<OutputMemory>,
}
impl std::ops::Deref for ResponseBatch {
    type Target = [Response];
    fn deref(&self) -> &Self::Target {
        &self.responses
    }
}
impl PartialEq<Vec<Response>> for ResponseBatch {
    fn eq(&self, other: &Vec<Response>) -> bool {
        self.responses == *other
    }
}
pub struct ResponseIter {
    responses: std::vec::IntoIter<Response>,
    _memory: Option<OutputMemory>,
}
impl Iterator for ResponseIter {
    type Item = Response;
    fn next(&mut self) -> Option<Self::Item> {
        self.responses.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.responses.size_hint()
    }
}
impl IntoIterator for ResponseBatch {
    type Item = Response;
    type IntoIter = ResponseIter;
    fn into_iter(self) -> Self::IntoIter {
        ResponseIter {
            responses: self.responses.into_iter(),
            _memory: self.memory,
        }
    }
}

pub struct QueryResult {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<PackValue>>,
    /// Engines without shared accounting leave this unset.
    pub memory: Option<OutputMemory>,
}

#[derive(Debug)]
pub struct EngineError {
    pub code: String,
    pub message: String,
}

/// The one memory-refusal code (docs/design/memory-error-contract.md): nothing of the
/// transaction was applied, so the caller may retry it, smaller or later.
pub const MEMORY_REFUSED: &str = "Neo.TransientError.General.MemoryPressure";

impl EngineError {
    /// A memory refusal with its machine-readable size hint. `reason` is one of
    /// `statement_limit`, `query_pool`, `process_limit`, `cgroup`, `ingress`.
    pub fn memory_refused(reason: &str, requested: u64, available: u64, detail: &str) -> Self {
        EngineError {
            code: MEMORY_REFUSED.into(),
            message: format!(
                "{detail}; the transaction was rolled back and may be retried smaller \
                 [brainwire.memory reason={reason} requested_bytes={requested} \
                 available_bytes={available} rolled_back=true]"
            ),
        }
    }
}

fn durable_request_id(extra: &PackValue) -> Result<Option<&str>, Response> {
    if matches!(extra, PackValue::Map(fields) if fields.iter().filter(|(key,_)| key == "tx_metadata").count() > 1)
    {
        return Err(invalid("duplicate transaction metadata"));
    }
    let Some(metadata) = extra.get("tx_metadata") else {
        return Ok(None);
    };
    let PackValue::Map(fields) = metadata else {
        return Err(invalid("transaction metadata must be a map"));
    };
    if fields
        .iter()
        .filter(|(key, _)| key == "brainwire_request_id")
        .count()
        > 1
    {
        return Err(invalid("duplicate durable request identity"));
    }
    match metadata.get("brainwire_request_id") {
        None => Ok(None),
        Some(PackValue::String(id))
            if !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_:.".contains(&c)) =>
        {
            Ok(Some(id))
        }
        _ => Err(invalid(
            "brainwire_request_id must be an ASCII identifier of 1..128 bytes",
        )),
    }
}

/// The caller visibility a delegating reader sends for this transaction
/// (`tx_metadata.brainwire_visibility`), validated like a static policy.
fn delegated_visibility(extra: &PackValue) -> Result<Option<ReadVisibility>, Response> {
    let Some(metadata) = extra.get("tx_metadata") else {
        return Ok(None);
    };
    if matches!(metadata, PackValue::Map(fields) if fields.iter().filter(|(key, _)| key == VISIBILITY_KEY).count() > 1)
    {
        return Err(invalid("duplicate delegated visibility"));
    }
    let Some(value) = metadata.get(VISIBILITY_KEY) else {
        return Ok(None);
    };
    let PackValue::Map(_) = value else {
        return Err(invalid("brainwire_visibility must be a map"));
    };
    let json = value
        .to_json()
        .ok_or_else(|| invalid("brainwire_visibility holds a value that is not JSON"))?;
    let visibility: ReadVisibility = serde_json::from_value(json)
        .map_err(|_| invalid("brainwire_visibility is not a valid visibility"))?;
    visibility
        .validate()
        .map_err(|_| invalid("brainwire_visibility is not a valid visibility"))?;
    Ok(Some(visibility))
}

fn transaction_timeout(extra: &PackValue) -> Result<Option<Duration>, Response> {
    if matches!(extra, PackValue::Map(fields) if fields.iter().filter(|(name, _)| name == "tx_timeout").count() > 1)
    {
        return Err(invalid("duplicate transaction timeout"));
    }
    match extra.get("tx_timeout") {
        None | Some(PackValue::Int(0)) => Ok(None),
        Some(PackValue::Int(ms)) if *ms > 0 => Ok(Some(Duration::from_millis(*ms as u64))),
        _ => Err(invalid(
            "tx_timeout must be a nonnegative integer in milliseconds",
        )),
    }
}

const VISIBILITY_KEY: &str = "brainwire_visibility";

pub trait QueryEngine {
    /// Engines must explicitly support protected identities; legacy engines fail closed.
    fn set_access(&mut self, access: AccessContext) -> Result<(), EngineError> {
        match access {
            AccessContext::Unrestricted => Ok(()),
            _ => Err(EngineError {
                code: "Neo.ClientError.Security.Forbidden".into(),
                message: "engine does not support protected access".into(),
            }),
        }
    }
    /// Begin one transaction, bound to this database until completion.
    /// A failure may leave partial state: the session will call `abort`.
    fn begin(&mut self, db: Option<&str>) -> Result<(), EngineError>;
    /// Legacy engines must not silently treat a READ transaction as writable.
    fn begin_read_only(&mut self, _db: Option<&str>) -> Result<(), EngineError> {
        Err(EngineError {
            code: "Neo.ClientError.Security.Forbidden".into(),
            message: "engine does not support read-only transactions".into(),
        })
    }
    /// Optional durable request identity; unsupported engines reject it explicitly.
    fn begin_with_request_id(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
    ) -> Result<(), EngineError> {
        if request_id.is_some() {
            return Err(EngineError {
                code: "Neo.ClientError.Request.Invalid".into(),
                message: "engine does not support durable request identities".into(),
            });
        }
        if read_only {
            self.begin_read_only(db)
        } else {
            self.begin(db)
        }
    }
    /// Begin a read transaction filtered by a visibility the authenticated principal
    /// delegates for its caller. Engines without delegation refuse it.
    fn begin_delegated(
        &mut self,
        _db: Option<&str>,
        _read_only: bool,
        _request_id: Option<&str>,
        _visibility: ReadVisibility,
    ) -> Result<(), EngineError> {
        Err(EngineError {
            code: "Neo.ClientError.Security.Forbidden".into(),
            message: "engine does not support delegated visibility".into(),
        })
    }
    /// Positive client timeout, bounded by the engine's server ceiling.
    /// Legacy engines reject requested timeouts instead of silently ignoring them.
    fn begin_with_timeout(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
        visibility: Option<ReadVisibility>,
        timeout: Option<Duration>,
    ) -> Result<(), EngineError> {
        if timeout.is_some() {
            return Err(EngineError {
                code: "Neo.ClientError.Statement.NotSupported".into(),
                message: "engine does not support transaction timeouts".into(),
            });
        }
        match visibility {
            Some(visibility) => self.begin_delegated(db, read_only, request_id, visibility),
            None => self.begin_with_request_id(db, read_only, request_id),
        }
    }
    /// Begin the implicit transaction of one autocommit RUN. Engines may use
    /// the distinction to restrict fresh-view administrative/status procedures.
    fn begin_autocommit_with_timeout(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
        visibility: Option<ReadVisibility>,
        timeout: Option<Duration>,
    ) -> Result<(), EngineError> {
        self.begin_with_timeout(db, read_only, request_id, visibility, timeout)
    }
    /// Check before delivering buffered output from an active transaction.
    fn check_transaction(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
    /// Commit and optionally return an engine-owned bookmark.
    /// Return success only after completion; a failure is followed by `abort`.
    fn commit(&mut self) -> Result<Option<String>, EngineError>;
    /// Discard pending work. Implementations must make repeated calls safe.
    fn abort(&mut self) -> Result<(), EngineError>;
    fn run(
        &mut self,
        db: Option<&str>,
        query: &str,
        params: &PackValue,
    ) -> Result<QueryResult, EngineError>;
    /// Rows of the last RUN the engine still holds after its `QueryResult` rows:
    /// the session asks for them with [`QueryEngine::pull`]. Zero for engines that
    /// answer RUN with every row.
    fn deferred_rows(&self) -> usize {
        0
    }
    /// Up to *n* more of the last RUN's deferred rows, or drop *n* of them unread
    /// when *discard*. Only called while [`QueryEngine::deferred_rows`] is nonzero.
    /// Transfer accounting for the rows returned by the last pull. It follows
    /// those rows through the session and ResponseBatch until written or dropped.
    fn take_pull_memory(&mut self) -> Option<OutputMemory> {
        None
    }
    /// Actual rows consumed by the most recent pull, including discarded rows.
    /// Lazy engines must report this because their remaining size is unknown.
    fn last_pull_consumed(&self) -> Option<usize> {
        None
    }
    fn pull(&mut self, _n: usize, _discard: bool) -> Result<Vec<Vec<PackValue>>, EngineError> {
        Err(EngineError {
            code: "Neo.ClientError.Request.Invalid".into(),
            message: "engine has no deferred rows".into(),
        })
    }
}

/// Deferred rows the session asks the engine for at once: the memory one PULL
/// materializes, whatever the client's `n`.
const PULL_PAGE: usize = 1000;

/// Answers `RETURN <integer> [AS <name>]` only. The Cypher plan replaces it.
pub struct StubEngine;

impl QueryEngine for StubEngine {
    fn set_access(&mut self, access: AccessContext) -> Result<(), EngineError> {
        match access {
            AccessContext::Unrestricted | AccessContext::Unauthenticated => Ok(()),
            AccessContext::Authenticated(_) => Err(EngineError {
                code: "Neo.ClientError.Security.Forbidden".into(),
                message: "stub engine does not support protected access".into(),
            }),
        }
    }
    // Synthetic results have no persistent state or durable bookmark.
    fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
        Ok(())
    }
    fn begin_read_only(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        self.begin(db)
    }
    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        Ok(None)
    }
    fn abort(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
    fn run(
        &mut self,
        _db: Option<&str>,
        query: &str,
        _params: &PackValue,
    ) -> Result<QueryResult, EngineError> {
        let refuse = || EngineError {
            code: "Neo.ClientError.Statement.SyntaxError".into(),
            message: "the stub engine only answers RETURN <integer> [AS <name>]".into(),
        };
        let words: Vec<&str> = query.split_whitespace().collect();
        let (value, name) = match words.as_slice() {
            [r, v] if r.eq_ignore_ascii_case("RETURN") => (*v, *v),
            [r, v, a, n] if r.eq_ignore_ascii_case("RETURN") && a.eq_ignore_ascii_case("AS") => {
                (*v, *n)
            }
            _ => return Err(refuse()),
        };
        let int: i64 = value.parse().map_err(|_| refuse())?;
        Ok(QueryResult {
            memory: None,
            fields: vec![name.to_string()],
            rows: vec![vec![PackValue::Int(int)]],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connected,
    Authentication,
    Ready,
    Streaming,
    TxReady,
    TxStreaming,
    Failed,
    Defunct,
}

pub struct Session<E: QueryEngine> {
    engine: E,
    state: State,
    rows: VecDeque<Vec<PackValue>>,
    /// Rows of the current result the engine still holds (`QueryEngine::pull`).
    deferred: usize,
    output_memory: Option<OutputMemory>,
    pull_memory: Option<OutputMemory>,
    response_memory: Vec<OutputMemory>,
    /// Socket transport yields after one real engine page, even below PULL_PAGE.
    transport_page: bool,
    db: Option<String>,
    pending: bool,
    auth: AuthMode,
}

fn meta(pairs: Vec<(&str, PackValue)>) -> Response {
    Response::Success(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn invalid(message: &str) -> Response {
    Response::Failure {
        code: "Neo.ClientError.Request.Invalid".into(),
        message: message.into(),
    }
}

impl<E: QueryEngine> Session<E> {
    pub fn new(engine: E, auth: AuthMode) -> Session<E> {
        Session {
            engine,
            state: State::Connected,
            rows: VecDeque::new(),
            deferred: 0,
            output_memory: None,
            pull_memory: None,
            response_memory: Vec::new(),
            transport_page: false,
            db: None,
            pending: false,
            auth,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn cleanup(&mut self) -> Result<(), EngineError> {
        self.rows.clear();
        self.deferred = 0;
        self.output_memory.take();
        self.pull_memory.take();
        if self.pending {
            self.engine.abort()?;
            self.pending = false;
        }
        self.db = None;
        Ok(())
    }

    fn engine_failure(error: EngineError) -> Response {
        Response::Failure {
            code: error.code,
            message: error.message,
        }
    }

    fn fail(&mut self, response: Response) -> Vec<Response> {
        match self.cleanup() {
            Ok(()) => {
                self.state = State::Failed;
                vec![response]
            }
            Err(error) => {
                self.state = State::Defunct;
                vec![Self::engine_failure(error)]
            }
        }
    }

    /// A request refused before it reached the engine (input memory): the session fails
    /// like any statement failure, rolling back its transaction, and RESET recovers it.
    pub fn refuse(&mut self, error: EngineError) -> Vec<Response> {
        if self.state == State::Failed {
            return vec![Response::Ignored];
        }
        self.fail(Self::engine_failure(error))
    }

    fn defunct(&mut self, response: Response) -> Vec<Response> {
        let out = self.fail(response);
        self.state = State::Defunct;
        out
    }

    /// Release pending engine state before a transport-level failure is written.
    pub(crate) fn close(&mut self) {
        let _ = self.cleanup();
        self.state = State::Defunct;
    }

    fn revoke(&mut self) -> Result<(), EngineError> {
        let cleanup = self.cleanup();
        // Revoke even if abort fails; the connection will close in that case.
        let revoke = self.engine.set_access(AccessContext::Unauthenticated);
        cleanup.and(revoke)
    }

    fn logon(&mut self, auth: &PackValue) -> Vec<Response> {
        let access = match &self.auth {
            AuthMode::AllowAll => AccessContext::Unrestricted,
            AuthMode::Required(policy) => match policy.authenticate(auth) {
                Ok(principal) => AccessContext::Authenticated(principal),
                Err(_) => {
                    let _ = self.revoke();
                    self.state = State::Defunct;
                    return vec![Response::Failure {
                        code: "Neo.ClientError.Security.Unauthorized".into(),
                        message: "authentication failed".into(),
                    }];
                }
            },
        };
        match self.engine.set_access(access) {
            Ok(()) => {
                self.state = State::Ready;
                vec![meta(vec![])]
            }
            Err(error) => {
                let _ = self.revoke();
                self.state = State::Defunct;
                vec![Self::engine_failure(error)]
            }
        }
    }

    fn database(extra: &PackValue) -> Result<Option<String>, Response> {
        if extra.get("imp_user").is_some() {
            return Err(invalid("impersonation is not supported"));
        }
        match extra.get("db") {
            None => Ok(None),
            Some(PackValue::String(db)) => Ok(Some(db.clone())),
            _ => Err(invalid("db must be a string")),
        }
    }

    fn begin(&mut self, extra: &PackValue, autocommit: bool) -> Result<(), Response> {
        self.db = Self::database(extra)?;
        if matches!(extra, PackValue::Map(fields) if fields.iter().filter(|(name, _)| name == "mode").count() > 1)
        {
            return Err(invalid("duplicate transaction mode"));
        }
        let read_only = match extra.get("mode") {
            None => false,
            Some(PackValue::String(mode)) if mode == "w" => false,
            Some(PackValue::String(mode)) if mode == "r" => true,
            _ => return Err(invalid("mode must be r or w")),
        };
        let request_id = durable_request_id(extra)?;
        let visibility = delegated_visibility(extra)?;
        let timeout = transaction_timeout(extra)?;
        // A failed begin may still own partially initialized engine state.
        self.pending = true;
        let result = if autocommit {
            self.engine.begin_autocommit_with_timeout(
                self.db.as_deref(),
                read_only,
                request_id,
                visibility,
                timeout,
            )
        } else {
            self.engine.begin_with_timeout(
                self.db.as_deref(),
                read_only,
                request_id,
                visibility,
                timeout,
            )
        };
        result.map_err(Self::engine_failure)
    }

    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        let bookmark = self.engine.commit()?;
        self.pending = false;
        self.db = None;
        Ok(bookmark)
    }

    fn start_query(&mut self, query: &str, params: &PackValue, in_tx: bool) -> Vec<Response> {
        match self.engine.run(self.db.as_deref(), query, params) {
            Ok(result) => {
                self.rows = result.rows.into();
                self.deferred = self.engine.deferred_rows();
                self.output_memory = result.memory;
                self.state = if in_tx {
                    State::TxStreaming
                } else {
                    State::Streaming
                };
                let fields =
                    PackValue::List(result.fields.into_iter().map(PackValue::String).collect());
                let mut pairs = vec![("fields", fields), ("t_first", PackValue::Int(0))];
                if in_tx {
                    pairs.push(("qid", PackValue::Int(0)));
                }
                vec![meta(pairs)]
            }
            Err(e) => self.fail(Response::Failure {
                code: e.code,
                message: e.message,
            }),
        }
    }

    fn pull(&mut self, extra: &PackValue, discard: bool) -> Vec<Response> {
        if let Err(error) = self.engine.check_transaction() {
            return self.fail(Self::engine_failure(error));
        }
        let n = match extra.get("n") {
            None | Some(PackValue::Int(-1)) => usize::MAX,
            Some(PackValue::Int(n)) if *n >= 0 => usize::try_from(*n).unwrap_or(usize::MAX),
            _ => return self.fail(invalid("n must be -1 or a nonnegative integer")),
        };
        let mut out = Vec::new();
        let unlimited = n == usize::MAX;
        let mut left = n;
        loop {
            while left > 0 {
                let Some(row) = self.rows.pop_front() else {
                    break;
                };
                if !discard {
                    out.push(Response::Record(row));
                }
                if !unlimited {
                    left -= 1;
                }
            }
            if left == 0 || self.deferred == 0 {
                break;
            }
            if self.transport_page && !discard && !out.is_empty() {
                break;
            }
            // Deferred rows are materialized a page at a time; discarded ones never.
            let ask = if discard {
                left.min(self.deferred)
            } else {
                left.min(self.deferred).min(PULL_PAGE)
            };
            let before = self.deferred;
            match self.engine.pull(ask, discard) {
                Ok(rows) => {
                    // The engine says what is left: a lazy result does not know its size.
                    self.deferred = self.engine.deferred_rows();
                    if discard {
                        if !unlimited {
                            let consumed = self.engine.last_pull_consumed().or_else(|| {
                                (before != usize::MAX && self.deferred != usize::MAX)
                                    .then(|| before.checked_sub(self.deferred))
                                    .flatten()
                                    .filter(|count| *count <= ask)
                            });
                            match consumed {
                                Some(count)
                                    if count <= ask && (count > 0 || self.deferred == 0) =>
                                {
                                    left -= count
                                }
                                _ => {
                                    return self.fail(Self::engine_failure(EngineError {
                                        code: "Neo.DatabaseError.General.UnknownError".into(),
                                        message: "engine did not report discarded row progress"
                                            .into(),
                                    }))
                                }
                            }
                        }
                        self.engine.take_pull_memory();
                        continue;
                    }
                    self.pull_memory = self.engine.take_pull_memory();
                    if let Some(memory) = &self.pull_memory {
                        self.response_memory.push(memory.clone());
                    }
                    self.rows.extend(rows);
                }
                Err(error) => {
                    out.extend(self.fail(Self::engine_failure(error)));
                    return out;
                }
            }
        }
        // The response owns leases of delivered rows. Keep a session copy only
        // while this page still has buffered rows, not into the next PULL.
        if self.rows.is_empty() {
            self.pull_memory.take();
        }
        let in_tx = self.state == State::TxStreaming;
        if self.rows.is_empty() && self.deferred == 0 {
            self.output_memory.take();
            self.pull_memory.take();
            let mut pairs = vec![
                ("type", PackValue::String("r".into())),
                ("t_last", PackValue::Int(0)),
            ];
            if !in_tx {
                match self.commit() {
                    Ok(Some(bookmark)) => pairs.push(("bookmark", PackValue::String(bookmark))),
                    Ok(None) => {}
                    Err(error) => {
                        out.extend(self.fail(Self::engine_failure(error)));
                        return out;
                    }
                }
            }
            self.state = if in_tx { State::TxReady } else { State::Ready };
            out.push(meta(pairs));
        } else {
            out.push(meta(vec![("has_more", PackValue::Bool(true))]));
        }
        out
    }

    pub fn handle(&mut self, req: Request) -> ResponseBatch {
        // Retain before PULL: commit or failure may clear Session's own lease.
        let memory = self.output_memory.clone();
        let previous_page = self.pull_memory.clone();
        let responses = self.handle_inner(req);
        let pages = std::mem::take(&mut self.response_memory);
        let memory = responses
            .iter()
            .any(|r| matches!(r, Response::Record(_)))
            .then(|| OutputMemory::new((memory, previous_page, pages)));
        ResponseBatch { responses, memory }
    }

    /// Bound each transport batch even when the client asks for every remaining row.
    /// The transport writes this batch before resuming `continuation`; intermediate
    /// has_more summaries are internal, so one wire PULL still has one summary.
    pub(crate) fn handle_page(&mut self, mut req: Request) -> (ResponseBatch, Option<Request>) {
        let remaining = match &mut req {
            Request::Pull {
                extra: PackValue::Map(fields),
            } => {
                let remaining = match fields
                    .iter()
                    .find(|(name, _)| name == "n")
                    .map(|(_, value)| value)
                {
                    None | Some(PackValue::Int(-1)) => usize::MAX,
                    Some(PackValue::Int(n)) if *n >= 0 => usize::try_from(*n).unwrap_or(usize::MAX),
                    _ => return (self.handle(req), None),
                };
                if let Some((_, value)) = fields.iter_mut().find(|(name, _)| name == "n") {
                    *value = PackValue::Int(remaining.min(PULL_PAGE) as i64);
                } else {
                    fields.push(("n".into(), PackValue::Int(PULL_PAGE as i64)));
                }
                Some(remaining)
            }
            _ => None,
        };
        if remaining.is_none() {
            return (self.handle(req), None);
        }
        self.transport_page = true;
        let mut batch = self.handle(req.clone());
        self.transport_page = false;
        let delivered = batch
            .responses
            .iter()
            .filter(|r| matches!(r, Response::Record(_)))
            .count();
        let more = matches!(batch.responses.last(), Some(Response::Success(extra))
            if extra.iter().any(|(name, value)| name == "has_more" && value == &PackValue::Bool(true)));
        if let Some(remaining) = remaining.filter(|n| *n > delivered && more && delivered > 0) {
            let left = if remaining == usize::MAX {
                -1
            } else {
                (remaining - delivered) as i64
            };
            if let Request::Pull {
                extra: PackValue::Map(fields),
            } = &mut req
            {
                fields.iter_mut().find(|(name, _)| name == "n").unwrap().1 = PackValue::Int(left);
            }
            batch.responses.pop();
            (batch, Some(req))
        } else {
            (batch, None)
        }
    }

    fn handle_inner(&mut self, req: Request) -> Vec<Response> {
        use State::*;
        match (self.state, req) {
            (Defunct, _) => vec![],
            (_, Request::Goodbye) => {
                self.close();
                vec![]
            }
            (Connected, Request::Hello { .. }) => {
                self.state = Authentication;
                vec![meta(vec![
                    ("server", PackValue::String(SERVER_AGENT.into())),
                    ("connection_id", PackValue::String("bw-1".into())),
                    (
                        "hints",
                        PackValue::Map(vec![(
                            "brainwire.version".into(),
                            PackValue::String(env!("CARGO_PKG_VERSION").into()),
                        )]),
                    ),
                ])]
            }
            // HELLO is only ever valid right after the handshake (research/
            // bolt.md 3.5: "State: only in NEGOTIATION. In any other state
            // it is a violation and closes the connection."); a second
            // HELLO -- even from Ready -- is fatal, not merely refused.
            (_, Request::Hello { .. }) => {
                self.defunct(invalid("HELLO is only valid right after the handshake"))
            }
            (Authentication, Request::Logon { auth }) => self.logon(&auth),
            (_, Request::Logon { .. }) => {
                let _ = self.revoke();
                self.state = Defunct;
                vec![invalid("LOGON is only valid in authentication state")]
            }
            (Ready | Streaming | TxReady | TxStreaming | Failed, Request::Logoff) => {
                match self.revoke() {
                    Ok(()) => {
                        self.state = Authentication;
                        vec![meta(vec![])]
                    }
                    Err(error) => {
                        self.state = Defunct;
                        vec![Self::engine_failure(error)]
                    }
                }
            }
            // Anything else while the session has not logged on yet is a
            // protocol violation, and unlike the same violation post-login it
            // is fatal (3.1, 3.5, 6.6): no RESET recovers a connection that
            // never reached READY, so it can never land in Failed here, only
            // Defunct. This must stay ahead of the generic RESET/ROUTE/
            // catch-all arms below, which are for states that already
            // reached Ready at least once.
            (Connected | Authentication, _) => self.defunct(invalid(
                "a session that has not logged on accepts only HELLO, LOGON and GOODBYE",
            )),
            (_, Request::Reset) => match self.cleanup() {
                Ok(()) => {
                    self.state = Ready;
                    vec![meta(vec![])]
                }
                Err(error) => {
                    self.state = Defunct;
                    vec![Self::engine_failure(error)]
                }
            },
            (Failed, _) => vec![Response::Ignored],
            (_, Request::Route) => self.fail(invalid(
                "routing arrives with the cluster plan; use bolt://",
            )),
            (Ready | TxReady, Request::Telemetry) => vec![meta(vec![])],
            (
                Ready,
                Request::Run {
                    query,
                    params,
                    extra,
                },
            ) => {
                if let Err(response) = self.begin(&extra, true) {
                    return self.fail(response);
                }
                self.start_query(&query, &params, false)
            }
            (Ready, Request::Begin { extra }) => {
                if let Err(response) = self.begin(&extra, false) {
                    return self.fail(response);
                }
                self.state = TxReady;
                vec![meta(vec![])]
            }
            (
                TxReady,
                Request::Run {
                    query,
                    params,
                    extra,
                },
            ) => {
                match Self::database(&extra) {
                    Err(response) => return self.fail(response),
                    Ok(Some(db)) if self.db.as_deref() != Some(db.as_str()) => {
                        return self.fail(invalid("RUN cannot change the transaction database"));
                    }
                    _ => {}
                }
                if extra.get("tx_timeout").is_some() {
                    return self.fail(invalid(
                        "tx_timeout is set by BEGIN, not RUN inside a transaction",
                    ));
                }
                self.start_query(&query, &params, true)
            }
            (Streaming | TxStreaming, Request::Pull { extra }) => self.pull(&extra, false),
            (Streaming | TxStreaming, Request::Discard { extra }) => self.pull(&extra, true),
            (TxReady, Request::Commit) => match self.commit() {
                Ok(bookmark) => {
                    self.state = Ready;
                    vec![meta(
                        bookmark
                            .into_iter()
                            .map(|b| ("bookmark", PackValue::String(b)))
                            .collect(),
                    )]
                }
                Err(error) => self.fail(Self::engine_failure(error)),
            },
            (TxReady, Request::Rollback) => match self.cleanup() {
                Ok(()) => {
                    self.state = Ready;
                    vec![meta(vec![])]
                }
                Err(error) => {
                    self.state = Defunct;
                    vec![Self::engine_failure(error)]
                }
            },
            (_, _) => self.fail(invalid("message not allowed in the current state")),
        }
    }
}

impl<E: QueryEngine> Drop for Session<E> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemoryOwner(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for MemoryOwner {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct DeferredMemory {
        remaining: usize,
        page_size: usize,
        occupied: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        memory: Option<OutputMemory>,
    }
    impl QueryEngine for DeferredMemory {
        fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<QueryResult, EngineError> {
            self.remaining = 4;
            Ok(QueryResult {
                fields: vec!["x".into()],
                rows: vec![],
                memory: None,
            })
        }
        fn deferred_rows(&self) -> usize {
            self.remaining
        }
        fn pull(&mut self, n: usize, _: bool) -> Result<Vec<Vec<PackValue>>, EngineError> {
            let count = n.min(self.remaining).min(self.page_size);
            self.remaining -= count;
            self.occupied
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.memory = Some(OutputMemory::new(MemoryOwner(self.occupied.clone())));
            Ok(vec![vec![PackValue::Int(1)]; count])
        }
        fn take_pull_memory(&mut self) -> Option<OutputMemory> {
            self.memory.take()
        }
        fn commit(&mut self) -> Result<Option<String>, EngineError> {
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), EngineError> {
            self.remaining = 0;
            self.memory.take();
            Ok(())
        }
    }

    struct PagedDiscard {
        next: usize,
        consumed: usize,
        report_progress: bool,
        initial_row: bool,
        commits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl QueryEngine for PagedDiscard {
        fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<QueryResult, EngineError> {
            self.next = usize::from(self.initial_row);
            Ok(QueryResult {
                fields: vec!["x".into()],
                rows: if self.initial_row {
                    vec![vec![PackValue::Int(0)]]
                } else {
                    vec![]
                },
                memory: None,
            })
        }
        fn deferred_rows(&self) -> usize {
            if self.next < 5 {
                usize::MAX
            } else {
                0
            }
        }
        fn pull(&mut self, n: usize, discard: bool) -> Result<Vec<Vec<PackValue>>, EngineError> {
            if discard && n == usize::MAX {
                self.next = 5;
                self.consumed = 0;
                return Ok(vec![]);
            }
            self.consumed = n.min(1).min(5 - self.next);
            let row = self.next as i64;
            self.next += self.consumed;
            Ok(if discard || self.consumed == 0 {
                vec![]
            } else {
                vec![vec![PackValue::Int(row)]]
            })
        }
        fn last_pull_consumed(&self) -> Option<usize> {
            self.report_progress.then_some(self.consumed)
        }
        fn commit(&mut self) -> Result<Option<String>, EngineError> {
            self.commits
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), EngineError> {
            self.next = 5;
            Ok(())
        }
    }

    #[test]
    fn finite_discard_consumes_only_requested_lazy_rows_without_early_commit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let commits = std::sync::Arc::new(AtomicUsize::new(0));
        let mut session = ready(PagedDiscard {
            next: 0,
            consumed: 0,
            report_progress: true,
            initial_row: false,
            commits: commits.clone(),
        });
        session.handle(run("RETURN x"));
        session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(0))]),
        });
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        let response = session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(3))]),
        });
        assert!(
            matches!(&response[0], Response::Success(meta) if meta.iter().any(|(key, value)| key == "has_more" && *value == PackValue::Bool(true)))
        );
        assert_eq!(session.state(), State::Streaming);
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        let next = session.handle(pull(1));
        assert_eq!(next[0], Response::Record(vec![PackValue::Int(3)]));
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        session.handle(pull(-1));
        assert_eq!(commits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn finite_lazy_discard_fails_closed_without_consumed_count() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let commits = std::sync::Arc::new(AtomicUsize::new(0));
        let mut session = ready(PagedDiscard {
            next: 0,
            consumed: 0,
            report_progress: false,
            initial_row: false,
            commits: commits.clone(),
        });
        session.handle(run("RETURN x"));
        let response = session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(3))]),
        });
        assert!(matches!(&response[0], Response::Failure { .. }));
        assert_eq!(session.state(), State::Failed);
        assert_eq!(commits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unlimited_discard_preserves_cancellation_after_buffered_rows() {
        let mut session = ready(PagedDiscard {
            next: 0,
            consumed: 0,
            report_progress: true,
            initial_row: true,
            commits: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        session.handle(run("RETURN x"));
        session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(-1))]),
        });
        assert_eq!(session.state(), State::Ready);
        assert_eq!(
            session.engine.consumed, 0,
            "unlimited discard enumerated the lazy tail"
        );
    }

    #[test]
    fn finite_discard_above_lazy_cardinality_finishes() {
        let mut session = ready(PagedDiscard {
            next: 0,
            consumed: 0,
            report_progress: true,
            initial_row: false,
            commits: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        session.handle(run("RETURN x"));
        let response = session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(8))]),
        });
        assert!(matches!(&response[0], Response::Success(_)));
        assert_eq!(session.state(), State::Ready);
    }

    #[test]
    fn finite_discard_uses_known_cardinality_for_legacy_engines() {
        let mut session = ready(DeferredMemory {
            remaining: 0,
            page_size: 1,
            occupied: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            memory: None,
        });
        session.handle(run("RETURN x"));
        session.handle(Request::Discard {
            extra: map(&[("n", PackValue::Int(2))]),
        });
        assert_eq!(session.state(), State::Streaming);
        assert_eq!(session.engine.remaining, 2);
        assert_eq!(
            session
                .handle(pull(-1))
                .iter()
                .filter(|r| matches!(r, Response::Record(_)))
                .count(),
            2
        );
    }

    #[test]
    fn transport_flushes_small_engine_pages_before_fetching_another() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for requested in [1000, -1] {
            let occupied = std::sync::Arc::new(AtomicUsize::new(0));
            let mut session = ready(DeferredMemory {
                remaining: 0,
                page_size: 1,
                occupied: occupied.clone(),
                memory: None,
            });
            session.handle(run("RETURN x"));
            let mut request = Some(pull(requested));
            let mut count = 0;
            let mut summaries = 0;
            while let Some(next) = request {
                let (batch, continuation) = session.handle_page(next);
                let records = batch
                    .iter()
                    .filter(|response| matches!(response, Response::Record(_)))
                    .count();
                assert_eq!(records, 1, "transport retained multiple engine pages");
                assert_eq!(occupied.load(Ordering::SeqCst), 1);
                count += records;
                summaries += batch
                    .iter()
                    .filter(|response| matches!(response, Response::Success(_)))
                    .count();
                drop(batch);
                assert_eq!(occupied.load(Ordering::SeqCst), 0);
                request = continuation;
            }
            assert_eq!(count, 4);
            assert_eq!(summaries, 1);
            assert_eq!(session.state(), State::Ready);
        }
    }

    #[test]
    fn consumed_page_credit_belongs_only_to_live_response_batches() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let occupied = std::sync::Arc::new(AtomicUsize::new(0));
        let mut session = ready(DeferredMemory {
            remaining: 0,
            page_size: usize::MAX,
            occupied: occupied.clone(),
            memory: None,
        });
        session.handle(run("RETURN x"));
        let first = session.handle(pull(2));
        assert_eq!(occupied.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(
            occupied.load(Ordering::SeqCst),
            0,
            "session retained an empty page"
        );
        let second = session.handle(pull(1));
        let third = session.handle(pull(1));
        assert_eq!(
            occupied.load(Ordering::SeqCst),
            2,
            "live response batches lost their credits"
        );
        session.handle(Request::Reset);
        assert_eq!(occupied.load(Ordering::SeqCst), 2);
        drop(second);
        assert_eq!(occupied.load(Ordering::SeqCst), 1);
        drop(third);
        assert_eq!(occupied.load(Ordering::SeqCst), 0);
    }

    struct CommitFailureWithRows(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl QueryEngine for CommitFailureWithRows {
        fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<QueryResult, EngineError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(QueryResult {
                fields: vec!["x".into()],
                rows: vec![vec![PackValue::Int(1)]; 2],
                memory: Some(OutputMemory::new(MemoryOwner(self.0.clone()))),
            })
        }
        fn commit(&mut self) -> Result<Option<String>, EngineError> {
            Err(EngineError {
                code: "test.commit".into(),
                message: "commit failed".into(),
            })
        }
        fn abort(&mut self) -> Result<(), EngineError> {
            Ok(())
        }
    }

    #[test]
    fn record_prefix_keeps_accounting_when_commit_fails_and_reset_clears_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let occupied = std::sync::Arc::new(AtomicUsize::new(0));
        let mut session = Session::new(CommitFailureWithRows(occupied.clone()), AuthMode::AllowAll);
        session.handle(Request::Hello { extra: map(&[]) });
        session.handle(Request::Logon { auth: map(&[]) });
        session.handle(run("query"));
        let responses = session.handle(pull(-1));
        assert!(matches!(responses[0], Response::Record(_)));
        assert!(matches!(responses.last(), Some(Response::Failure { .. })));
        assert_eq!(session.state(), State::Failed);
        session.handle(Request::Reset);
        assert_eq!(
            occupied.load(Ordering::SeqCst),
            1,
            "failure prefix still owns its rows"
        );
        drop(responses);
        assert_eq!(occupied.load(Ordering::SeqCst), 0);
    }

    fn map(pairs: &[(&str, PackValue)]) -> PackValue {
        PackValue::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn run(q: &str) -> Request {
        Request::Run {
            query: q.into(),
            params: map(&[]),
            extra: map(&[]),
        }
    }

    fn pull(n: i64) -> Request {
        Request::Pull {
            extra: map(&[("n", PackValue::Int(n))]),
        }
    }

    fn ready<E: QueryEngine>(engine: E) -> Session<E> {
        let mut s = Session::new(engine, AuthMode::AllowAll);
        let hello = s.handle(Request::Hello { extra: map(&[]) });
        match &hello[0] {
            Response::Success(meta) => {
                let server = meta
                    .iter()
                    .find(|(k, _)| k == "server")
                    .map(|(_, v)| v.clone());
                assert_eq!(
                    server,
                    Some(PackValue::String(
                        "Neo4j/5.26.0 compatible graph database server - Brainwire".into()
                    ))
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(s.handle(Request::Logon { auth: map(&[]) }).len(), 1);
        assert_eq!(s.state(), State::Ready);
        s
    }

    struct ThreeRows;

    #[test]
    fn invalid_transaction_timeouts_fail_before_begin() {
        for value in [
            PackValue::Null,
            PackValue::Bool(true),
            PackValue::Float(1.0),
            PackValue::String("10".into()),
            PackValue::Int(-1),
        ] {
            for implicit in [false, true] {
                let mut session = ready(StubEngine);
                let extra = map(&[("tx_timeout", value.clone())]);
                let request = if implicit {
                    Request::Run {
                        query: "RETURN 1".into(),
                        params: map(&[]),
                        extra,
                    }
                } else {
                    Request::Begin { extra }
                };
                assert!(
                    matches!(&session.handle(request)[0], Response::Failure {code,..}
                    if code == "Neo.ClientError.Request.Invalid")
                );
            }
        }
        let mut session = ready(StubEngine);
        let extra = map(&[
            ("tx_timeout", PackValue::Int(1)),
            ("tx_timeout", PackValue::Int(2)),
        ]);
        assert!(
            matches!(&session.handle(Request::Begin {extra})[0], Response::Failure {code,..}
            if code == "Neo.ClientError.Request.Invalid")
        );
    }

    #[test]
    fn unsupported_timeout_is_not_silently_ignored_but_zero_is_unlimited() {
        let mut session = ready(StubEngine);
        assert!(
            matches!(&session.handle(Request::Begin {extra: map(&[("tx_timeout", PackValue::Int(1))])})[0],
            Response::Failure {code,..} if code == "Neo.ClientError.Statement.NotSupported")
        );
        let mut session = ready(StubEngine);
        assert!(matches!(
            &session.handle(Request::Begin {
                extra: map(&[("tx_timeout", PackValue::Int(0))])
            })[0],
            Response::Success(_)
        ));
    }

    #[test]
    fn transaction_mode_is_validated_and_legacy_read_support_fails_closed() {
        for extra in [
            map(&[("mode", PackValue::Null)]),
            map(&[("mode", PackValue::String("READ".into()))]),
            map(&[
                ("mode", PackValue::String("r".into())),
                ("mode", PackValue::String("w".into())),
            ]),
        ] {
            let mut session = ready(StubEngine);
            assert!(matches!(
                session.handle(Request::Begin { extra })[0],
                Response::Failure { .. }
            ));
        }
        let mut session = ready(ThreeRows);
        assert!(
            matches!(&session.handle(Request::Begin { extra:map(&[("mode", PackValue::String("r".into()))]) })[0], Response::Failure { code, .. } if code == "Neo.ClientError.Security.Forbidden")
        );
        let mut session = ready(StubEngine);
        assert!(matches!(
            session.handle(Request::Begin {
                extra: map(&[("mode", PackValue::String("r".into()))])
            })[0],
            Response::Success(_)
        ));
    }

    impl QueryEngine for ThreeRows {
        fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
            Ok(())
        }
        fn commit(&mut self) -> Result<Option<String>, EngineError> {
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<QueryResult, EngineError> {
            Ok(QueryResult {
                memory: None,
                fields: vec!["i".into()],
                rows: (0..3).map(|i| vec![PackValue::Int(i)]).collect(),
            })
        }
    }

    #[test]
    fn auto_commit_run_and_pull() {
        let mut s = ready(StubEngine);
        let r = s.handle(run("RETURN 1 AS x"));
        assert!(
            matches!(&r[0], Response::Success(meta) if meta.iter().any(|(k, v)| k == "fields" && *v == PackValue::List(vec![PackValue::String("x".into())])))
        );
        let r = s.handle(pull(1000));
        assert_eq!(r[0], Response::Record(vec![PackValue::Int(1)]));
        assert!(matches!(&r[1], Response::Success(_)));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn stub_transaction_does_not_fabricate_a_bookmark() {
        let mut s = ready(StubEngine);
        s.handle(Request::Begin { extra: map(&[]) });
        assert_eq!(s.state(), State::TxReady);
        s.handle(run("RETURN 2"));
        assert_eq!(s.state(), State::TxStreaming);
        s.handle(pull(-1));
        assert_eq!(s.state(), State::TxReady);
        let r = s.handle(Request::Commit);
        assert!(
            matches!(&r[0], Response::Success(meta) if !meta.iter().any(|(k, _)| k == "bookmark"))
        );
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn failure_then_ignored_until_reset() {
        let mut s = ready(StubEngine);
        let r = s.handle(run("MATCH (n) RETURN n"));
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Statement.SyntaxError")
        );
        assert_eq!(s.handle(pull(10)), vec![Response::Ignored]);
        assert!(matches!(s.handle(Request::Reset)[0], Response::Success(_)));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn pull_in_batches_reports_has_more() {
        let mut s = ready(ThreeRows);
        s.handle(run("anything"));
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 3);
        assert!(
            matches!(&r[2], Response::Success(meta) if meta.iter().any(|(k, v)| k == "has_more" && *v == PackValue::Bool(true)))
        );
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 2);
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn goodbye_answers_nothing() {
        let mut s = ready(StubEngine);
        assert!(s.handle(Request::Goodbye).is_empty());
        assert_eq!(s.state(), State::Defunct);
    }

    // The five tests above come from the brief. The table in the brief (research/bolt.md
    // section 3) has more rows than those tests exercise; the ones below close the gaps:
    // DISCARD, a failing RUN inside a transaction, a PULL that leaves rows behind inside a
    // transaction, ROLLBACK, TELEMETRY, ROUTE, and a request that is simply out of place.

    #[test]
    fn discard_drops_remaining_rows_and_returns_to_ready() {
        let mut s = ready(ThreeRows);
        s.handle(run("anything"));
        assert_eq!(s.state(), State::Streaming);
        let r = s.handle(Request::Discard { extra: map(&[]) });
        assert_eq!(r.len(), 1);
        assert!(
            matches!(&r[0], Response::Success(meta) if meta.iter().any(|(k, v)| k == "type" && *v == PackValue::String("r".into())))
        );
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn run_failure_inside_a_transaction_goes_to_failed() {
        let mut s = ready(StubEngine);
        s.handle(Request::Begin { extra: map(&[]) });
        let r = s.handle(run("MATCH (n) RETURN n"));
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Statement.SyntaxError")
        );
        assert_eq!(s.state(), State::Failed);
    }

    #[test]
    fn tx_pull_in_batches_reports_has_more_and_stays_in_the_transaction() {
        let mut s = ready(ThreeRows);
        s.handle(Request::Begin { extra: map(&[]) });
        s.handle(run("anything"));
        assert_eq!(s.state(), State::TxStreaming);
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 3);
        assert!(
            matches!(&r[2], Response::Success(meta) if meta.iter().any(|(k, v)| k == "has_more" && *v == PackValue::Bool(true)))
        );
        assert_eq!(s.state(), State::TxStreaming);
        let r = s.handle(pull(2));
        assert_eq!(r.len(), 2);
        assert_eq!(s.state(), State::TxReady);
    }

    #[test]
    fn rollback_returns_to_ready_without_a_bookmark() {
        let mut s = ready(StubEngine);
        s.handle(Request::Begin { extra: map(&[]) });
        let r = s.handle(Request::Rollback);
        assert!(matches!(&r[0], Response::Success(meta) if meta.is_empty()));
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn telemetry_is_acknowledged_in_ready_and_tx_ready_without_changing_state() {
        let mut s = ready(StubEngine);
        assert!(matches!(
            s.handle(Request::Telemetry)[0],
            Response::Success(_)
        ));
        assert_eq!(s.state(), State::Ready);
        s.handle(Request::Begin { extra: map(&[]) });
        assert!(matches!(
            s.handle(Request::Telemetry)[0],
            Response::Success(_)
        ));
        assert_eq!(s.state(), State::TxReady);
    }

    #[test]
    fn route_is_refused_with_a_failure() {
        let mut s = ready(StubEngine);
        let r = s.handle(Request::Route);
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid")
        );
        assert_eq!(s.state(), State::Failed);
    }

    #[test]
    fn out_of_place_request_fails_invalid_and_enters_failed() {
        let mut s = ready(StubEngine);
        // No RUN preceded this: PULL is out of place in the Ready state.
        let r = s.handle(pull(10));
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid")
        );
        assert_eq!(s.state(), State::Failed);
    }

    // I1 (review finding, 2026-09-25): a session that never sent HELLO/LOGON
    // must never reach READY or FAILED. Before the fix, RESET and the
    // catch-all both matched `(_, _)`-style arms ahead of the Connected/
    // Authentication-specific ones, so RESET short-circuited straight to
    // READY (an authentication bypass) and a bad message in Connected or
    // Authentication landed in FAILED, which RESET then also turns back
    // into READY. research/bolt.md 3.1 ("a message that is unexpected for
    // the state ... the server goes to DEFUNCT"), 3.5 (RESET: "FAILURE only
    // if the connection has not reached READY yet, and then the server
    // closes it") and 6.6 ("FAILURE on HELLO or LOGON: the server closes")
    // all agree the outcome pre-login is FAILURE + DEFUNCT, never READY or
    // FAILED.

    #[test]
    fn reset_right_after_the_handshake_is_refused_and_the_session_becomes_defunct() {
        let mut s = Session::new(StubEngine, AuthMode::AllowAll);
        assert_eq!(s.state(), State::Connected);
        let r = s.handle(Request::Reset);
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid"),
            "expected FAILURE, got {r:?}"
        );
        assert_eq!(s.state(), State::Defunct);
    }

    #[test]
    fn reset_after_hello_but_before_logon_is_refused_and_the_session_becomes_defunct() {
        let mut s = Session::new(StubEngine, AuthMode::AllowAll);
        s.handle(Request::Hello { extra: map(&[]) });
        assert_eq!(s.state(), State::Authentication);
        let r = s.handle(Request::Reset);
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid"),
            "expected FAILURE, got {r:?}"
        );
        assert_eq!(s.state(), State::Defunct);
    }

    #[test]
    fn run_after_hello_but_before_logon_is_refused_and_the_session_becomes_defunct_not_failed() {
        let mut s = Session::new(StubEngine, AuthMode::AllowAll);
        s.handle(Request::Hello { extra: map(&[]) });
        let r = s.handle(run("RETURN 7 AS z"));
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid"),
            "expected FAILURE, got {r:?}"
        );
        // The important distinction: DEFUNCT (closed), not FAILED (recoverable
        // with RESET) -- a session that never logged on must not linger.
        assert_eq!(s.state(), State::Defunct);
    }

    #[test]
    fn a_second_hello_from_ready_is_refused_and_the_session_becomes_defunct() {
        let mut s = ready(StubEngine);
        let r = s.handle(Request::Hello { extra: map(&[]) });
        assert!(
            matches!(&r[0], Response::Failure { code, .. } if code == "Neo.ClientError.Request.Invalid"),
            "expected FAILURE, got {r:?}"
        );
        assert_eq!(s.state(), State::Defunct);
    }
    #[test]
    fn default_access_hook_rejects_any_protected_context() {
        let mut engine = ThreeRows;
        assert!(engine.set_access(AccessContext::Unrestricted).is_ok());
        assert!(engine.set_access(AccessContext::Unauthenticated).is_err());
        assert!(engine
            .set_access(AccessContext::Authenticated(crate::auth::Principal {
                id: "reader".into(),
                graphs: Default::default(),
                instance_admin: false,
                node_visibility: None,
                visibility_delegate: false,
            }))
            .is_err());
    }
}

#[cfg(test)]
mod protected_tests {
    use super::*;
    use crate::auth::{AccessContext, AuthPolicy, GraphAccess};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    fn empty() -> PackValue {
        PackValue::Map(vec![])
    }
    fn credentials() -> PackValue {
        PackValue::Map(vec![
            ("scheme".into(), PackValue::String("basic".into())),
            ("principal".into(), PackValue::String("reader".into())),
            ("credentials".into(), PackValue::String(TOKEN.into())),
        ])
    }
    fn required() -> AuthMode {
        AuthMode::Required(Arc::new(AuthPolicy::from_json(br#"{"principals":[{"id":"reader","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"tickets":"read"},"instance_admin":false}]}"#).unwrap()))
    }
    #[derive(Default)]
    struct Trace {
        access: Vec<AccessContext>,
        events: Vec<&'static str>,
        fail_abort: bool,
    }
    struct Engine(Arc<Mutex<Trace>>);
    impl QueryEngine for Engine {
        fn set_access(&mut self, access: AccessContext) -> Result<(), EngineError> {
            let mut trace = self.0.lock().unwrap();
            trace.events.push("access");
            trace.access.push(access);
            Ok(())
        }
        fn begin(&mut self, _: Option<&str>) -> Result<(), EngineError> {
            self.0.lock().unwrap().events.push("begin");
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<QueryResult, EngineError> {
            self.0.lock().unwrap().events.push("run");
            Ok(QueryResult {
                memory: None,
                fields: vec!["x".into()],
                rows: vec![vec![PackValue::Int(1)]; 3],
            })
        }
        fn commit(&mut self) -> Result<Option<String>, EngineError> {
            self.0.lock().unwrap().events.push("commit");
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), EngineError> {
            let mut trace = self.0.lock().unwrap();
            trace.events.push("abort");
            if trace.fail_abort {
                Err(EngineError {
                    code: "test.abort".into(),
                    message: "cleanup failed".into(),
                })
            } else {
                Ok(())
            }
        }
    }
    fn session(mode: AuthMode) -> (Session<Engine>, Arc<Mutex<Trace>>) {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut session = Session::new(Engine(trace.clone()), mode);
        session.handle(Request::Hello { extra: empty() });
        (session, trace)
    }
    fn login(s: &mut Session<Engine>) {
        assert!(matches!(
            s.handle(Request::Logon {
                auth: credentials()
            })[0],
            Response::Success(_)
        ));
    }
    fn run() -> Request {
        Request::Run {
            query: "RETURN 1".into(),
            params: empty(),
            extra: empty(),
        }
    }

    #[test]
    fn protected_logon_passes_principal_and_reset_preserves_identity() {
        let (mut s, t) = session(required());
        login(&mut s);
        let trace = t.lock().unwrap();
        assert!(
            matches!(&trace.access[0],AccessContext::Authenticated(p) if p.id=="reader" && p.graphs.get("tickets")==Some(&GraphAccess::Read))
        );
        drop(trace);
        s.handle(run());
        s.handle(Request::Reset);
        assert_eq!(s.state(), State::Ready);
        assert_eq!(t.lock().unwrap().access.len(), 1);
    }
    #[test]
    fn failed_auth_is_generic_fatal_and_cannot_be_reset() {
        for auth in [
            empty(),
            PackValue::Map(vec![(
                "credentials".into(),
                PackValue::String(TOKEN.into()),
            )]),
        ] {
            let (mut s, t) = session(required());
            let result = s.handle(Request::Logon { auth });
            assert!(
                matches!(&result[0],Response::Failure{code,message} if code=="Neo.ClientError.Security.Unauthorized" && message=="authentication failed")
            );
            assert_eq!(s.state(), State::Defunct);
            assert!(s.handle(Request::Reset).is_empty());
            assert!(s.handle(run()).is_empty());
            assert!(!t.lock().unwrap().events.contains(&"begin"));
        }
    }
    #[test]
    fn logoff_discards_stream_and_transaction_before_revoking_identity() {
        for explicit in [false, true] {
            let (mut s, t) = session(required());
            login(&mut s);
            if explicit {
                s.handle(Request::Begin { extra: empty() });
            }
            s.handle(run());
            assert!(matches!(s.handle(Request::Logoff)[0], Response::Success(_)));
            assert_eq!(s.state(), State::Authentication);
            let trace = t.lock().unwrap();
            assert_eq!(&trace.events[trace.events.len() - 2..], ["abort", "access"]);
            assert_eq!(trace.access.last(), Some(&AccessContext::Unauthenticated));
            assert!(!trace.events.contains(&"commit"));
            drop(trace);
            login(&mut s);
            assert!(s.rows.is_empty());
        }
    }
    #[test]
    fn logoff_works_in_failed_state_and_allow_all_requires_fresh_login() {
        for mode in [required(), AuthMode::AllowAll] {
            let (mut s, t) = session(mode);
            login(&mut s);
            s.handle(Request::Route);
            assert_eq!(s.state(), State::Failed);
            assert!(matches!(s.handle(Request::Logoff)[0], Response::Success(_)));
            assert_eq!(s.state(), State::Authentication);
            assert_eq!(
                t.lock().unwrap().access.last(),
                Some(&AccessContext::Unauthenticated)
            );
            assert!(matches!(
                s.handle(Request::Reset)[0],
                Response::Failure { .. }
            ));
            assert_eq!(s.state(), State::Defunct);
        }
    }
    #[test]
    fn repeated_logon_closes_and_revokes_previous_identity() {
        for failed in [false, true] {
            let (mut s, t) = session(required());
            login(&mut s);
            s.handle(run());
            if failed {
                s.handle(Request::Route);
            }
            assert!(matches!(
                s.handle(Request::Logon {
                    auth: credentials()
                })[0],
                Response::Failure { .. }
            ));
            assert_eq!(s.state(), State::Defunct);
            assert_eq!(
                t.lock().unwrap().access.last(),
                Some(&AccessContext::Unauthenticated)
            );
            assert!(!t.lock().unwrap().events.contains(&"commit"));
        }
    }
    #[test]
    fn logoff_cleanup_failure_still_revokes_and_closes() {
        let (mut s, t) = session(required());
        login(&mut s);
        s.handle(run());
        t.lock().unwrap().fail_abort = true;
        assert!(matches!(
            s.handle(Request::Logoff)[0],
            Response::Failure { .. }
        ));
        assert_eq!(s.state(), State::Defunct);
        assert_eq!(
            t.lock().unwrap().access.last(),
            Some(&AccessContext::Unauthenticated)
        );
    }
    #[test]
    fn hello_credentials_do_not_authenticate_and_unsupported_engines_fail_closed() {
        let mut s = Session::new(StubEngine, required());
        s.handle(Request::Hello {
            extra: credentials(),
        });
        assert_eq!(s.state(), State::Authentication);
        assert!(matches!(s.handle(run())[0], Response::Failure { .. }));
        let mut s = Session::new(StubEngine, required());
        s.handle(Request::Hello { extra: empty() });
        assert!(
            matches!(&s.handle(Request::Logon{auth:credentials()})[0],Response::Failure{code,..} if code=="Neo.ClientError.Security.Forbidden")
        );
        assert_eq!(s.state(), State::Defunct);
    }
    #[test]
    fn impersonation_is_rejected_before_begin_or_run() {
        for explicit in [false, true] {
            let (mut s, t) = session(required());
            login(&mut s);
            let extra =
                PackValue::Map(vec![("imp_user".into(), PackValue::String("admin".into()))]);
            let request = if explicit {
                Request::Begin { extra }
            } else {
                Request::Run {
                    query: "RETURN 1".into(),
                    params: empty(),
                    extra,
                }
            };
            assert!(matches!(s.handle(request)[0], Response::Failure { .. }));
            assert!(!t.lock().unwrap().events.contains(&"begin"));
        }
    }
    #[test]
    fn logoff_ready_and_tx_ready_revoke_without_committing() {
        for explicit in [false, true] {
            let (mut s, t) = session(required());
            login(&mut s);
            if explicit {
                s.handle(Request::Begin { extra: empty() });
            }
            assert!(matches!(s.handle(Request::Logoff)[0], Response::Success(_)));
            assert_eq!(s.state(), State::Authentication);
            let trace = t.lock().unwrap();
            assert_eq!(trace.access.last(), Some(&AccessContext::Unauthenticated));
            assert!(!trace.events.contains(&"commit"));
        }
    }

    #[test]
    fn explicit_run_impersonation_aborts_bound_transaction() {
        let (mut s, t) = session(required());
        login(&mut s);
        s.handle(Request::Begin { extra: empty() });
        assert!(matches!(
            s.handle(Request::Run {
                query: "RETURN 1".into(),
                params: empty(),
                extra: PackValue::Map(vec![("imp_user".into(), PackValue::Null)])
            })[0],
            Response::Failure { .. }
        ));
        assert_eq!(s.state(), State::Failed);
        let trace = t.lock().unwrap();
        assert!(trace.events.contains(&"abort"));
        assert!(!trace.events.contains(&"run"));
    }
}

#[cfg(test)]
mod delegated_visibility_tests {
    use super::*;

    fn extra(value: PackValue) -> PackValue {
        PackValue::Map(vec![(
            "tx_metadata".into(),
            PackValue::Map(vec![("brainwire_visibility".into(), value)]),
        )])
    }

    #[test]
    fn a_delegated_visibility_is_parsed_and_validated() {
        assert!(delegated_visibility(&PackValue::Map(vec![]))
            .unwrap()
            .is_none());
        let visibility = delegated_visibility(&extra(PackValue::Map(vec![
            (
                "principals".into(),
                PackValue::List(vec![PackValue::String("user:a".into())]),
            ),
            ("allowed_connectors".into(), PackValue::List(vec![])),
            ("entry_strict".into(), PackValue::Bool(true)),
        ])))
        .unwrap()
        .unwrap();
        assert_eq!(visibility.principals, vec!["user:a".to_string()]);
        assert_eq!(visibility.allowed_connectors, Some(vec![]));
        assert!(visibility.entry_strict);
        for bad in [
            PackValue::String("all".into()),
            PackValue::Map(vec![("unknown".into(), PackValue::Bool(true))]),
            PackValue::Map(vec![(
                "principals".into(),
                PackValue::List(vec![PackValue::String(String::new())]),
            )]),
        ] {
            assert!(delegated_visibility(&extra(bad)).is_err());
        }
    }
}

#[cfg(test)]
mod request_identity_tests {
    use super::*;
    fn extra(value: PackValue) -> PackValue {
        PackValue::Map(vec![(
            "tx_metadata".into(),
            PackValue::Map(vec![("brainwire_request_id".into(), value)]),
        )])
    }
    #[test]
    fn durable_identity_validates_nested_metadata_and_never_silently_falls_back() {
        assert_eq!(
            durable_request_id(&extra(PackValue::String("batch-42".into()))).unwrap(),
            Some("batch-42")
        );
        for id in ["", "with space", "é", &"x".repeat(129)] {
            assert!(durable_request_id(&extra(PackValue::String(id.into()))).is_err());
        }
        assert!(durable_request_id(&extra(PackValue::Int(42))).is_err());
        assert!(durable_request_id(&PackValue::Map(vec![(
            "tx_metadata".into(),
            PackValue::Null
        )]))
        .is_err());
        let duplicate = PackValue::Map(vec![(
            "tx_metadata".into(),
            PackValue::Map(vec![
                (
                    "brainwire_request_id".into(),
                    PackValue::String("one".into()),
                ),
                (
                    "brainwire_request_id".into(),
                    PackValue::String("two".into()),
                ),
            ]),
        )]);
        assert!(durable_request_id(&duplicate).is_err());
        let mut engine = StubEngine;
        assert!(engine
            .begin_with_request_id(None, false, Some("batch-42"))
            .is_err());
        assert!(engine.begin_with_request_id(None, true, None).is_ok());
    }
}
