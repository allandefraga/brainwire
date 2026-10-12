//! The Bolt 5.1+ server state machine, independent of the network.

use std::collections::VecDeque;

use crate::auth::AccessContext;
pub use crate::auth::AuthMode;
use crate::message::{Request, Response};
use crate::packstream::PackValue;

/// The `server` value of the HELLO answer. Driver 5.x refuses a server whose
/// agent does not start with `Neo4j/`; the rest says what Brainwire is, so it
/// never passes for Neo4j itself (Memgraph uses the same form).
const SERVER_AGENT: &str = "Neo4j/5.26.0 compatible graph database server - Brainwire";

pub struct QueryResult {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<PackValue>>,
}

#[derive(Debug)]
pub struct EngineError {
    pub code: String,
    pub message: String,
}

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
}

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

    fn begin(&mut self, extra: &PackValue) -> Result<(), Response> {
        self.db = Self::database(extra)?;
        // A failed begin may still own partially initialized engine state.
        self.pending = true;
        self.engine
            .begin(self.db.as_deref())
            .map_err(Self::engine_failure)
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
        let n = match extra.get("n") {
            None | Some(PackValue::Int(-1)) => usize::MAX,
            Some(PackValue::Int(n)) if *n >= 0 => usize::try_from(*n).unwrap_or(usize::MAX),
            _ => return self.fail(invalid("n must be -1 or a nonnegative integer")),
        };
        let mut out = Vec::new();
        for _ in 0..n.min(self.rows.len()) {
            let row = self.rows.pop_front().expect("remaining row");
            if !discard {
                out.push(Response::Record(row));
            }
        }
        let in_tx = self.state == State::TxStreaming;
        if self.rows.is_empty() {
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

    pub fn handle(&mut self, req: Request) -> Vec<Response> {
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
                if let Err(response) = self.begin(&extra) {
                    return self.fail(response);
                }
                self.start_query(&query, &params, false)
            }
            (Ready, Request::Begin { extra }) => {
                if let Err(response) = self.begin(&extra) {
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
                        return self.fail(invalid("RUN cannot change the transaction database"))
                    }
                    _ => {}
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
