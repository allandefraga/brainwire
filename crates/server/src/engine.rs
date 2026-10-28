//! Cypher/Bolt value conversion and the durable per-connection engine.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use brainwire_bolt::auth::{AccessContext, GraphAccess};
use brainwire_bolt::packstream::PackValue;
use brainwire_bolt::session::{EngineError, OutputMemory, QueryEngine, QueryResult};
use brainwire_core::acl::ReadVisibility;
use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, Out, QueryAccess, QueryOutput, QueryTransaction};

use crate::cluster::Replica;
use crate::databases::Databases;

enum Command {
    SetAccess(AccessContext, mpsc::Sender<Result<(), EngineError>>),
    Begin(
        Option<String>,
        bool,
        Option<String>,
        Option<ReadVisibility>,
        Option<(Instant, Duration)>,
        mpsc::Sender<Result<(), EngineError>>,
    ),
    Run(
        Option<String>,
        String,
        PackValue,
        mpsc::Sender<Result<QueryResult, EngineError>>,
    ),
    Check(mpsc::Sender<Result<(), EngineError>>),
    Commit(mpsc::Sender<Result<Option<String>, EngineError>>),
    Abort(mpsc::Sender<Result<(), EngineError>>),
}

/// One worker owns each connection's scoped storage transaction. The worker's
/// local database handle outlives its transaction, without unsafe lifetimes.
pub struct DbEngine {
    sender: mpsc::SyncSender<Command>,
}

impl DbEngine {
    pub fn new(databases: Arc<Databases>, default_database: String) -> Result<Self, String> {
        Self::with_access(
            databases,
            default_database,
            AccessContext::Unrestricted,
            None,
        )
    }

    pub fn new_protected(
        databases: Arc<Databases>,
        default_database: String,
    ) -> Result<Self, String> {
        Self::with_access(
            databases,
            default_database,
            AccessContext::Unauthenticated,
            None,
        )
    }

    pub(crate) fn new_replicated(
        databases: Arc<Databases>,
        default_database: String,
        replica: Arc<Replica>,
    ) -> Result<Self, String> {
        Self::with_access(
            databases,
            default_database,
            AccessContext::Unauthenticated,
            Some(replica),
        )
    }

    fn with_access(
        databases: Arc<Databases>,
        default_database: String,
        access: AccessContext,
        replica: Option<Arc<Replica>>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("brainwire-query".into())
            .spawn(move || worker(receiver, databases, default_database, access, replica))
            .map_err(|e| format!("cannot start query worker: {e}"))?;
        Ok(Self { sender })
    }

    fn ask<T>(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<T, EngineError>>) -> Command,
    ) -> Result<T, EngineError> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(command(reply))
            .map_err(|_| worker_error())?;
        receiver.recv().map_err(|_| worker_error())?
    }
}

impl QueryEngine for DbEngine {
    fn set_access(&mut self, access: AccessContext) -> Result<(), EngineError> {
        self.ask(|reply| Command::SetAccess(access, reply))
    }

    fn begin(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        self.ask(|reply| Command::Begin(db.map(str::to_owned), false, None, None, None, reply))
    }

    fn begin_read_only(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        self.ask(|reply| Command::Begin(db.map(str::to_owned), true, None, None, None, reply))
    }

    fn begin_delegated(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
        visibility: ReadVisibility,
    ) -> Result<(), EngineError> {
        self.ask(|reply| {
            Command::Begin(
                db.map(str::to_owned),
                read_only,
                request_id.map(str::to_owned),
                Some(visibility),
                None,
                reply,
            )
        })
    }

    fn begin_with_request_id(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
    ) -> Result<(), EngineError> {
        self.ask(|reply| {
            Command::Begin(
                db.map(str::to_owned),
                read_only,
                request_id.map(str::to_owned),
                None,
                None,
                reply,
            )
        })
    }

    fn begin_with_timeout(
        &mut self,
        db: Option<&str>,
        read_only: bool,
        request_id: Option<&str>,
        visibility: Option<ReadVisibility>,
        timeout: Option<Duration>,
    ) -> Result<(), EngineError> {
        let timeout = timeout
            .filter(|timeout| !timeout.is_zero())
            .map(|timeout| (Instant::now(), timeout));
        self.ask(|reply| {
            Command::Begin(
                db.map(str::to_owned),
                read_only,
                request_id.map(str::to_owned),
                visibility,
                timeout,
                reply,
            )
        })
    }

    fn check_transaction(&mut self) -> Result<(), EngineError> {
        self.ask(Command::Check)
    }

    fn run(
        &mut self,
        db: Option<&str>,
        query: &str,
        params: &PackValue,
    ) -> Result<QueryResult, EngineError> {
        self.ask(|reply| {
            Command::Run(
                db.map(str::to_owned),
                query.to_owned(),
                params.clone(),
                reply,
            )
        })
    }

    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        self.ask(Command::Commit)
    }

    fn abort(&mut self) -> Result<(), EngineError> {
        self.ask(Command::Abort)
    }
}

fn forbidden() -> EngineError {
    error("Neo.ClientError.Security.Forbidden", "access denied")
}

fn graph_access(access: &AccessContext, graph: &str) -> Result<QueryAccess, EngineError> {
    match access {
        AccessContext::Unrestricted => Ok(QueryAccess::UNRESTRICTED),
        AccessContext::Unauthenticated => Err(forbidden()),
        AccessContext::Authenticated(principal) => {
            let grant = principal.graphs.get(graph).ok_or_else(forbidden)?;
            Ok(QueryAccess {
                write: matches!(grant, GraphAccess::Write),
                instance_admin: principal.instance_admin,
            })
        }
    }
}

/// The visibility a transaction reads under: the principal's static policy, or,
/// for a visibility delegate that sends one, its caller's (the transaction is
/// then read-only). A delegate has no static policy, so every transaction must
/// carry caller visibility; omitting it is denied. Only a delegate (or an
/// unprotected development server) may send one.
fn transaction_visibility(
    access: &AccessContext,
    delegated: Option<ReadVisibility>,
) -> Result<Option<ReadVisibility>, EngineError> {
    match access {
        AccessContext::Authenticated(principal) if principal.visibility_delegate => {
            delegated.map(Some).ok_or_else(forbidden)
        }
        AccessContext::Authenticated(principal) => match delegated {
            Some(_) => Err(error(
                "Neo.ClientError.Security.Forbidden",
                "this principal may not delegate visibility",
            )),
            None => Ok(principal.node_visibility.clone()),
        },
        AccessContext::Unrestricted => Ok(delegated),
        AccessContext::Unauthenticated => Err(forbidden()),
    }
}

fn worker_error() -> EngineError {
    error(
        "Neo.DatabaseError.General.UnknownError",
        "query worker stopped",
    )
}

