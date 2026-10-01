//! Cypher/Bolt value conversion and the durable per-connection engine.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};
use std::thread;

use brainwire_bolt::packstream::PackValue;
use brainwire_bolt::session::{EngineError, QueryEngine, QueryResult};
use brainwire_core::value::Value;
use brainwire_cypher::{ExecError, Out, QueryOutput, QueryTransaction};

use crate::databases::Databases;

enum Command {
    Begin(Option<String>, mpsc::Sender<Result<(), EngineError>>),
    Run(
        String,
        PackValue,
        mpsc::Sender<Result<QueryResult, EngineError>>,
    ),
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
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("brainwire-query".into())
            .spawn(move || worker(receiver, databases, default_database))
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
    fn begin(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        self.ask(|reply| Command::Begin(db.map(str::to_owned), reply))
    }

    fn run(
        &mut self,
        _db: Option<&str>,
        query: &str,
        params: &PackValue,
    ) -> Result<QueryResult, EngineError> {
        self.ask(|reply| Command::Run(query.to_owned(), params.clone(), reply))
    }

    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        self.ask(Command::Commit)
    }

    fn abort(&mut self) -> Result<(), EngineError> {
        self.ask(Command::Abort)
    }
}

fn worker_error() -> EngineError {
    error(
        "Neo.DatabaseError.General.UnknownError",
        "query worker stopped",
    )
}

fn worker(receiver: mpsc::Receiver<Command>, databases: Arc<Databases>, default_database: String) {
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Begin(name, reply) => {
                let name = name.as_deref().unwrap_or(&default_database);
                let Some(database) = databases.get(name) else {
                    let _ = reply.send(Err(error(
                        "Neo.ClientError.Database.DatabaseNotFound",
                        format!("database {name:?} not found"),
                    )));
                    continue;
                };
                let mut transaction = match QueryTransaction::begin(&database) {
                    Ok(transaction) => transaction,
                    Err(err) => {
                        let _ = reply.send(Err(execution_error(err)));
                        continue;
                    }
                };
                let _ = reply.send(Ok(()));
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Run(query, params, reply) => {
                            let result = params_from_pack(&params)
                                .and_then(|params| {
                                    transaction.run(&query, &params).map_err(execution_error)
                                })
                                .and_then(output_to_bolt);
                            let _ = reply.send(result);
                        }
                        Command::Commit(reply) => {
                            let result =
                                transaction.commit().map(|_| None).map_err(execution_error);
                            let _ = reply.send(result);
                            break;
                        }
                        Command::Abort(reply) => {
                            transaction.rollback();
                            let _ = reply.send(Ok(()));
                            break;
                        }
                        Command::Begin(_, reply) => {
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
            Command::Run(_, _, reply) => {
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

fn out_to_pack(output: Out) -> Result<PackValue, EngineError> {
    match output {
        Out::Value(value) => value_to_pack(value, 0),
        Out::Node(node) => {
            let id = i64::try_from(node.id.0)
                .map_err(|_| type_error("node id exceeds Bolt integer range"))?;
            let props = node
                .props
                .into_iter()
                .map(|(key, value)| value_to_pack(value, 0).map(|value| (key, value)))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PackValue::Struct {
                tag: 0x4e,
                fields: vec![
                    PackValue::Int(id),
                    PackValue::List(vec![PackValue::String(node.label)]),
                    PackValue::Map(props),
                    PackValue::String(node.id.element_id()),
                ],
            })
        }
    }
}

fn output_to_bolt(output: QueryOutput) -> Result<QueryResult, EngineError> {
    let rows = output
        .rows
        .into_iter()
        .map(|row| row.into_iter().map(out_to_pack).collect::<Result<_, _>>())
        .collect::<Result<_, _>>()?;
    Ok(QueryResult {
        fields: output.fields,
        rows,
    })
}

fn error_code(error: &ExecError) -> &'static str {
    match error {
        ExecError::Syntax(_) | ExecError::Unsupported(_) => "Neo.ClientError.Statement.SyntaxError",
        ExecError::Semantic(_) => "Neo.ClientError.Statement.SemanticError",
        ExecError::ParameterMissing(_) => "Neo.ClientError.Statement.ParameterMissing",
        ExecError::ConstraintViolation(_) => "Neo.ClientError.Schema.ConstraintValidationFailed",
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
    fn worker_preserves_transaction_visibility_commit_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let dbs = Arc::new(Databases::open_all(&config).unwrap());
        let db = dbs.get("client_memory").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut engine = DbEngine::new(Arc::clone(&dbs), "client_memory".into()).unwrap();
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
        let db = dbs.get("client_memory").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let empty = PackValue::Map(vec![]);
        for reset in [true, false] {
            let engine = DbEngine::new(Arc::clone(&dbs), "client_memory".into()).unwrap();
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
        let db = dbs.get("client_memory").unwrap();
        let mapping: GraphMapping =
            serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json"))
                .unwrap();
        db.apply_ontology(&mapping).unwrap();
        let mut first = DbEngine::new(Arc::clone(&dbs), "client_memory".into()).unwrap();
        let mut second = DbEngine::new(Arc::clone(&dbs), "client_memory".into()).unwrap();
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
}
