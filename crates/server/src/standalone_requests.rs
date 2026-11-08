//! Experimental standalone request identity. Only the server supplies a scope.
use super::{error, forbidden};
use brainwire_bolt::auth::AccessContext;
use brainwire_bolt::packstream::PackValue;
use brainwire_bolt::session::{EngineError, QueryResult};
use brainwire_core::value::Value;
use brainwire_cypher::ast::{Clause, Expr};
use brainwire_store::db::request_ledger::LedgerError;
use brainwire_store::db::{Database, StoreError};
use std::collections::BTreeMap;

fn scope(access: &AccessContext) -> Result<&str, EngineError> {
    match access {
        AccessContext::Authenticated(principal)
            if principal.node_visibility.is_none() && !principal.visibility_delegate =>
        {
            Ok(&principal.id)
        }
        _ => Err(forbidden()),
    }
}

pub(super) fn identified_scope(
    access: &AccessContext,
    enabled: bool,
    can_write: bool,
    read_only: bool,
    restricted: bool,
    autocommit: bool,
) -> Result<&str, EngineError> {
    if !enabled {
        return Err(error(
            "Neo.ClientError.Request.Invalid",
            "standalone request identity requires experimental server.request_ledger",
        ));
    }
    if !can_write || read_only || restricted || autocommit {
        return Err(forbidden());
    }
    scope(access)
}

pub(super) fn already_committed() -> EngineError {
    error(
        "Neo.ClientError.Transaction.RequestAlreadyCommitted",
        "this principal's request is already committed; inspect its status, do not replay the transaction",
    )
}

pub(super) fn ledger_error(failure: LedgerError) -> EngineError {
    match failure {
        LedgerError::AlreadyCommitted(_) => already_committed(),
        LedgerError::PayloadMismatch => error(
            "Neo.ClientError.Transaction.RequestPayloadMismatch",
            "request identity was reused with different staged mutations [brainwire.commit outcome=rolled_back]",
        ),
        LedgerError::Full => error(
            "Neo.ClientError.Transaction.RequestLedgerFull",
            "request ledger quota is full [brainwire.commit outcome=rolled_back]",
        ),
        LedgerError::Invalid(_) | LedgerError::Disabled => error(
            "Neo.ClientError.Request.Invalid",
            "invalid or disabled standalone request identity [brainwire.commit outcome=rolled_back]",
        ),
        LedgerError::Store(StoreError::Rocks(_)) => error(
            "Neo.ClientError.Transaction.CommitOutcomeUnknown",
            "storage write result is unknown; close/reopen and reconcile the request in the same retained history [brainwire.commit outcome=unknown]",
        ),
        LedgerError::Transaction(failure) => {
            super::commit_error(brainwire_cypher::ExecError::from(failure))
        }
        _ => error(
            "Neo.TransientError.General.DatabaseUnavailable",
            "request ledger is unavailable or corrupt; reconcile after recovery",
        ),
    }
}