fn worker(
    receiver: mpsc::Receiver<Command>,
    databases: Arc<Databases>,
    default_database: String,
    mut access: AccessContext,
    replica: Option<Arc<Replica>>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::SetAccess(next, reply) => {
                access = next;
                let _ = reply.send(Ok(()));
            }
            Command::Begin(name, read_only, request_id, delegated, timeout, reply) => {
                let name = name.as_deref().unwrap_or(&default_database);
                let visibility = match transaction_visibility(&access, delegated) {
                    Ok(visibility) => visibility,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                let mut query_access = match graph_access(&access, name) {
                    Ok(access) => access,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                if read_only {
                    query_access.write = false;
                }
                if request_id.is_some() && replica.is_none() {
                    let _ = reply.send(Err(error(
                        "Neo.ClientError.Request.Invalid",
                        "explicit request IDs require replicated serving",
                    )));
                    continue;
                }
                let seed = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let caller = match &access {
                    AccessContext::Authenticated(principal) => principal.id.as_str(),
                    _ => "standalone",
                };
                let request_id = replica
                    .as_ref()
                    .map(|replica| replica.request_key(name, caller, &seed));
                let restricted = visibility.is_some();
                let _writer_permit = if let Some(replica) = &replica {
                    match replica.begin(
                        query_access.write && !restricted,
                        request_id.as_deref().expect("replicated request"),
                    ) {
                        Ok(permit) => permit,
                        Err(message) => {
                            let code = if message.contains("already committed") {
                                "Neo.ClientError.Transaction.RequestAlreadyCommitted"
                            } else if message.contains("previously rejected") {
                                "Neo.ClientError.Transaction.RequestPreviouslyRejected"
                            } else {
                                "Neo.TransientError.General.DatabaseUnavailable"
                            };
                            let _ = reply.send(Err(error(code, message)));
                            continue;
                        }
                    }
                } else {
                    None
                };
                let Some(database) = databases.get(name) else {
                    let _ = reply.send(Err(error(
                        "Neo.ClientError.Database.DatabaseNotFound",
                        format!("database {name:?} not found"),
                    )));
                    continue;
                };
                let deadline = match timeout {
                    Some((started, client_timeout)) => {
                        let timeout = databases
                            .query_limits()
                            .timeout
                            .map_or(client_timeout, |server_timeout| {
                                server_timeout.min(client_timeout)
                            });
                        match started.checked_add(timeout) {
                            Some(deadline) => Some(deadline),
                            None => {
                                let _ = reply.send(Err(error(
                                    "Neo.ClientError.Request.Invalid",
                                    "transaction timeout exceeds supported duration",
                                )));
                                continue;
                            }
                        }
                    }
                    None => None,
                };
                let mut transaction = match QueryTransaction::begin_with_pool(
                    &database,
                    databases.query_limits(),
                    databases.query_pool(),
                ) {
                    Ok(transaction) => transaction,
                    Err(err) => {
                        let _ = reply.send(Err(execution_error(err)));
                        continue;
                    }
                };
                if let Some(deadline) = deadline {
                    transaction = transaction.with_deadline(deadline);
                    if let Err(err) = transaction.check_transaction_deadline() {
                        let _ = reply.send(Err(execution_error(err)));
                        continue;
                    }
                }
                if let Some(root) = databases.checkpoint_dir() {
                    transaction = transaction.with_checkpoint_dir(root.to_path_buf());
                }
                if replica.is_some() {
                    transaction = transaction.with_replicated_ids();
                }
                if let Some(visibility) = visibility {
                    transaction = match transaction.with_read_visibility(visibility) {
                        Ok(transaction) => transaction,
                        Err(err) => {
                            let _ = reply.send(Err(execution_error(err)));
                            continue;
                        }
                    };
                }
                let _ = reply.send(Ok(()));
                loop {
                    let command = match deadline {
                        Some(deadline) => match receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        {
                            Ok(command) => Some(command),
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                let retained_memory = transaction.rollback_retaining_memory();
                                drop(transaction);
                                drop(_writer_permit);
                                expired_transaction(&receiver, &mut access, None, retained_memory);
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => None,
                        },
                        None => receiver.recv().ok(),
                    };
                    let Some(command) = command else { break };
                    // A command queued before recv_timeout returned may already be late.
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        let retained_memory = transaction.rollback_retaining_memory();
                        drop(transaction);
                        drop(_writer_permit);
                        expired_transaction(&receiver, &mut access, Some(command), retained_memory);
                        break;
                    }
                    match command {
                        Command::SetAccess(next, reply) => {
                            transaction.rollback();
                            access = next;
                            let _ = reply.send(Ok(()));
                            break;
                        }
                        Command::Run(requested, query, params, reply) => {
                            if requested
                                .as_deref()
                                .is_some_and(|requested| requested != name)
                            {
                                transaction.rollback();
                                let _ = reply.send(Err(forbidden()));
                                continue;
                            }
                            let result = params_from_pack(&params)
                                .and_then(|params| {
                                    transaction
                                        .run_with_access(&query, &params, query_access)
                                        .map_err(execution_error)
                                })
                                .and_then(output_to_bolt)
                                .and_then(|output| {
                                    transaction
                                        .check_transaction_deadline()
                                        .map(|()| {
                                            let mut output = output;
                                            output.memory = Some(OutputMemory::new(
                                                transaction.output_memory_lease(),
                                            ));
                                            output
                                        })
                                        .map_err(execution_error)
                                });
                            // Conversion errors happen outside QueryTransaction::run;
                            // they must also discard earlier writes and pool reservations.
                            if result.is_err() {
                                transaction.rollback();
                            }
                            let _ = reply.send(result);
                        }
                        Command::Check(reply) => {
                            // The pre-dispatch check is this operation's deadline
                            // linearization point. A second mutating check here
                            // could release accounting while Session owns rows.
                            let result = if transaction.state()
                                == brainwire_cypher::QueryTransactionState::Active
                            {
                                Ok(())
                            } else {
                                Err(error(
                                    "Neo.ClientError.Transaction.TransactionNotFound",
                                    "no active transaction",
                                ))
                            };
                            let _ = reply.send(result);
                        }
                        Command::Commit(reply) => {
                            let result = if let Some(replica) = &replica {
                                match transaction.prepare_replication() {
                                    Ok(Some(_))
                                        if deadline
                                            .is_some_and(|deadline| Instant::now() >= deadline) =>
                                    {
                                        Err(timeout_error())
                                    }
                                    Ok(Some(prepared)) => replica
                                        .commit(
                                            request_id.clone().expect("replicated request"),
                                            name.to_owned(),
                                            prepared.expected_revision,
                                            prepared.batch,
                                        )
                                        .map(Some)
                                        .map_err(|message| {
                                            let code = if message
                                                == "replicated transaction revision conflict"
                                            {
                                                "Neo.TransientError.Transaction.Conflict"
                                            } else if message.contains("exceeds")
                                                || message.contains("validation limits")
                                            {
                                                "Neo.ClientError.Statement.ArgumentError"
                                            } else {
                                                "Neo.ClientError.Transaction.CommitOutcomeUnknown"
                                            };
                                            error(code, message)
                                        }),
                                    Ok(None) => Ok(None),
                                    Err(err) => Err(execution_error(err)),
                                }
                            } else {
                                transaction.commit().map(|_| None).map_err(execution_error)
                            };
                            let _ = reply.send(result);
                            break;
                        }
                        Command::Abort(reply) => {
                            transaction.rollback();
                            let _ = reply.send(Ok(()));
                            break;
                        }
                        Command::Begin(_, _, _, _, _, reply) => {
                            let _ = reply.send(Err(error(
                                "Neo.ClientError.Transaction.TransactionStartFailed",
                                "transaction already active",
                            )));
                        }
                    }
                }
            }
            Command::Abort(reply) => {
                let _ = reply.send(Ok(()));
            }
            Command::Check(reply) => {
                let _ = reply.send(Err(error(
                    "Neo.ClientError.Transaction.TransactionNotFound",
                    "no active transaction",
                )));
            }
            Command::Run(_, _, _, reply) => {
                let _ = reply.send(Err(error(
                    "Neo.ClientError.Transaction.TransactionNotFound",
                    "no active transaction",
                )));
            }
            Command::Commit(reply) => {
                let _ = reply.send(Err(error(
                    "Neo.ClientError.Transaction.TransactionNotFound",
                    "no active transaction",
                )));
            }
        }
    }
}

fn timeout_error() -> EngineError {
    error(
        "Neo.ClientError.Statement.ExecutionTimedOut",
        "transaction deadline exceeded",
    )
}

