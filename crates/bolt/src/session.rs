//! The Bolt 5.1+ server state machine, independent of the network.

use std::collections::VecDeque;

use crate::message::{Request, Response};
use crate::packstream::PackValue;

pub struct QueryResult {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<PackValue>>,
}

pub struct EngineError {
    pub code: String,
    pub message: String,
}

pub trait QueryEngine {
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

pub enum AuthMode {
    /// Development only; real authentication arrives with the security plan.
    AllowAll,
}

pub struct Session<E: QueryEngine> {
    engine: E,
    state: State,
    rows: VecDeque<Vec<PackValue>>,
    db: Option<String>,
    commits: u64,
    _auth: AuthMode,
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
            commits: 0,
            _auth: auth,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn fail(&mut self, response: Response) -> Vec<Response> {
        self.state = State::Failed;
        self.rows.clear();
        vec![response]
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
            Some(PackValue::Int(n)) if *n >= 0 && !discard => *n as usize,
            _ if discard => 0,
            _ => usize::MAX,
        };
        let mut out = Vec::new();
        if discard {
            self.rows.clear();
        }
        while out.len() < n {
            match self.rows.pop_front() {
                Some(row) => out.push(Response::Record(row)),
                None => break,
            }
        }
        let in_tx = self.state == State::TxStreaming;
        if self.rows.is_empty() {
            self.state = if in_tx { State::TxReady } else { State::Ready };
            out.push(meta(vec![
                ("type", PackValue::String("r".into())),
                ("t_last", PackValue::Int(0)),
            ]));
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
                self.state = Defunct;
                vec![]
            }
            (_, Request::Reset) => {
                self.rows.clear();
                self.state = Ready;
                vec![meta(vec![])]
            }
            (Failed, _) => vec![Response::Ignored],
            (_, Request::Route) => self.fail(invalid(
                "routing arrives with the cluster plan; use bolt://",
            )),
            (Connected, Request::Hello { .. }) => {
                self.state = Authentication;
                vec![meta(vec![
                    ("server", PackValue::String("Neo4j/5.26.0".into())),
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
            (Authentication, Request::Logon { .. }) => {
                self.state = Ready;
                vec![meta(vec![])]
            }
            (Ready | TxReady, Request::Telemetry) => vec![meta(vec![])],
            (
                Ready,
                Request::Run {
                    query,
                    params,
                    extra,
                },
            ) => {
                self.db = match extra.get("db") {
                    Some(PackValue::String(db)) => Some(db.clone()),
                    _ => None,
                };
                self.start_query(&query, &params, false)
            }
            (Ready, Request::Begin { extra }) => {
                self.db = match extra.get("db") {
                    Some(PackValue::String(db)) => Some(db.clone()),
                    _ => None,
                };
                self.state = TxReady;
                vec![meta(vec![])]
            }
            (TxReady, Request::Run { query, params, .. }) => {
                self.start_query(&query, &params, true)
            }
            (Streaming | TxStreaming, Request::Pull { extra }) => self.pull(&extra, false),
            (Streaming | TxStreaming, Request::Discard { extra }) => self.pull(&extra, true),
            (TxReady, Request::Commit) => {
                self.commits += 1;
                self.state = Ready;
                let db = self.db.clone().unwrap_or_else(|| "default".into());
                vec![meta(vec![(
                    "bookmark",
                    PackValue::String(format!("bw:{db}:{}", self.commits)),
                )])]
            }
            (TxReady, Request::Rollback) => {
                self.state = Ready;
                vec![meta(vec![])]
            }
            (_, _) => self.fail(invalid("message not allowed in the current state")),
        }
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
                assert!(
                    matches!(server, Some(PackValue::String(ref v)) if v.starts_with("Neo4j/"))
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
    fn explicit_transaction_returns_a_bookmark() {
        let mut s = ready(StubEngine);
        s.handle(Request::Begin { extra: map(&[]) });
        assert_eq!(s.state(), State::TxReady);
        s.handle(run("RETURN 2"));
        assert_eq!(s.state(), State::TxStreaming);
        s.handle(pull(-1));
        assert_eq!(s.state(), State::TxReady);
        let r = s.handle(Request::Commit);
        assert!(
            matches!(&r[0], Response::Success(meta) if meta.iter().any(|(k, _)| k == "bookmark"))
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
}
