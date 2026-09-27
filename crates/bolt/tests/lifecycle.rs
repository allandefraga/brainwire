use brainwire_bolt::{
    message::{Request, Response},
    packstream::PackValue,
    session::{AuthMode, EngineError, QueryEngine, QueryResult, Session, State},
};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Trace {
    events: Vec<String>,
    fail: Option<&'static str>,
}
struct Engine(Arc<Mutex<Trace>>);
impl Engine {
    fn event(&self, name: &str) -> Result<(), EngineError> {
        let mut t = self.0.lock().unwrap();
        t.events.push(name.into());
        if t.fail == Some(name) {
            Err(EngineError {
                code: "test.error".into(),
                message: name.into(),
            })
        } else {
            Ok(())
        }
    }
}
impl QueryEngine for Engine {
    fn begin(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        self.event(&format!("db:{db:?}"))?;
        self.event("begin")
    }
    fn run(
        &mut self,
        db: Option<&str>,
        _: &str,
        _: &PackValue,
    ) -> Result<QueryResult, EngineError> {
        self.event(&format!("run-db:{db:?}"))?;
        self.event("run")?;
        Ok(QueryResult {
            fields: vec!["x".into()],
            rows: vec![vec![PackValue::Int(1)]; 3],
        })
    }
    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        self.event("commit")?;
        Ok(Some("engine-bookmark".into()))
    }
    fn abort(&mut self) -> Result<(), EngineError> {
        self.event("abort")
    }
}
fn map() -> PackValue {
    PackValue::Map(vec![])
}
fn extra(k: &str, v: PackValue) -> PackValue {
    PackValue::Map(vec![(k.into(), v)])
}
fn ready() -> (Session<Engine>, Arc<Mutex<Trace>>) {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let mut s = Session::new(Engine(trace.clone()), AuthMode::AllowAll);
    s.handle(Request::Hello { extra: map() });
    s.handle(Request::Logon { auth: map() });
    (s, trace)
}
fn run() -> Request {
    Request::Run {
        query: "query".into(),
        params: map(),
        extra: map(),
    }
}
fn pull(n: i64) -> Request {
    Request::Pull {
        extra: extra("n", PackValue::Int(n)),
    }
}
fn begin() -> Request {
    Request::Begin { extra: map() }
}
fn events(t: &Arc<Mutex<Trace>>) -> Vec<String> {
    t.lock().unwrap().events.clone()
}
fn failed(r: &[Response]) -> bool {
    matches!(r.last(), Some(Response::Failure { .. }))
}
#[test]
fn implicit_partial_pull_discard_then_commit_with_engine_bookmark() {
    let (mut s, t) = ready();
    s.handle(run());
    s.handle(pull(1));
    let r = s.handle(Request::Discard {
        extra: extra("n", PackValue::Int(1)),
    });
    assert!(
        matches!(&r[0],Response::Success(m) if m.contains(&("has_more".into(),PackValue::Bool(true))))
    );
    assert!(!events(&t).contains(&"commit".into()));
    let r = s.handle(pull(-1));
    assert!(
        matches!(r.last(),Some(Response::Success(m)) if m.contains(&("bookmark".into(),PackValue::String("engine-bookmark".into()))))
    );
    drop(s);
    assert_eq!(
        events(&t),
        ["db:None", "begin", "run-db:None", "run", "commit"]
    );
}
#[test]
fn explicit_multiple_queries_commit_once() {
    let (mut s, t) = ready();
    s.handle(begin());
    for _ in 0..2 {
        s.handle(run());
        s.handle(pull(-1));
    }
    assert_eq!(s.state(), State::TxReady);
    assert!(!events(&t).contains(&"commit".into()));
    s.handle(Request::Commit);
    drop(s);
    assert_eq!(events(&t).last().unwrap(), "commit");
}
#[test]
fn errors_abort_and_ignore_until_reset() {
    for op in ["begin", "run", "commit"] {
        let (mut s, t) = ready();
        t.lock().unwrap().fail = Some(op);
        let r = if op == "begin" {
            s.handle(begin())
        } else {
            let result = s.handle(run());
            if op == "run" {
                result
            } else {
                s.handle(pull(-1))
            }
        };
        assert!(
            matches!(r.last(), Some(Response::Failure {code, message}) if code == "test.error" && message == op)
        );
        assert!(!r.iter().any(|r| matches!(r, Response::Success(_))));
        assert_eq!(s.state(), State::Failed);
        assert_eq!(events(&t).last().unwrap(), "abort");
        assert_eq!(s.handle(run()), vec![Response::Ignored]);
        s.handle(Request::Reset);
        assert_eq!(s.state(), State::Ready);
        drop(s);
        assert_eq!(events(&t).iter().filter(|e| *e == "abort").count(), 1);
    }
}
#[test]
fn cleanup_for_rollback_reset_goodbye_protocol_failure_and_drop() {
    for req in [
        Some(Request::Rollback),
        Some(Request::Reset),
        Some(Request::Goodbye),
        Some(Request::Commit),
        None,
    ] {
        let (mut s, t) = ready();
        s.handle(begin());
        if matches!(req, Some(Request::Commit)) {
            s.handle(run());
        }
        if let Some(req) = req {
            s.handle(req);
        }
        drop(s);
        assert_eq!(events(&t).iter().filter(|e| *e == "abort").count(), 1);
    }
}
#[test]
fn cleanup_error_is_defunct_and_drop_retries() {
    let (mut s, t) = ready();
    s.handle(begin());
    t.lock().unwrap().fail = Some("abort");
    assert!(failed(&s.handle(Request::Reset)));
    assert_eq!(s.state(), State::Defunct);
    drop(s);
    assert_eq!(events(&t).iter().filter(|e| *e == "abort").count(), 2);
}
#[test]
fn database_is_bound_and_invalid_metadata_aborts() {
    let (mut s, t) = ready();
    s.handle(Request::Begin {
        extra: extra("db", PackValue::String("graph".into())),
    });
    s.handle(run());
    s.handle(pull(-1));
    assert!(events(&t).contains(&"run-db:Some(\"graph\")".into()));
    assert!(failed(&s.handle(Request::Run {
        query: "x".into(),
        params: map(),
        extra: extra("db", PackValue::String("other".into()))
    })));
    assert_eq!(events(&t).last().unwrap(), "abort");
    let (mut s, t) = ready();
    assert!(failed(&s.handle(Request::Begin {
        extra: extra("db", PackValue::Int(2))
    })));
    assert!(events(&t).is_empty());
}
#[test]
fn invalid_counts_fail_before_consumption() {
    for n in [PackValue::Int(-2), PackValue::String("all".into())] {
        let (mut s, t) = ready();
        s.handle(run());
        assert!(failed(&s.handle(Request::Discard {
            extra: extra("n", n)
        })));
        assert_eq!(events(&t).last().unwrap(), "abort");
    }
}