/// Wait for session cleanup after storage resources and the replicated writer
/// permit have been dropped. Retain result accounting until buffered rows clear.
fn expired_transaction(
    receiver: &mpsc::Receiver<Command>,
    access: &mut AccessContext,
    mut first: Option<Command>,
    retained_memory: brainwire_cypher::exec::QueryMemoryLease,
) {
    loop {
        let command = match first.take().or_else(|| receiver.recv().ok()) {
            Some(command) => command,
            None => return,
        };
        match command {
            Command::Abort(reply) => {
                drop(retained_memory);
                let _ = reply.send(Ok(()));
                return;
            }
            Command::SetAccess(next, reply) => {
                drop(retained_memory);
                *access = next;
                let _ = reply.send(Ok(()));
                return;
            }
            Command::Begin(_, _, _, _, _, reply) | Command::Check(reply) => {
                let _ = reply.send(Err(timeout_error()));
            }
            Command::Run(_, _, _, reply) => {
                let _ = reply.send(Err(timeout_error()));
            }
            Command::Commit(reply) => {
                drop(retained_memory);
                let _ = reply.send(Err(timeout_error()));
                return;
            }
        }
    }
}

fn error(code: &str, message: impl Into<String>) -> EngineError {
    EngineError {
        code: code.into(),
        message: message.into(),
    }
}

fn type_error(message: impl Into<String>) -> EngineError {
    error("Neo.ClientError.Statement.TypeError", message)
}

fn params_from_pack(value: &PackValue) -> Result<BTreeMap<String, Value>, EngineError> {
    let PackValue::Map(entries) = value else {
        return Err(type_error("parameters must be a map"));
    };
    entries
        .iter()
        .map(|(key, value)| value_from_pack(value, 0).map(|value| (key.clone(), value)))
        .collect()
}