/// Only an isolated procedure call is accepted. Never classify request text by
/// substring: comments, literals and other branches must not select this path.
pub(super) fn status_query(
    query: &str,
    params: &BTreeMap<String, Value>,
    memory_limit: usize,
) -> Result<Option<String>, EngineError> {
    if query.len() > memory_limit {
        return Err(super::execution_error(
            brainwire_cypher::ExecError::MemoryLimit {
                limit: memory_limit,
                attempted: query.len(),
            },
        ));
    }
    let parsed = match brainwire_cypher::parse(query) {
        Ok(query) => query,
        Err(_) => return Ok(None),
    };
    let has_status = parsed
        .clauses
        .iter()
        .chain(parsed.unions.iter().flat_map(|(_, part)| part))
        .any(|clause| matches!(clause, Clause::Call { name, .. } if name == "db.requestStatus"));
    if !has_status {
        return Ok(None);
    }
    let unsupported = || {
        error(
            "Neo.ClientError.Statement.NotSupported",
            "db.requestStatus requires one isolated autocommit CALL with one seed argument; no YIELD, RETURN, WHERE or UNION",
        )
    };
    if !parsed.unions.is_empty() || parsed.clauses.len() != 1 {
        return Err(unsupported());
    }
    let Clause::Call {
        args,
        yields,
        where_,
        ..
    } = &parsed.clauses[0]
    else {
        return Err(unsupported());
    };
    if args.len() != 1 || !yields.is_empty() || where_.is_some() {
        return Err(unsupported());
    }
    let value = match &args[0] {
        Expr::Literal(value) => value,
        Expr::Param(name) => params.get(name).ok_or_else(|| {
            error(
                "Neo.ClientError.Statement.ParameterMissing",
                "request seed parameter is missing",
            )
        })?,
        _ => return Err(unsupported()),
    };
    match value {
        Value::Str(seed)
            if !seed.is_empty()
                && seed.len() <= 128
                && seed.bytes().all(|byte| byte.is_ascii_graphic()) =>
        {
            Ok(Some(seed.clone()))
        }
        Value::Str(_) => Err(error(
            "Neo.ClientError.Request.Invalid",
            "request seed must be 1..128 printable ASCII bytes",
        )),
        _ => Err(error(
            "Neo.ClientError.Statement.ArgumentError",
            "request seed must be a string",
        )),
    }
}