#[test]
fn final_discard_commits_but_zero_discard_keeps_rows() {
    let (mut s, t) = ready();
    s.handle(run());
    s.handle(Request::Discard {
        extra: extra("n", PackValue::Int(0)),
    });
    assert_eq!(s.state(), State::Streaming);
    let result = s.handle(Request::Discard {
        extra: extra("n", PackValue::Int(-1)),
    });
    assert_eq!(result.len(), 1);
    assert!(matches!(&result[0], Response::Success(_)));
    assert_eq!(events(&t).last().unwrap(), "commit");
}
#[test]
fn explicit_commit_failure_is_failure_and_aborts() {
    let (mut s, t) = ready();
    s.handle(begin());
    s.handle(run());
    s.handle(pull(-1));
    t.lock().unwrap().fail = Some("commit");
    assert!(failed(&s.handle(Request::Commit)));
    assert_eq!(s.state(), State::Failed);
    assert_eq!(events(&t).last().unwrap(), "abort");
}
#[test]
fn fatal_hello_aborts_and_idle_drop_does_not_abort() {
    let (mut s, t) = ready();
    s.handle(begin());
    s.handle(Request::Hello { extra: map() });
    assert_eq!(s.state(), State::Defunct);
    drop(s);
    assert_eq!(events(&t).iter().filter(|e| *e == "abort").count(), 1);
    let (s, t) = ready();
    drop(s);
    assert!(events(&t).is_empty());
}
#[test]
fn database_metadata_is_validated_on_every_run() {
    for explicit in [false, true] {
        let (mut s, t) = ready();
        if explicit {
            s.handle(begin());
        }
        assert!(failed(&s.handle(Request::Run {
            query: "x".into(),
            params: map(),
            extra: extra("db", PackValue::Int(2))
        })));
        assert_eq!(events(&t).iter().filter(|e| *e == "run").count(), 0);
    }
    let (mut s, t) = ready();
    let db = extra("db", PackValue::String("same".into()));
    s.handle(Request::Begin { extra: db.clone() });
    assert!(!failed(&s.handle(Request::Run {
        query: "x".into(),
        params: map(),
        extra: db
    })));
    s.handle(pull(-1));
    s.handle(Request::Commit);
    s.handle(run());
    assert_eq!(events(&t).iter().filter(|e| *e == "db:None").count(), 1);
}