fn value_from_pack(value: &PackValue, depth: usize) -> Result<Value, EngineError> {
    if depth > 64 {
        return Err(type_error("parameter nesting exceeds 64 levels"));
    }
    Ok(match value {
        PackValue::Null => Value::Null,
        PackValue::Bool(v) => Value::Bool(*v),
        PackValue::Int(v) => Value::Int(*v),
        PackValue::Float(v) if v.is_finite() => Value::Float(*v),
        PackValue::Float(_) => return Err(type_error("nonfinite parameter float")),
        PackValue::String(v) => Value::Str(v.clone()),
        PackValue::List(items) => Value::List(
            items
                .iter()
                .map(|item| value_from_pack(item, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        PackValue::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|(key, item)| {
                    value_from_pack(item, depth + 1).map(|value| (key.clone(), value))
                })
                .collect::<Result<_, _>>()?,
        ),
        PackValue::Bytes(_) | PackValue::Struct { .. } => {
            return Err(type_error("unsupported parameter value type"));
        }
    })
}

fn value_to_pack(value: Value, depth: usize) -> Result<PackValue, EngineError> {
    if depth > 64 {
        return Err(type_error("result nesting exceeds 64 levels"));
    }
    Ok(match value {
        Value::Null => PackValue::Null,
        Value::Bool(v) => PackValue::Bool(v),
        Value::Int(v) => PackValue::Int(v),
        Value::Float(v) if v.is_finite() => PackValue::Float(v),
        Value::Float(_) => return Err(type_error("nonfinite stored float")),
        Value::Str(v) => PackValue::String(v),
        Value::List(items) => PackValue::List(
            items
                .into_iter()
                .map(|item| value_to_pack(item, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(entries) => PackValue::Map(
            entries
                .into_iter()
                .map(|(key, item)| value_to_pack(item, depth + 1).map(|item| (key, item)))
                .collect::<Result<_, _>>()?,
        ),
    })
}

fn props_to_pack(
    props: std::collections::BTreeMap<String, Value>,
) -> Result<PackValue, EngineError> {
    Ok(PackValue::Map(
        props
            .into_iter()
            .map(|(key, value)| value_to_pack(value, 0).map(|value| (key, value)))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn bolt_id(raw: u64) -> Result<i64, EngineError> {
    i64::try_from(raw).map_err(|_| type_error("node id exceeds Bolt integer range"))
}

fn node_to_pack(node: brainwire_cypher::NodeOut) -> Result<PackValue, EngineError> {
    Ok(PackValue::Struct {
        tag: 0x4e,
        fields: vec![
            PackValue::Int(bolt_id(node.id.0)?),
            PackValue::List(vec![PackValue::String(node.label)]),
            props_to_pack(node.props)?,
            PackValue::String(node.id.element_id()),
        ],
    })
}

fn out_to_pack(output: Out) -> Result<PackValue, EngineError> {
    match output {
        Out::Value(value) => value_to_pack(value, 0),
        Out::Node(node) => node_to_pack(node),
        Out::Relationship(rel) => Ok(PackValue::Struct {
            tag: 0x52,
            fields: vec![
                PackValue::Int(rel.id),
                PackValue::Int(bolt_id(rel.start.0)?),
                PackValue::Int(bolt_id(rel.end.0)?),
                PackValue::String(rel.rel_type),
                props_to_pack(rel.props)?,
                PackValue::String(rel.element_id),
                PackValue::String(rel.start.element_id()),
                PackValue::String(rel.end.element_id()),
            ],
        }),
        Out::Path(path) => {
            // Bolt paths list nodes, unbound relationships, then alternating
            // (signed 1-based relationship index, node index) per step.
            let mut indices = vec![];
            for (step, rel) in path.rels.iter().enumerate() {
                let forward = rel.start == path.nodes[step].id;
                let index = step as i64 + 1;
                indices.push(PackValue::Int(if forward { index } else { -index }));
                indices.push(PackValue::Int(index));
            }
            let nodes = path
                .nodes
                .into_iter()
                .map(node_to_pack)
                .collect::<Result<Vec<_>, _>>()?;
            let rels = path
                .rels
                .into_iter()
                .map(|rel| {
                    Ok(PackValue::Struct {
                        tag: 0x72,
                        fields: vec![
                            PackValue::Int(rel.id),
                            PackValue::String(rel.rel_type),
                            props_to_pack(rel.props)?,
                            PackValue::String(rel.element_id),
                        ],
                    })
                })
                .collect::<Result<Vec<_>, EngineError>>()?;
            Ok(PackValue::Struct {
                tag: 0x50,
                fields: vec![
                    PackValue::List(nodes),
                    PackValue::List(rels),
                    PackValue::List(indices),
                ],
            })
        }
        Out::List(items) => Ok(PackValue::List(
            items
                .into_iter()
                .map(out_to_pack)
                .collect::<Result<_, _>>()?,
        )),
        Out::Map(map) => Ok(PackValue::Map(
            map.into_iter()
                .map(|(key, item)| out_to_pack(item).map(|item| (key, item)))
                .collect::<Result<_, _>>()?,
        )),
    }
}

fn output_to_bolt(output: QueryOutput) -> Result<QueryResult, EngineError> {
    let rows = output
        .rows
        .into_iter()
        .map(|row| row.into_iter().map(out_to_pack).collect::<Result<_, _>>())
        .collect::<Result<_, _>>()?;
    Ok(QueryResult {
        memory: None,
        fields: output.fields,
        rows,
    })
}

fn error_code(error: &ExecError) -> &'static str {
    match error {
        ExecError::Syntax(_) | ExecError::Unsupported(_) => "Neo.ClientError.Statement.SyntaxError",
        ExecError::Forbidden(_) => "Neo.ClientError.Security.Forbidden",
        ExecError::Semantic(_) => "Neo.ClientError.Statement.SemanticError",
        ExecError::ParameterMissing(_) => "Neo.ClientError.Statement.ParameterMissing",
        ExecError::ConstraintViolation(_) => "Neo.ClientError.Schema.ConstraintValidationFailed",
        ExecError::MemoryLimit { .. } => "Neo.ClientError.Statement.MemoryLimitExceeded",
        ExecError::MemoryPoolExhausted { .. } => {
            "Neo.TransientError.General.QueryMemoryPoolExhausted"
        }
        ExecError::TimedOut => "Neo.ClientError.Statement.ExecutionTimedOut",
        ExecError::Conflict => "Neo.TransientError.Transaction.Outdated",
        ExecError::Graph(_) => "Neo.DatabaseError.General.UnknownError",
    }
}

fn execution_error(error: ExecError) -> EngineError {
    EngineError {
        code: error_code(&error).into(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brainwire_bolt::message::{Request, Response};
    use brainwire_bolt::session::{AuthMode, Session};
    use brainwire_core::ids::{NodeId, TypeId};
    use brainwire_core::schema::GraphMapping;
    use brainwire_cypher::NodeOut;

    #[test]
    fn parameters_keep_nested_types_and_last_duplicate_key() {
        let input = PackValue::Map(vec![
            ("x".into(), PackValue::Int(1)),
            ("x".into(), PackValue::Int(2)),
            (
                "doc".into(),
                PackValue::List(vec![
                    PackValue::Null,
                    PackValue::Bool(true),
                    PackValue::Float(1.25),
                    PackValue::String("text".into()),
                    PackValue::Map(vec![("k".into(), PackValue::Int(-3))]),
                ]),
            ),
        ]);
        let values = params_from_pack(&input).unwrap();
        assert_eq!(values["x"], Value::Int(2));
        assert_eq!(
            values["doc"],
            Value::List(vec![
                Value::Null,
                Value::Bool(true),
                Value::Float(1.25),
                Value::Str("text".into()),
                Value::Map(BTreeMap::from([("k".into(), Value::Int(-3))])),
            ])
        );
        assert!(params_from_pack(&PackValue::List(vec![])).is_err());
        assert!(params_from_pack(&PackValue::Map(vec![(
            "x".into(),
            PackValue::Bytes(vec![1])
        )]))
        .is_err());
        assert!(params_from_pack(&PackValue::Map(vec![(
            "x".into(),
            PackValue::Float(f64::NAN)
        )]))
        .is_err());
    }

    #[test]
    fn node_output_uses_bolt_node_tag_and_element_id() {
        let id = NodeId::new(0, TypeId(1), 42).unwrap();
        let out = Out::Node(NodeOut {
            id,
            label: "Page".into(),
            props: BTreeMap::from([
                ("entity_id".into(), Value::Str("synthetic-42".into())),
                ("score".into(), Value::Float(0.5)),
            ]),
        });
        assert_eq!(
            out_to_pack(out).unwrap(),
            PackValue::Struct {
                tag: 0x4e,
                fields: vec![
                    PackValue::Int(id.0 as i64),
                    PackValue::List(vec![PackValue::String("Page".into())]),
                    PackValue::Map(vec![
                        ("entity_id".into(), PackValue::String("synthetic-42".into())),
                        ("score".into(), PackValue::Float(0.5)),
                    ]),
                    PackValue::String(id.element_id()),
                ],
            }
        );
    }

    #[test]
    fn protocol_error_codes_preserve_retry_and_user_error_categories() {
        assert_eq!(
            error_code(&ExecError::Conflict),
            "Neo.TransientError.Transaction.Outdated"
        );
        assert_eq!(
            error_code(&ExecError::ConstraintViolation("duplicate".into())),
            "Neo.ClientError.Schema.ConstraintValidationFailed"
        );
        assert_eq!(
            error_code(&ExecError::ParameterMissing("id".into())),
            "Neo.ClientError.Statement.ParameterMissing"
        );
        assert_eq!(
            error_code(&ExecError::Semantic("missing".into())),
            "Neo.ClientError.Statement.SemanticError"
        );
        assert_eq!(
            error_code(&ExecError::Unsupported("x".into())),
            "Neo.ClientError.Statement.SyntaxError"
        );
    }

    #[test]
    fn reserved_property_write_is_a_client_error_and_storage_errors_stay_database_errors() {
        let (_directory, _databases, mut engine) = scoped_engine_fixture();
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine.begin(None).unwrap();
        let failure = engine
            .run(
                None,
                "CREATE (n:Customer {entity_id:'x', acl_allow:['a']})",
                &PackValue::Map(vec![]),
            )
            .err()
            .expect("reserved property must fail");
        assert_eq!(failure.code, "Neo.ClientError.Statement.SemanticError");
        let corrupt = ExecError::from(brainwire_store::db::StoreError::Corrupt("x".into()));
        assert_eq!(
            error_code(&corrupt),
            "Neo.DatabaseError.General.UnknownError"
        );
    }

    #[test]
    fn parameter_conversion_failure_discards_prior_writes_and_reservations() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("default").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut engine = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
        engine.begin(None).unwrap();
        engine
            .run(
                None,
                "CREATE (c:Customer {entity_id:'pending-conversion'})",
                &PackValue::Map(vec![]),
            )
            .unwrap();
        assert!(dbs.query_pool().usage().reserved_bytes > 0);
        assert!(engine
            .run(
                None,
                "RETURN $bad",
                &PackValue::Map(vec![("bad".into(), PackValue::Bytes(vec![1, 2]))])
            )
            .is_err());
        assert!(engine.commit().is_err());
        assert_eq!(dbs.query_pool().usage().reserved_bytes, 0);
        assert_eq!(db.stats().unwrap().nodes, 0);
    }

    #[test]
    fn output_reservation_survives_autocommit_until_response_batch_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        config.memory.query = crate::size::ByteSize(24_000);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        let engine = DbEngine::new(Arc::clone(&databases), "default".into()).unwrap();
        let mut session = Session::new(engine, AuthMode::AllowAll);
        session.handle(Request::Hello {
            extra: PackValue::Map(vec![]),
        });
        session.handle(Request::Logon {
            auth: PackValue::Map(vec![]),
        });
        let params = PackValue::Map(vec![("text".into(), PackValue::String("x".repeat(3000)))]);
        session.handle(Request::Run {
            query: "RETURN $text".into(),
            params: params.clone(),
            extra: PackValue::Map(vec![]),
        });
        let held = databases.query_pool().usage().reserved_bytes;
        assert!(held > 0);
        let pending = session.handle(Request::Pull {
            extra: PackValue::Map(vec![("n".into(), PackValue::Int(-1))]),
        });
        assert!(matches!(pending[0], Response::Record(_)));
        assert_eq!(
            databases.query_pool().usage().reserved_bytes,
            held,
            "in-flight output must remain charged after autocommit"
        );
        let mut other = DbEngine::new(Arc::clone(&databases), "default".into()).unwrap();
        other.begin(None).unwrap();
        let error = other
            .run(None, "RETURN $text", &params)
            .err()
            .expect("pending output must constrain other sessions");
        assert_eq!(
            error.code,
            "Neo.TransientError.General.QueryMemoryPoolExhausted"
        );
        other.abort().unwrap();
        drop(pending);
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        other.begin(None).unwrap();
        other.run(None, "RETURN $text", &params).unwrap();
        other.commit().unwrap();
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
    }

    #[test]
    fn explicit_and_partial_output_keep_shared_reservations_until_cleanup() {
        for explicit in [false, true] {
            for cleanup in ["reset", "logoff", "drop"] {
                let dir = tempfile::tempdir().unwrap();
                let mut config = crate::config::Config::default();
                config.server.data_dir = dir.path().join("data");
                config.memory.cache = crate::size::ByteSize(64 << 20);
                config.memory.memtables = crate::size::ByteSize(16 << 20);
                let databases = Arc::new(Databases::open_all(&config).unwrap());
                let engine = DbEngine::new(Arc::clone(&databases), "default".into()).unwrap();
                let mut session = Session::new(engine, AuthMode::AllowAll);
                session.handle(Request::Hello {
                    extra: PackValue::Map(vec![]),
                });
                session.handle(Request::Logon {
                    auth: PackValue::Map(vec![]),
                });
                if explicit {
                    session.handle(Request::Begin {
                        extra: PackValue::Map(vec![]),
                    });
                }
                session.handle(Request::Run {
                    query: "UNWIND [1,2] AS x RETURN x".into(),
                    params: PackValue::Map(vec![]),
                    extra: PackValue::Map(vec![]),
                });
                let first = session.handle(Request::Pull {
                    extra: PackValue::Map(vec![("n".into(), PackValue::Int(1))]),
                });
                let held = databases.query_pool().usage().reserved_bytes;
                assert!(held > 0);
                let second = session.handle(Request::Pull {
                    extra: PackValue::Map(vec![("n".into(), PackValue::Int(-1))]),
                });
                if explicit {
                    session.handle(Request::Commit);
                }
                assert_eq!(databases.query_pool().usage().reserved_bytes, held);
                match cleanup {
                    "reset" => {
                        session.handle(Request::Reset);
                    }
                    "logoff" => {
                        session.handle(Request::Logoff);
                    }
                    "drop" => {
                        drop(session);
                    }
                    _ => unreachable!(),
                }
                assert_eq!(databases.query_pool().usage().reserved_bytes, held);
                drop(first);
                assert_eq!(databases.query_pool().usage().reserved_bytes, held);
                drop(second);
                assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
            }
        }
    }

    #[test]
    fn dropped_worker_reply_does_not_leak_output_accounting() {
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine.begin(None).unwrap();
        let (reply, receiver) = mpsc::channel();
        drop(receiver);
        engine
            .sender
            .send(Command::Run(
                None,
                "RETURN $text".into(),
                PackValue::Map(vec![("text".into(), PackValue::String("x".repeat(3000)))]),
                reply,
            ))
            .unwrap();
        engine.abort().unwrap();
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
    }

    #[test]
    fn named_database_workers_share_and_release_the_query_pool() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        config.memory.query = crate::size::ByteSize(24_000);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        dbs.create("second").unwrap();
        let mut a = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
        let mut b = DbEngine::new(Arc::clone(&dbs), "second".into()).unwrap();
        let params = PackValue::Map(vec![("text".into(), PackValue::String("x".repeat(3000)))]);
        a.begin(None).unwrap();
        a.run(None, "RETURN $text", &params).unwrap();
        let held = dbs.query_pool().usage().reserved_bytes;
        b.begin(None).unwrap();
        let error = b
            .run(None, "RETURN $text", &params)
            .err()
            .expect("shared pool must reject");
        assert_eq!(
            error.code,
            "Neo.TransientError.General.QueryMemoryPoolExhausted"
        );
        assert!(b.commit().is_err());
        b.abort().unwrap();
        assert_eq!(dbs.query_pool().usage().reserved_bytes, held);
        a.abort().unwrap();
        assert_eq!(dbs.query_pool().usage().reserved_bytes, 0);
        b.begin(None).unwrap();
        b.run(None, "RETURN $text", &params).unwrap();
        b.commit().unwrap();
        assert_eq!(dbs.query_pool().usage().reserved_bytes, 0);
    }

    fn scoped_engine_fixture() -> (tempfile::TempDir, Arc<Databases>, DbEngine) {
        scoped_engine_fixture_with_timeout(30_000)
    }

    fn scoped_engine_fixture_with_timeout(
        timeout_ms: u64,
    ) -> (tempfile::TempDir, Arc<Databases>, DbEngine) {
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.query_timeout_ms = timeout_ms;
        config.server.data_dir = directory.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        databases.create("other").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        databases
            .get("default")
            .unwrap()
            .apply_ontology(&mapping)
            .unwrap();
        let engine = DbEngine::new_protected(Arc::clone(&databases), "default".into()).unwrap();
        (directory, databases, engine)
    }

    #[test]
    fn online_checkpoint_uses_configured_root_and_requires_instance_admin() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkpoints");
        std::fs::create_dir(&root).unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = directory.path().join("data");
        config.server.checkpoint_dir = Some(root.clone());
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        databases
            .get("default")
            .unwrap()
            .apply_ontology(&mapping)
            .unwrap();
        let mut engine = DbEngine::new_protected(Arc::clone(&databases), "default".into()).unwrap();
        let empty = PackValue::Map(vec![]);
        let query = "CALL db.checkpoint('online-1') YIELD name, tracked RETURN name";
        let principal = |instance_admin| {
            AccessContext::Authenticated(brainwire_bolt::auth::Principal {
                id: "synthetic-operator".into(),
                graphs: BTreeMap::from([("default".into(), GraphAccess::Write)]),
                instance_admin,
                node_visibility: None,
                visibility_delegate: false,
            })
        };

        engine.set_access(principal(false)).unwrap();
        engine.begin(None).unwrap();
        assert_eq!(
            engine.run(None, query, &empty).err().unwrap().code,
            "Neo.ClientError.Security.Forbidden"
        );
        engine.abort().unwrap();
        assert!(!root.join("online-1").exists());

        engine.set_access(principal(true)).unwrap();
        engine.begin(None).unwrap();
        engine
            .run(None, "CREATE (:Customer {entity_id:'kept'})", &empty)
            .unwrap();
        engine.commit().unwrap();
        engine.begin(None).unwrap();
        engine.run(None, query, &empty).unwrap();
        engine.commit().unwrap();
        let copy = brainwire_store::db::Database::open(
            &root.join("online-1"),
            &brainwire_store::db::Resources::new(brainwire_store::db::StoreConfig::for_tests()),
        )
        .unwrap();
        assert_eq!(copy.stats().unwrap().nodes, 1);
    }

    fn grant(graph: &str, access: GraphAccess) -> AccessContext {
        AccessContext::Authenticated(brainwire_bolt::auth::Principal {
            id: "synthetic-client".into(),
            graphs: BTreeMap::from([(graph.into(), access)]),
            instance_admin: false,
            node_visibility: None,
            visibility_delegate: false,
        })
    }

    #[test]
    fn trusted_node_visibility_survives_runs_and_resets_on_identity_change() {
        use brainwire_core::acl::{NodeAcl, ReadVisibility};
        use brainwire_store::graph::{find_by_entity_id, set_node_acl};
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine.begin(None).unwrap();
        engine.run(None, "MERGE (a:Customer {entity_id:'alice'}) MERGE (b:Customer {entity_id:'bob'}) MERGE (u:Customer {entity_id:'untagged'})", &PackValue::Map(vec![])).unwrap();
        engine.commit().unwrap();
        let database = databases.get("default").unwrap();
        let mut tx = database.begin();
        for name in ["alice", "bob"] {
            let id = find_by_entity_id(&tx, name, None).unwrap()[0];
            set_node_acl(
                &mut tx,
                id,
                &NodeAcl {
                    allow: vec![name.into()],
                    ..Default::default()
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let access = |name: &str| {
            AccessContext::Authenticated(brainwire_bolt::auth::Principal {
                id: name.into(),
                graphs: BTreeMap::from([("default".into(), GraphAccess::Read)]),
                instance_admin: false,
                node_visibility: Some(ReadVisibility {
                    principals: vec![name.into()],
                    ..Default::default()
                }),
                visibility_delegate: false,
            })
        };
        engine.set_access(access("alice")).unwrap();
        engine.begin(None).unwrap();
        let spoof = PackValue::Map(vec![(
            "acl_principals".into(),
            PackValue::List(vec![PackValue::String("bob".into())]),
        )]);
        assert_eq!(
            engine
                .run(None, "MATCH (c:Customer) RETURN c.entity_id", &spoof)
                .unwrap()
                .rows,
            vec![vec![PackValue::String("alice".into())]]
        );
        assert_eq!(
            engine
                .run(
                    None,
                    "MATCH (c:Customer) RETURN count(*)",
                    &PackValue::Map(vec![])
                )
                .unwrap()
                .rows,
            vec![vec![PackValue::Int(1)]]
        );
        // Identity replacement aborts the old snapshot; the new transaction gets its own policy.
        engine.set_access(access("bob")).unwrap();
        assert!(engine.commit().is_err());
        engine.begin(None).unwrap();
        assert_eq!(
            engine
                .run(
                    None,
                    "MATCH (c:Customer) RETURN c.entity_id",
                    &PackValue::Map(vec![])
                )
                .unwrap()
                .rows,
            vec![vec![PackValue::String("bob".into())]]
        );
        engine.commit().unwrap();
        engine.set_access(AccessContext::Unauthenticated).unwrap();
        assert_eq!(
            engine.begin(None).unwrap_err().code,
            "Neo.ClientError.Security.Forbidden"
        );
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine.begin(None).unwrap();
        assert_eq!(
            engine
                .run(
                    None,
                    "MATCH (c:Customer) RETURN count(*)",
                    &PackValue::Map(vec![])
                )
                .unwrap()
                .rows,
            vec![vec![PackValue::Int(3)]]
        );
        engine.abort().unwrap();
    }

    #[test]
    fn a_visibility_delegate_reads_under_each_transactions_visibility() {
        use brainwire_core::acl::{NodeAcl, ReadVisibility};
        use brainwire_store::graph::{find_by_entity_id, set_node_acl};
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        let empty = PackValue::Map(vec![]);
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine.begin(None).unwrap();
        engine
            .run(
                None,
                "MERGE (a:Customer {entity_id:'alice'}) MERGE (b:Customer {entity_id:'bob'})",
                &empty,
            )
            .unwrap();
        engine.commit().unwrap();
        let database = databases.get("default").unwrap();
        let mut tx = database.begin();
        for name in ["alice", "bob"] {
            let id = find_by_entity_id(&tx, name, None).unwrap()[0];
            set_node_acl(
                &mut tx,
                id,
                &NodeAcl {
                    allow: vec![name.into()],
                    ..Default::default()
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let principal = |delegate| {
            AccessContext::Authenticated(brainwire_bolt::auth::Principal {
                id: "reader".into(),
                graphs: BTreeMap::from([("default".into(), GraphAccess::Write)]),
                instance_admin: false,
                node_visibility: None,
                visibility_delegate: delegate,
            })
        };
        let caller = |name: &str| ReadVisibility {
            principals: vec![name.into()],
            ..Default::default()
        };
        let ids = |engine: &mut DbEngine| {
            engine
                .run(
                    None,
                    "MATCH (c:Customer) RETURN c.entity_id",
                    &PackValue::Map(vec![]),
                )
                .unwrap()
                .rows
        };

        engine.set_access(principal(true)).unwrap();
        engine
            .begin_delegated(None, true, None, caller("bob"))
            .unwrap();
        assert_eq!(
            ids(&mut engine),
            vec![vec![PackValue::String("bob".into())]]
        );
        engine.commit().unwrap();
        // A delegate's write is refused even with a write grant.
        engine
            .begin_delegated(None, false, None, caller("alice"))
            .unwrap();
        assert_eq!(
            engine
                .run(None, "MERGE (n:Customer {entity_id:'x'})", &empty)
                .err()
                .unwrap()
                .code,
            "Neo.ClientError.Security.Forbidden"
        );
        engine.abort().unwrap();
        // Missing caller visibility must fail closed for both transaction modes.
        assert_eq!(
            engine.begin(None).unwrap_err().code,
            "Neo.ClientError.Security.Forbidden"
        );
        assert_eq!(
            engine.begin_read_only(None).unwrap_err().code,
            "Neo.ClientError.Security.Forbidden"
        );
        // A principal that is not a delegate cannot send one.
        engine.set_access(principal(false)).unwrap();
        assert_eq!(
            engine
                .begin_delegated(None, true, None, caller("bob"))
                .unwrap_err()
                .code,
            "Neo.ClientError.Security.Forbidden"
        );
    }

    #[test]
    fn read_only_transaction_disallows_writes_and_acl_stamp_for_admin() {
        let (_directory, _databases, mut engine) = scoped_engine_fixture();
        engine.set_access(AccessContext::Unrestricted).unwrap();
        for query in ["MERGE (n:Customer {entity_id:'read-write'})", "CALL db.setNodeAcl({label:'Customer',entityId:'read-write',allow:['user']}) YIELD updated RETURN updated"] {
            engine.begin_read_only(None).unwrap();
            assert_eq!(engine.run(None, query, &PackValue::Map(vec![])).err().expect("read mode must deny mutation").code, "Neo.ClientError.Security.Forbidden");
            engine.abort().unwrap();
        }
        engine.begin_read_only(None).unwrap();
        assert_eq!(
            engine
                .run(
                    None,
                    "MATCH (n:Customer) RETURN count(*)",
                    &PackValue::Map(vec![])
                )
                .unwrap()
                .rows,
            vec![vec![PackValue::Int(0)]]
        );
        engine.commit().unwrap();
        engine.begin(None).unwrap();
        engine
            .run(
                None,
                "MERGE (n:Customer {entity_id:'write-mode'})",
                &PackValue::Map(vec![]),
            )
            .unwrap();
        engine.commit().unwrap();
    }

    #[test]
    fn protected_engine_checks_default_and_explicit_graph_before_lookup() {
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        for name in [None, Some("other"), Some("missing")] {
            assert_eq!(
                engine.begin(name).unwrap_err().code,
                "Neo.ClientError.Security.Forbidden"
            );
        }
        engine
            .set_access(grant("other", GraphAccess::Read))
            .unwrap();
        assert_eq!(
            engine.begin(None).unwrap_err().code,
            "Neo.ClientError.Security.Forbidden"
        );
        engine.begin(Some("other")).unwrap();
        assert_eq!(
            engine
                .run(None, "RETURN 1", &PackValue::Map(vec![]))
                .unwrap()
                .rows,
            vec![vec![PackValue::Int(1)]]
        );
        engine.commit().unwrap();
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        engine.set_access(AccessContext::Unauthenticated).unwrap();
        assert_eq!(
            engine.begin(Some("other")).unwrap_err().code,
            "Neo.ClientError.Security.Forbidden"
        );
    }

    #[test]
    fn graph_reader_cannot_mutate_and_writer_cannot_read_instance_telemetry() {
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        let empty = PackValue::Map(vec![]);
        engine
            .set_access(grant("default", GraphAccess::Read))
            .unwrap();
        engine.begin(None).unwrap();
        engine
            .run(
                None,
                "CALL db.schema() YIELD version RETURN version",
                &empty,
            )
            .unwrap();
        assert_eq!(
            engine
                .run(None, "MERGE (c:Customer {entity_id:'denied'})", &empty)
                .err()
                .unwrap()
                .code,
            "Neo.ClientError.Security.Forbidden"
        );
        assert!(engine.commit().is_err());
        engine.abort().unwrap();
        let db = databases.get("default").unwrap();
        assert_eq!(db.stats().unwrap().nodes, 0);
        engine
            .set_access(grant("default", GraphAccess::Write))
            .unwrap();
        engine.begin(None).unwrap();
        engine
            .run(None, "CREATE (c:Customer {entity_id:'pending'})", &empty)
            .unwrap();
        assert_eq!(
            engine
                .run(
                    None,
                    "CALL db.memory() YIELD poolLimitBytes RETURN poolLimitBytes",
                    &empty
                )
                .err()
                .unwrap()
                .code,
            "Neo.ClientError.Security.Forbidden"
        );
        assert!(engine.commit().is_err());
        engine.abort().unwrap();
        assert_eq!(db.stats().unwrap().nodes, 0);
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
    }

    #[test]
    fn privilege_change_and_graph_switch_discard_pending_writes() {
        let (_directory, databases, mut engine) = scoped_engine_fixture();
        let empty = PackValue::Map(vec![]);
        let db = databases.get("default").unwrap();
        for change_principal in [true, false] {
            engine
                .set_access(grant("default", GraphAccess::Write))
                .unwrap();
            engine.begin(None).unwrap();
            engine
                .run(None, "CREATE (c:Customer {entity_id:'pending'})", &empty)
                .unwrap();
            if change_principal {
                engine.set_access(AccessContext::Unauthenticated).unwrap();
            } else {
                assert_eq!(
                    engine
                        .run(Some("other"), "RETURN 1", &empty)
                        .err()
                        .unwrap()
                        .code,
                    "Neo.ClientError.Security.Forbidden"
                );
            }
            assert!(engine.commit().is_err());
            engine.abort().unwrap();
            assert_eq!(db.stats().unwrap().nodes, 0);
            assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        }
    }

    fn service_credentials(principal: &str) -> PackValue {
        PackValue::Map(vec![
            ("scheme".into(), PackValue::String("basic".into())),
            ("principal".into(), PackValue::String(principal.into())),
            (
                "credentials".into(),
                PackValue::String(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                ),
            ),
        ])
    }

    fn protected_session(engine: DbEngine) -> Session<DbEngine> {
        let policy = brainwire_bolt::auth::AuthPolicy::from_json(br#"{"principals":[{"id":"writer","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"default":"write"},"instance_admin":false},{"id":"reader","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"default":"read"},"instance_admin":false}]}"#).unwrap();
        let mut session = Session::new(engine, AuthMode::Required(Arc::new(policy)));
        session.handle(Request::Hello {
            extra: PackValue::Map(vec![]),
        });
        assert!(matches!(
            session.handle(Request::Logon {
                auth: service_credentials("writer")
            })[0],
            Response::Success(_)
        ));
        session
    }

    fn stage_session_write(session: &mut Session<DbEngine>, databases: &Databases, explicit: bool) {
        let empty = PackValue::Map(vec![]);
        if explicit {
            assert!(matches!(
                session.handle(Request::Begin {
                    extra: empty.clone()
                })[0],
                Response::Success(_)
            ));
        }
        let result = session.handle(Request::Run {
            query: "CREATE (c:Customer {entity_id:'pending-auth'}) RETURN 'pending-row'".into(),
            params: empty.clone(),
            extra: empty,
        });
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Response::Success(_)));
        assert_eq!(
            session.state(),
            if explicit {
                brainwire_bolt::session::State::TxStreaming
            } else {
                brainwire_bolt::session::State::Streaming
            }
        );
        assert!(databases.query_pool().usage().reserved_bytes > 0);
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
    }

    fn assert_session_write_discarded(databases: &Databases) {
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
    }

    fn check_protected_logoff(explicit: bool) {
        let (_directory, databases, engine) = scoped_engine_fixture();
        let mut session = protected_session(engine);
        stage_session_write(&mut session, &databases, explicit);
        let result = session.handle(Request::Logoff);
        assert_eq!(result, vec![Response::Success(vec![])]);
        assert_eq!(
            session.state(),
            brainwire_bolt::session::State::Authentication
        );
        assert_session_write_discarded(&databases);

        assert!(matches!(
            session.handle(Request::Logon {
                auth: service_credentials("reader")
            })[0],
            Response::Success(_)
        ));
        // A new identity cannot fetch the old identity's unconsumed result.
        let result = session.handle(Request::Pull {
            extra: PackValue::Map(vec![]),
        });
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Response::Failure { .. }));
        session.handle(Request::Reset);
        session.handle(Request::Run {
            query: "MATCH (c:Customer) RETURN count(*)".into(),
            params: PackValue::Map(vec![]),
            extra: PackValue::Map(vec![]),
        });
        let result = session.handle(Request::Pull {
            extra: PackValue::Map(vec![]),
        });
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Response::Record(vec![PackValue::Int(0)]));
        assert!(matches!(result[1], Response::Success(_)));
        drop(result); // The transport has finished writing the previous response.
        let denied = session.handle(Request::Run {
            query: "CREATE (c:Customer {entity_id:'reader-write'})".into(),
            params: PackValue::Map(vec![]),
            extra: PackValue::Map(vec![]),
        });
        assert!(
            matches!(&denied[0], Response::Failure { code, .. } if code == "Neo.ClientError.Security.Forbidden")
        );
        assert_session_write_discarded(&databases);
    }

    #[test]
    fn protected_session_logoff_aborts_implicit_write_and_unconsumed_rows() {
        check_protected_logoff(false);
    }

    #[test]
    fn protected_session_logoff_aborts_explicit_write_and_unconsumed_rows() {
        check_protected_logoff(true);
    }

    #[test]
    fn protected_session_reset_cannot_restore_logged_out_access() {
        for explicit in [false, true] {
            let (_directory, databases, engine) = scoped_engine_fixture();
            let mut session = protected_session(engine);
            stage_session_write(&mut session, &databases, explicit);
            session.handle(Request::Logoff);
            assert!(matches!(
                session.handle(Request::Reset)[0],
                Response::Failure { .. }
            ));
            assert_eq!(session.state(), brainwire_bolt::session::State::Defunct);
            for request in [
                Request::Logon {
                    auth: service_credentials("writer"),
                },
                Request::Run {
                    query: "RETURN 1".into(),
                    params: PackValue::Map(vec![]),
                    extra: PackValue::Map(vec![]),
                },
                Request::Pull {
                    extra: PackValue::Map(vec![]),
                },
                Request::Commit,
            ] {
                assert!(session.handle(request).is_empty());
            }
            assert_session_write_discarded(&databases);
        }
    }

    #[test]
    fn protected_session_repeated_or_invalid_logon_aborts_streaming_writes() {
        for explicit in [false, true] {
            for valid_credentials in [false, true] {
                let (_directory, databases, engine) = scoped_engine_fixture();
                let mut session = protected_session(engine);
                stage_session_write(&mut session, &databases, explicit);
                let auth = if valid_credentials {
                    service_credentials("writer")
                } else {
                    PackValue::Map(vec![])
                };
                let result = session.handle(Request::Logon { auth });
                assert_eq!(result.len(), 1);
                assert!(matches!(result[0], Response::Failure { .. }));
                assert_eq!(session.state(), brainwire_bolt::session::State::Defunct);
                assert!(session
                    .handle(Request::Pull {
                        extra: PackValue::Map(vec![])
                    })
                    .is_empty());
                assert!(session.handle(Request::Commit).is_empty());
                assert!(session.handle(Request::Reset).is_empty());
                assert_session_write_discarded(&databases);
            }
        }
    }

    #[test]
    fn configured_query_guard_rejects_large_runs_and_discards_prior_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        config.memory.query = crate::size::ByteSize(32_768);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("default").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut engine = DbEngine::new(dbs, "default".into()).unwrap();
        let empty = PackValue::Map(vec![]);
        engine.begin(None).unwrap();
        engine
            .run(
                None,
                "CREATE (c:Customer {entity_id:'pending-limit'})",
                &empty,
            )
            .unwrap();
        let params = PackValue::Map(vec![(
            "large".into(),
            PackValue::String("x".repeat(40_000)),
        )]);
        let error = engine
            .run(None, "RETURN $large", &params)
            .err()
            .expect("guard must reject oversized parameter");
        assert_eq!(error.code, "Neo.ClientError.Statement.MemoryLimitExceeded");
        assert!(engine.commit().is_err());
        engine.abort().unwrap();
        assert_eq!(db.stats().unwrap().nodes, 0);
        engine.begin(None).unwrap();
        assert_eq!(
            engine.run(None, "RETURN 1", &empty).unwrap().rows,
            vec![vec![PackValue::Int(1)]]
        );
        engine.commit().unwrap();
    }

    #[test]
    fn worker_preserves_transaction_visibility_commit_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("default").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut engine = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
        let empty = PackValue::Map(vec![]);
        assert_eq!(
            engine.begin(Some("missing")).unwrap_err().code,
            "Neo.ClientError.Database.DatabaseNotFound"
        );
        engine.abort().unwrap();

        engine.begin(None).unwrap();
        engine
            .run(None, "CREATE (c:Customer {entity_id: 'one'})", &empty)
            .unwrap();
        assert_eq!(db.stats().unwrap().nodes, 0);
        assert_eq!(
            engine
                .run(None, "MATCH (c:Customer) RETURN count(*)", &empty)
                .unwrap()
                .rows,
            vec![vec![PackValue::Int(1)]]
        );
        engine.abort().unwrap();
        assert_eq!(db.stats().unwrap().nodes, 0);

        engine.begin(None).unwrap();
        engine
            .run(None, "CREATE (c:Customer {entity_id: 'two'})", &empty)
            .unwrap();
        assert_eq!(engine.commit().unwrap(), None);
        assert_eq!(db.stats().unwrap().nodes, 1);

        engine.begin(None).unwrap();
        engine
            .run(None, "CREATE (c:Customer {entity_id: 'three'})", &empty)
            .unwrap();
        assert!(engine.run(None, "this is not Cypher", &empty).is_err());
        engine.abort().unwrap();
        assert_eq!(db.stats().unwrap().nodes, 1);
    }

    #[test]
    fn production_session_reset_and_drop_discard_pending_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("default").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let empty = PackValue::Map(vec![]);
        for reset in [true, false] {
            let engine = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
            let mut session = Session::new(engine, AuthMode::AllowAll);
            session.handle(Request::Hello {
                extra: empty.clone(),
            });
            session.handle(Request::Logon {
                auth: empty.clone(),
            });
            assert!(matches!(
                session.handle(Request::Begin {
                    extra: empty.clone()
                })[0],
                Response::Success(_)
            ));
            assert!(matches!(
                session.handle(Request::Run {
                    query: format!("CREATE (c:Customer {{entity_id: 'pending-{reset}'}})"),
                    params: empty.clone(),
                    extra: empty.clone(),
                })[0],
                Response::Success(_)
            ));
            session.handle(Request::Pull {
                extra: empty.clone(),
            });
            if reset {
                assert!(matches!(
                    session.handle(Request::Reset)[0],
                    Response::Success(_)
                ));
            }
            drop(session);
            assert_eq!(db.stats().unwrap().nodes, 0);
        }
    }

    #[test]
    fn concurrent_writers_report_retryable_commit_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("default").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut first = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
        let mut second = DbEngine::new(Arc::clone(&dbs), "default".into()).unwrap();
        let empty = PackValue::Map(vec![]);
        first.begin(None).unwrap();
        second.begin(None).unwrap();
        first
            .run(None, "CREATE (c:Customer {entity_id: 'same'})", &empty)
            .unwrap();
        second
            .run(None, "CREATE (c:Customer {entity_id: 'same'})", &empty)
            .unwrap();
        first.commit().unwrap();
        let err = second.commit().unwrap_err();
        assert_eq!(err.code, "Neo.TransientError.Transaction.Outdated");
        second.abort().unwrap();
        assert_eq!(db.stats().unwrap().nodes, 1);
    }
    fn timeout_extra(ms: i64) -> PackValue {
        PackValue::Map(vec![("tx_timeout".into(), PackValue::Int(ms))])
    }

    fn assert_transaction_timeout(responses: &[Response]) {
        assert!(
            matches!(responses, [Response::Failure {code,..}]
            if code == "Neo.ClientError.Statement.ExecutionTimedOut"),
            "{responses:?}"
        );
    }

    #[test]
    fn client_timeout_rolls_back_idle_explicit_writes_and_reset_recovers() {
        let (_directory, databases, engine) = scoped_engine_fixture();
        let mut session = protected_session(engine);
        assert!(matches!(
            session.handle(Request::Begin {
                extra: timeout_extra(100)
            })[0],
            Response::Success(_)
        ));
        assert!(matches!(
            session.handle(Request::Run {
                query: "CREATE (:Customer {entity_id:'timeout-staged'}) RETURN 1".into(),
                params: PackValue::Map(vec![]),
                extra: PackValue::Map(vec![])
            })[0],
            Response::Success(_)
        ));
        assert!(databases.query_pool().usage().reserved_bytes > 0);
        assert!(matches!(
            session
                .handle(Request::Pull {
                    extra: PackValue::Map(vec![])
                })
                .last(),
            Some(Response::Success(_))
        ));
        thread::sleep(Duration::from_millis(180));
        assert!(databases.query_pool().usage().reserved_bytes > 0);
        assert_transaction_timeout(&session.handle(Request::Commit));
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
        assert!(matches!(
            session.handle(Request::Reset)[0],
            Response::Success(_)
        ));
        assert!(matches!(
            session.handle(Request::Run {
                query: "RETURN 1".into(),
                params: PackValue::Map(vec![]),
                extra: PackValue::Map(vec![])
            })[0],
            Response::Success(_)
        ));
    }

    #[test]
    fn client_timeout_prevents_implicit_pull_of_expired_rows_and_writes() {
        let (_directory, databases, engine) = scoped_engine_fixture();
        let mut session = protected_session(engine);
        assert!(matches!(
            session.handle(Request::Run {
                query: "CREATE (:Customer {entity_id:'timeout-implicit'}) RETURN 'buffered'".into(),
                params: PackValue::Map(vec![]),
                extra: timeout_extra(100)
            })[0],
            Response::Success(_)
        ));
        thread::sleep(Duration::from_millis(180));
        assert!(databases.query_pool().usage().reserved_bytes > 0);
        assert_transaction_timeout(&session.handle(Request::Pull {
            extra: PackValue::Map(vec![]),
        }));
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
        assert!(matches!(
            session.handle(Request::Reset)[0],
            Response::Success(_)
        ));
    }

    #[test]
    fn client_deadline_is_shared_across_runs_instead_of_resetting() {
        let (_directory, _databases, engine) = scoped_engine_fixture();
        let mut session = protected_session(engine);
        assert!(matches!(
            session.handle(Request::Begin {
                extra: timeout_extra(180)
            })[0],
            Response::Success(_)
        ));
        thread::sleep(Duration::from_millis(100));
        assert!(matches!(
            session.handle(Request::Run {
                query: "RETURN 1".into(),
                params: PackValue::Map(vec![]),
                extra: PackValue::Map(vec![])
            })[0],
            Response::Success(_)
        ));
        session.handle(Request::Pull {
            extra: PackValue::Map(vec![]),
        });
        thread::sleep(Duration::from_millis(120));
        assert_transaction_timeout(&session.handle(Request::Run {
            query: "RETURN 2".into(),
            params: PackValue::Map(vec![]),
            extra: PackValue::Map(vec![]),
        }));
    }

    #[test]
    fn client_timeout_cancels_active_low_memory_fanout() {
        let (_directory, databases, engine) = scoped_engine_fixture();
        let mut session = protected_session(engine);
        let began = Instant::now();
        let responses = session.handle(Request::Run {
            query: "UNWIND $v AS a UNWIND $v AS b UNWIND $v AS c UNWIND $v AS d RETURN count(*)"
                .into(),
            params: PackValue::Map(vec![(
                "v".into(),
                PackValue::List((0..200).map(PackValue::Int).collect()),
            )]),
            extra: timeout_extra(50),
        });
        assert_transaction_timeout(&responses);
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "cooperative fanout deadline was not enforced"
        );
        assert_eq!(databases.query_pool().usage().reserved_bytes, 0);
    }

    #[test]
    fn server_timeout_caps_client_duration_without_changing_legacy_transactions() {
        let (_directory, _databases, mut engine) = scoped_engine_fixture_with_timeout(50);
        engine.set_access(AccessContext::Unrestricted).unwrap();
        engine
            .begin_with_timeout(
                None,
                false,
                None,
                None,
                Some(Duration::from_millis(i64::MAX as u64)),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(120));
        assert_eq!(
            engine.commit().unwrap_err().code,
            "Neo.ClientError.Statement.ExecutionTimedOut"
        );
        engine.abort().unwrap();
        // Missing/zero client timeout retains the existing per-RUN server policy.
        for timeout in [None, Some(Duration::ZERO)] {
            engine
                .begin_with_timeout(None, false, None, None, timeout)
                .unwrap();
            thread::sleep(Duration::from_millis(80));
            engine
                .run(None, "RETURN 1", &PackValue::Map(vec![]))
                .unwrap();
            engine.commit().unwrap();
        }
    }
}