pub(super) fn status(
    access: &AccessContext,
    database: &Database,
    seed: &str,
    fresh_autocommit: bool,
) -> Result<QueryResult, EngineError> {
    let scope = scope(access)?;
    if !fresh_autocommit {
        return Err(error(
            "Neo.ClientError.Request.Invalid",
            "db.requestStatus requires a fresh autocommit transaction",
        ));
    }
    let receipt = database
        .request_receipt(scope, seed)
        .map_err(ledger_error)?;
    let incarnation = database
        .request_ledger_incarnation()
        .ok_or_else(|| ledger_error(LedgerError::Disabled))?;
    Ok(QueryResult {
        memory: None,
        fields: ["status", "requestId", "incarnation", "sequence"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        rows: vec![vec![
            PackValue::String(
                if receipt.is_some() {
                    "committed"
                } else {
                    "not_recorded"
                }
                .into(),
            ),
            PackValue::String(seed.to_owned()),
            PackValue::String(uuid::Uuid::from_bytes(incarnation).to_string()),
            receipt.map_or(PackValue::Null, |receipt| {
                PackValue::String(receipt.sequence.to_string())
            }),
        ]],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_ast_only_and_rejects_composition_before_other_effects() {
        let params = BTreeMap::from([("seed".into(), Value::Str("event-1".into()))]);
        assert_eq!(
            status_query("CALL db.requestStatus($seed)", &params, usize::MAX).unwrap(),
            Some("event-1".into())
        );
        assert_eq!(
            status_query("RETURN 'CALL db.requestStatus($seed)'", &params, usize::MAX).unwrap(),
            None
        );
        for query in [
            "CALL db.requestStatus($seed) RETURN 1",
            "CREATE (:A) CALL db.requestStatus($seed)",
            "RETURN 1 UNION CALL db.requestStatus($seed)",
            "CALL db.requestStatus($seed) YIELD status RETURN status",
        ] {
            assert!(status_query(query, &params, usize::MAX).is_err(), "{query}");
        }
    }
    #[test]
    fn status_rejects_invalid_seed_before_returning_it() {
        for seed in [
            String::new(),
            "x".repeat(129),
            "bad seed".to_owned(),
            "é".to_owned(),
        ] {
            let params = BTreeMap::from([("seed".into(), Value::Str(seed))]);
            let error = status_query("CALL db.requestStatus($seed)", &params, usize::MAX)
                .expect_err("invalid seed must be rejected before it is cloned");
            assert_eq!(error.code, "Neo.ClientError.Request.Invalid");
        }
    }

    #[test]
    fn status_parser_refuses_source_before_building_an_ast() {
        let failure =
            status_query(&"invalid source ".repeat(1000), &BTreeMap::new(), 1024).unwrap_err();
        assert_eq!(failure.code, "Neo.TransientError.General.MemoryPressure");
        assert!(failure.message.contains("reason=statement_limit"));
    }

    #[test]
    fn ledger_write_error_preserves_unknown_outcome() {
        assert_eq!(
            ledger_error(LedgerError::Full).code,
            "Neo.ClientError.Transaction.RequestLedgerFull"
        );
        assert!(ledger_error(LedgerError::PayloadMismatch)
            .message
            .contains("outcome=rolled_back"));
        assert!(!already_committed().message.contains("outcome=rolled_back"));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{config::Config, databases::Databases, engine::DbEngine};
    use brainwire_bolt::auth::{GraphAccess, Principal};
    use brainwire_bolt::message::{Request, Response};
    use brainwire_bolt::session::{AuthMode, QueryEngine, Session};
    use brainwire_core::acl::ReadVisibility;
    use brainwire_core::schema::GraphMapping;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    pub(super) fn fixture() -> (tempfile::TempDir, Config, Arc<Databases>) {
        fixture_with_ledger(true)
    }
    pub(super) fn fixture_with_ledger(
        enabled: bool,
    ) -> (tempfile::TempDir, Config, Arc<Databases>) {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.server.data_dir = root.path().join("data");
        config.server.auth_file = Some(root.path().join("private-policy.json"));
        config.server.request_ledger = enabled;
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        let mapping: GraphMapping = serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Item","properties":["value"],"indexed_fields":[]}],"edges":[]}"#).unwrap();
        for name in ["default", "other"] {
            databases
                .create(name)
                .unwrap()
                .apply_ontology(&mapping)
                .unwrap();
        }
        (root, config, databases)
    }

    fn principal(id: &str, permission: GraphAccess) -> AccessContext {
        AccessContext::Authenticated(Principal {
            id: id.into(),
            graphs: BTreeMap::from([("default".into(), permission), ("other".into(), permission)]),
            instance_admin: true,
            node_visibility: None,
            visibility_delegate: false,
        })
    }
    fn engine(databases: &Arc<Databases>, access: AccessContext) -> DbEngine {
        let mut engine = DbEngine::new_protected(Arc::clone(databases), "default".into()).unwrap();
        engine.set_access(access).unwrap();
        engine
    }
    fn status(engine: &mut DbEngine, graph: &str, seed: &str) -> Result<QueryResult, EngineError> {
        engine.begin_autocommit_with_timeout(Some(graph), true, None, None, None)?;
        let result = engine.run(
            Some(graph),
            "CALL db.requestStatus($seed)",
            &PackValue::Map(vec![("seed".into(), PackValue::String(seed.into()))]),
        );
        if result.is_ok() {
            engine.commit()?;
        } else {
            engine.abort()?;
        }
        result
    }
    fn identified(engine: &mut DbEngine, graph: &str, seed: &str, id: &str) {
        engine
            .begin_with_request_id(Some(graph), false, Some(seed))
            .unwrap();
        engine
            .run(
                Some(graph),
                "CREATE (:Item {entity_id:$id, value:1})",
                &PackValue::Map(vec![("id".into(), PackValue::String(id.into()))]),
            )
            .unwrap();
        engine.commit().unwrap();
    }
    pub(super) fn drain_workers(databases: &Arc<Databases>) {
        let until = Instant::now() + Duration::from_secs(3);
        while Arc::strong_count(databases) > 1 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(Arc::strong_count(databases), 1);
    }

    #[test]
    fn request_outcomes_are_scoped_to_principal_and_graph_and_survive_reopen() {
        let (_root, config, databases) = fixture();
        let mut a = engine(&databases, principal("principal-a", GraphAccess::Write));
        let mut b = engine(&databases, principal("principal-b", GraphAccess::Write));
        identified(&mut a, "default", "event-1", "a-1");
        let first = status(&mut a, "default", "event-1").unwrap();
        assert_eq!(first.rows[0][0], PackValue::String("committed".into()));
        assert_eq!(
            status(&mut b, "default", "event-1").unwrap().rows[0][0],
            PackValue::String("not_recorded".into())
        );
        assert_eq!(
            status(&mut a, "other", "event-1").unwrap().rows[0][0],
            PackValue::String("not_recorded".into())
        );
        identified(&mut b, "default", "event-1", "b-1");
        identified(&mut a, "other", "event-1", "other-1");
        assert_eq!(
            a.begin_with_request_id(Some("default"), false, Some("event-1"))
                .unwrap_err()
                .code,
            "Neo.ClientError.Transaction.RequestAlreadyCommitted"
        );
        a.abort().unwrap();
        drop(a);
        drop(b);
        drain_workers(&databases);
        drop(databases);
        let reopened = Arc::new(Databases::open_all(&config).unwrap());
        let mut a = engine(&reopened, principal("principal-a", GraphAccess::Write));
        assert_eq!(
            status(&mut a, "default", "event-1").unwrap().rows,
            first.rows
        );
    }

    #[test]
    fn missing_auth_grant_visibility_and_non_explicit_identity_are_denied() {
        let (_root, _config, databases) = fixture();
        for access in [
            AccessContext::Unrestricted,
            AccessContext::Unauthenticated,
            principal("reader", GraphAccess::Read),
        ] {
            let mut e = engine(&databases, access);
            assert!(e.begin_with_request_id(None, false, Some("seed")).is_err());
            e.abort().unwrap();
        }
        let mut no_grant = match principal("outsider", GraphAccess::Write) {
            AccessContext::Authenticated(mut p) => {
                p.graphs.clear();
                AccessContext::Authenticated(p)
            }
            _ => unreachable!(),
        };
        let mut e = engine(&databases, no_grant.clone());
        assert!(e
            .begin_autocommit_with_timeout(None, true, None, None, None)
            .is_err());
        e.abort().unwrap();
        for delegated in [false, true] {
            if let AccessContext::Authenticated(p) = &mut no_grant {
                p.graphs.insert("default".into(), GraphAccess::Write);
                p.node_visibility = (!delegated).then_some(ReadVisibility {
                    allow_all: true,
                    ..Default::default()
                });
                p.visibility_delegate = delegated;
            }
            let mut e = engine(&databases, no_grant.clone());
            assert!(e
                .begin_with_timeout(
                    None,
                    false,
                    Some("seed"),
                    delegated.then_some(ReadVisibility {
                        allow_all: true,
                        ..Default::default()
                    }),
                    None
                )
                .is_err());
            e.abort().unwrap();
            if !delegated {
                assert!(status(&mut e, "default", "seed").is_err());
            } else {
                e.begin_autocommit_with_timeout(
                    None,
                    true,
                    None,
                    Some(ReadVisibility {
                        allow_all: true,
                        ..Default::default()
                    }),
                    None,
                )
                .unwrap();
                assert!(e
                    .run(
                        None,
                        "CALL db.requestStatus('seed')",
                        &PackValue::Map(vec![])
                    )
                    .is_err());
                e.abort().unwrap();
            }
        }
        let mut e = engine(&databases, principal("writer", GraphAccess::Write));
        assert!(e
            .begin_autocommit_with_timeout(None, false, Some("seed"), None, None)
            .is_err());
        e.abort().unwrap();
        assert!(e.begin_with_request_id(None, true, Some("seed")).is_err());
        e.abort().unwrap();
    }

    #[test]
    fn failed_procedure_and_status_composition_discard_prior_identified_mutations() {
        let (_root, _config, databases) = fixture();
        let mut e = engine(&databases, principal("writer", GraphAccess::Write));
        for query in [
            "CALL db.schema()",
            "CALL db.checkpoint('must-not-exist')",
            "CREATE (:Item {entity_id:'bad'}) RETURN 1 AS x UNION CALL db.schema()",
        ] {
            e.begin_with_request_id(None, false, Some("rejected"))
                .unwrap();
            e.run(
                None,
                "CREATE (:Item {entity_id:'pending'})",
                &PackValue::Map(vec![]),
            )
            .unwrap();
            assert!(e.run(None, query, &PackValue::Map(vec![])).is_err());
            e.abort().unwrap();
            assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
            assert_eq!(
                status(&mut e, "default", "rejected").unwrap().rows[0][0],
                PackValue::String("not_recorded".into())
            );
        }
        e.begin(None).unwrap();
        assert!(e
            .run(
                None,
                "CALL db.requestStatus('missing')",
                &PackValue::Map(vec![])
            )
            .is_err());
        e.abort().unwrap();
    }

    #[test]
    fn concurrent_identified_writers_commit_once_without_callback_replay() {
        let (_root, _config, databases) = fixture();
        let db = databases.get("default").unwrap();
        brainwire_cypher::execute(
            &db,
            "CREATE (:Item {entity_id:'counter',value:0})",
            &BTreeMap::new(),
        )
        .unwrap();
        drop(db);
        let gate = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let operation = || {
                let mut e = engine(&databases, principal("writer", GraphAccess::Write));
                e.begin_with_request_id(None, false, Some("increment-1"))
                    .unwrap();
                e.run(
                    None,
                    "MATCH (n:Item {entity_id:'counter'}) SET n.value=n.value+1",
                    &PackValue::Map(vec![]),
                )
                .unwrap();
                gate.wait();
                e.commit()
            };
            let a = scope.spawn(operation);
            let b = scope.spawn(operation);
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| matches!(r, Err(e) if e.code == "Neo.ClientError.Transaction.RequestAlreadyCommitted")).count(), 1);
        let db = databases.get("default").unwrap();
        let value = brainwire_cypher::execute(
            &db,
            "MATCH (n:Item {entity_id:'counter'}) RETURN n.value",
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(matches!(
            &value.rows[0][0],
            brainwire_cypher::Out::Value(Value::Int(1))
        ));
    }

    #[test]
    fn bolt_protocol_distinguishes_status_autocommit_from_explicit_begin() {
        let (_root, _config, databases) = fixture();
        let policy = brainwire_bolt::auth::AuthPolicy::from_json(br#"{"principals":[{"id":"writer","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"default":"write"},"instance_admin":false}]}"#).unwrap();
        let e = DbEngine::new_protected(Arc::clone(&databases), "default".into()).unwrap();
        let mut s = Session::new(e, AuthMode::Required(Arc::new(policy)));
        let empty = PackValue::Map(vec![]);
        s.handle(Request::Hello {
            extra: empty.clone(),
        });
        let auth = PackValue::Map(vec![
            ("scheme".into(), PackValue::String("basic".into())),
            ("principal".into(), PackValue::String("writer".into())),
            (
                "credentials".into(),
                PackValue::String(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                ),
            ),
        ]);
        assert!(matches!(
            s.handle(Request::Logon { auth })[0],
            Response::Success(_)
        ));
        let request = || Request::Run {
            query: "CALL db.requestStatus('missing')".into(),
            params: empty.clone(),
            extra: empty.clone(),
        };
        assert!(matches!(s.handle(request())[0], Response::Success(_)));
        let response = s.handle(Request::Pull {
            extra: PackValue::Map(vec![("n".into(), PackValue::Int(-1))]),
        });
        assert!(matches!(response[0], Response::Record(_)));
        drop(response);
        assert!(matches!(
            s.handle(Request::Begin {
                extra: empty.clone()
            })[0],
            Response::Success(_)
        ));
        assert!(matches!(s.handle(request())[0], Response::Failure { .. }));
    }

    #[test]
    fn oversized_run_in_ledger_mode_is_refused_before_status_parse() {
        let (_root, mut config, databases) = fixture();
        drop(databases);
        config.memory.query = crate::size::ByteSize(16 << 10);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        let mut e = engine(&databases, principal("writer", GraphAccess::Write));
        e.begin_with_request_id(None, false, Some("oversized"))
            .unwrap();
        e.run(
            None,
            "CREATE (:Item {entity_id:'pending'})",
            &PackValue::Map(vec![]),
        )
        .unwrap();
        let source = "RETURN '".to_owned() + &"x".repeat(100_000) + "'";
        let failure = e.run(None, &source, &PackValue::Map(vec![])).err().unwrap();
        assert_eq!(failure.code, "Neo.TransientError.General.MemoryPressure");
        e.abort().unwrap();
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 0);
        assert_eq!(
            status(&mut e, "default", "oversized").unwrap().rows[0][0],
            PackValue::String("not_recorded".into())
        );
    }

    #[test]
    fn offline_check_with_opt_in_works_but_checkpoint_refuses_without_copying() {
        let (root, config, databases) = fixture();
        drop(databases);
        let path = root.path().join("config.toml");
        std::fs::write(&path, format!("[server]\ndata_dir='{}'\nauth_file='{}'\nrequest_ledger=true\n[memory]\ncache='64MiB'\nmemtables='16MiB'\n", config.server.data_dir.display(), config.server.auth_file.unwrap().display())).unwrap();
        crate::admin::check("default", true, Some(&path)).unwrap();
        let destination = root.path().join("must-not-exist");
        let failure = crate::admin::checkpoint("default", &destination, Some(&path)).unwrap_err();
        assert!(failure.contains("checkpoint/restore/fork is not supported"));
        assert!(!destination.exists());
    }

    #[test]
    fn real_storage_error_maps_to_unknown_and_never_to_rollback() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, b"not-a-directory").unwrap();
        let resources =
            brainwire_store::db::Resources::new(brainwire_store::db::StoreConfig::for_tests());
        let failure = Database::open(&path, &resources).err().unwrap();
        assert!(matches!(&failure, StoreError::Rocks(_)));
        let failure = ledger_error(LedgerError::Store(failure));
        assert_eq!(
            failure.code,
            "Neo.ClientError.Transaction.CommitOutcomeUnknown"
        );
        assert!(failure.message.contains("outcome=unknown"));
        assert!(!failure.message.contains("rolled_back"));
    }
}

#[cfg(test)]
mod wire_tests {
    use super::integration_tests::{drain_workers, fixture, fixture_with_ledger};
    use super::*;
    use crate::{databases::Databases, engine::DbEngine};
    use brainwire_bolt::session::AuthMode;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct Wire(TcpStream);
    impl Wire {
        fn connect(address: SocketAddr, principal: &str) -> Self {
            let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut client = Self(stream);
            let mut handshake = [0; 20];
            handshake[..4].copy_from_slice(&brainwire_bolt::handshake::MAGIC);
            handshake[4..8].copy_from_slice(&[0, 0, 4, 5]);
            client.0.write_all(&handshake).unwrap();
            let mut version = [0; 4];
            client.0.read_exact(&mut version).unwrap();
            assert_eq!(version, [0, 0, 4, 5]);
            client.send(
                0x01,
                vec![PackValue::Map(vec![(
                    "user_agent".into(),
                    PackValue::String("synthetic-ledger-test".into()),
                )])],
            );
            client.success();
            client.send(
                0x6a,
                vec![PackValue::Map(vec![
                    ("scheme".into(), PackValue::String("basic".into())),
                    ("principal".into(), PackValue::String(principal.into())),
                    (
                        "credentials".into(),
                        PackValue::String(
                            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                .into(),
                        ),
                    ),
                ])],
            );
            client.success();
            client
        }
        fn send(&mut self, tag: u8, fields: Vec<PackValue>) {
            let mut payload = vec![];
            brainwire_bolt::packstream::pack(&PackValue::Struct { tag, fields }, &mut payload);
            self.0
                .write_all(&brainwire_bolt::chunk::encode_message(&payload))
                .unwrap();
        }
        fn recv(&mut self) -> PackValue {
            let mut payload = vec![];
            loop {
                let mut header = [0; 2];
                self.0.read_exact(&mut header).unwrap();
                let size = usize::from(u16::from_be_bytes(header));
                if size == 0 {
                    if !payload.is_empty() {
                        break;
                    } else {
                        continue;
                    }
                }
                assert!(payload.len() + size <= 1 << 20);
                let old = payload.len();
                payload.resize(old + size, 0);
                self.0.read_exact(&mut payload[old..]).unwrap();
            }
            brainwire_bolt::packstream::unpack(&mut payload.as_slice()).unwrap()
        }
        fn success(&mut self) {
            let response = self.recv();
            assert!(
                matches!(response, PackValue::Struct { tag: 0x70, .. }),
                "{response:?}"
            );
        }
        fn failure(&mut self, code: &str) -> PackValue {
            let response = self.recv();
            let PackValue::Struct {
                tag: 0x7f,
                mut fields,
            } = response
            else {
                panic!("failure expected: {response:?}")
            };
            assert_eq!(fields[0].get("code"), Some(&PackValue::String(code.into())));
            fields.remove(0)
        }
        fn begin(&mut self, seed: &str) {
            self.send(
                0x11,
                vec![PackValue::Map(vec![(
                    "tx_metadata".into(),
                    PackValue::Map(vec![(
                        "brainwire_request_id".into(),
                        PackValue::String(seed.into()),
                    )]),
                )])],
            );
            self.success();
        }
        fn run(&mut self, query: &str) {
            self.send(
                0x10,
                vec![
                    PackValue::String(query.into()),
                    PackValue::Map(vec![]),
                    PackValue::Map(vec![]),
                ],
            );
        }
        fn pull(&mut self) -> Vec<Vec<PackValue>> {
            self.send(
                0x3f,
                vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
            );
            let mut rows = vec![];
            loop {
                match self.recv() {
                    PackValue::Struct {
                        tag: 0x71,
                        mut fields,
                    } => {
                        let PackValue::List(row) = fields.remove(0) else {
                            panic!("record row")
                        };
                        rows.push(row);
                    }
                    PackValue::Struct { tag: 0x70, .. } => return rows,
                    other => panic!("unexpected pull response: {other:?}"),
                }
            }
        }
        fn reset(&mut self) {
            self.send(0x0f, vec![]);
            self.success();
        }
        fn create(&mut self, id: &str) {
            self.run(&format!("CREATE (:Item {{entity_id:'{id}'}})"));
            self.success();
            assert!(self.pull().is_empty());
        }
        fn status(&mut self, seed: &str) -> Vec<Vec<PackValue>> {
            self.run(&format!("CALL db.requestStatus('{seed}')"));
            self.success();
            self.pull()
        }
    }

    fn serve_once(databases: &Arc<Databases>, action: impl FnOnce(SocketAddr) + Send + 'static) {
        let owned = Arc::clone(databases);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address=listener.local_addr().unwrap();
            let policy=brainwire_bolt::auth::AuthPolicy::from_json(br#"{"principals":[{"id":"writer","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"default":"write"},"instance_admin":false},{"id":"other","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"default":"read"},"instance_admin":false}]}"#).unwrap();
            let (stop,stopped)=tokio::sync::oneshot::channel();
            let server=tokio::spawn(brainwire_bolt::server::serve_until(listener,move || DbEngine::new_protected(Arc::clone(&owned),"default".into()).unwrap(),brainwire_bolt::server::ConnectionLimits::default(),AuthMode::Required(Arc::new(policy)),async move {let _=stopped.await;},Duration::from_secs(1)));
            let result=tokio::task::spawn_blocking(move || action(address)).await;
            let _=stop.send(());
            server.await.unwrap().unwrap();
            result.unwrap();
        });
        drain_workers(databases);
    }

    #[test]
    fn bolt_plain_commit_unknown_blocks_further_writes_until_reopen() {
        let (root, config, databases) = fixture_with_ledger(false);
        assert!(!databases.request_ledger_enabled());
        let db = databases.get("default").unwrap();
        let invalid = root.path().join("invalid-file");
        std::fs::write(&invalid, b"file").unwrap();
        let resources =
            brainwire_store::db::Resources::new(brainwire_store::db::StoreConfig::for_tests());
        let injected = Database::open(&invalid, &resources).err().unwrap();
        serve_once(&databases, move |address| {
            let mut client = Wire::connect(address, "writer");
            client.send(0x11, vec![PackValue::Map(vec![])]);
            client.success();
            client.create("persisted-before-plain-failure");
            db.inject_unknown_result_after_next_write(injected);
            client.send(0x12, vec![]);
            let failure = client.failure("Neo.ClientError.Transaction.CommitOutcomeUnknown");
            assert!(
                matches!(failure.get("message"), Some(PackValue::String(message))
                if message.contains("brainwire.commit outcome=unknown"))
            );
            client.reset();
            drop(client);

            let mut reconnected = Wire::connect(address, "writer");
            reconnected.send(0x11, vec![PackValue::Map(vec![])]);
            reconnected.success();
            reconnected.create("blocked-after-unknown");
            reconnected.send(0x12, vec![]);
            let failure = reconnected.failure("Neo.DatabaseError.General.UnknownError");
            assert!(
                matches!(failure.get("message"), Some(PackValue::String(message))
                if message.contains("brainwire.commit outcome=rolled_back"))
            );
            reconnected.reset();
        });
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 1);
        drop(databases);

        let reopened = Arc::new(Databases::open_all(&config).unwrap());
        assert_eq!(reopened.get("default").unwrap().stats().unwrap().nodes, 1);
        serve_once(&reopened, move |address| {
            let mut writer = Wire::connect(address, "writer");
            writer.create("normal-after-reopen");
        });
        assert_eq!(reopened.get("default").unwrap().stats().unwrap().nodes, 2);
    }

    #[test]
    fn bolt_unknown_commit_quarantines_reset_reconnect_and_reopen_reconciles() {
        let (root, config, databases) = fixture();
        let db = databases.get("default").unwrap();
        let invalid = root.path().join("invalid-file");
        std::fs::write(&invalid, b"file").unwrap();
        let resources =
            brainwire_store::db::Resources::new(brainwire_store::db::StoreConfig::for_tests());
        let injected = Database::open(&invalid, &resources).err().unwrap();
        serve_once(&databases, move |address| {
            let mut client = Wire::connect(address, "writer");
            client.begin("uncertain-wire");
            client.create("persisted-before-failure");
            db.inject_unknown_result_after_next_write(injected);
            client.send(0x12, vec![]);
            client.failure("Neo.ClientError.Transaction.CommitOutcomeUnknown");
            client.reset();
            client.run("CALL db.requestStatus('uncertain-wire')");
            client.failure("Neo.TransientError.General.DatabaseUnavailable");
            client.reset();
            drop(client);
            let mut reconnected = Wire::connect(address, "writer");
            reconnected.run("CALL db.requestStatus('uncertain-wire')");
            reconnected.failure("Neo.TransientError.General.DatabaseUnavailable");
            reconnected.reset();
        });
        assert_eq!(databases.get("default").unwrap().stats().unwrap().nodes, 1);
        drop(databases);
        let reopened = Arc::new(Databases::open_all(&config).unwrap());
        let db = reopened.get("default").unwrap();
        serve_once(&reopened, move |address| {
            let mut writer = Wire::connect(address, "writer");
            assert_eq!(
                writer.status("uncertain-wire")[0][0],
                PackValue::String("committed".into())
            );
            let mut other = Wire::connect(address, "other");
            assert_eq!(
                other.status("uncertain-wire")[0][0],
                PackValue::String("not_recorded".into())
            );
            drop(other);
            // Also lose a normal successful COMMIT acknowledgement on the wire.
            writer.begin("lost-wire-ack");
            writer.create("ack-not-read");
            writer.send(0x12, vec![]);
            drop(writer);
            let deadline = Instant::now() + Duration::from_secs(5);
            while db
                .request_receipt("writer", "lost-wire-ack")
                .unwrap()
                .is_none()
            {
                assert!(Instant::now() < deadline, "COMMIT was not consumed");
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut reconnected = Wire::connect(address, "writer");
            assert_eq!(
                reconnected.status("lost-wire-ack")[0][0],
                PackValue::String("committed".into())
            );
        });
        assert_eq!(reopened.get("default").unwrap().stats().unwrap().nodes, 2);
    }
}
