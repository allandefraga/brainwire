//! Test-only borrowed adapter: scalar output and empty parameters only. This is
//! not the production PackStream adapter or a worker/server integration.
use brainwire_bolt::{
    message::{Request, Response},
    packstream::PackValue,
    session::{AuthMode, EngineError, QueryEngine, QueryResult, Session, State},
};
use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryTransaction};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::collections::BTreeMap;

struct Adapter<'db> {
    db: &'db Database,
    tx: Option<QueryTransaction<'db>>,
}
fn error(e: ExecError) -> EngineError {
    EngineError {
        code: if matches!(e, ExecError::Conflict) {
            "Neo.TransientError.Transaction.Conflict"
        } else {
            "Neo.ClientError.Statement.ExecutionFailed"
        }
        .into(),
        message: e.to_string(),
    }
}
impl QueryEngine for Adapter<'_> {
    fn begin(&mut self, db: Option<&str>) -> Result<(), EngineError> {
        assert!(db.is_none());
        assert!(self.tx.is_none());
        self.tx = Some(QueryTransaction::begin(self.db).map_err(error)?);
        Ok(())
    }
    fn run(
        &mut self,
        _: Option<&str>,
        query: &str,
        params: &PackValue,
    ) -> Result<QueryResult, EngineError> {
        assert_eq!(*params, map());
        let output = self
            .tx
            .as_mut()
            .unwrap()
            .run(query, &BTreeMap::new())
            .map_err(error)?;
        let rows = output
            .rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| match v {
                        Out::Value(Value::Int(v)) => PackValue::Int(v),
                        Out::Value(Value::Str(v)) => PackValue::String(v),
                        _ => panic!("fixture uses integer/string scalar outputs only"),
                    })
                    .collect()
            })
            .collect();
        Ok(QueryResult {
            fields: output.fields,
            rows,
        })
    }
    fn commit(&mut self) -> Result<Option<String>, EngineError> {
        self.tx.as_mut().unwrap().commit().map_err(error)?;
        self.tx.take();
        // Store sequences are not restart-stable bookmarks.
        Ok(None)
    }
    fn abort(&mut self) -> Result<(), EngineError> {
        if let Some(mut tx) = self.tx.take() {
            tx.rollback();
        }
        Ok(())
    }
}
fn map() -> PackValue {
    PackValue::Map(vec![])
}
fn open(path: &std::path::Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}
fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}
fn session(db: &Database) -> Session<Adapter<'_>> {
    let mut s = Session::new(Adapter { db, tx: None }, AuthMode::AllowAll);
    s.handle(Request::Hello { extra: map() });
    s.handle(Request::Logon { auth: map() });
    s
}
fn begin(s: &mut Session<Adapter<'_>>) {
    assert!(matches!(
        s.handle(Request::Begin { extra: map() })[0],
        Response::Success(_)
    ));
}
fn run(s: &mut Session<Adapter<'_>>, q: &str) -> Vec<Response> {
    s.handle(Request::Run {
        query: q.into(),
        params: map(),
        extra: map(),
    })
}
fn pull(s: &mut Session<Adapter<'_>>) -> Vec<Response> {
    s.handle(Request::Pull { extra: map() })
}
fn create(s: &mut Session<Adapter<'_>>, id: &str) {
    assert!(matches!(
        run(s, &format!("CREATE (c:Customer {{entity_id: '{id}'}})"))[0],
        Response::Success(_)
    ));
    assert!(matches!(pull(s).last(), Some(Response::Success(_))));
}
fn count(db: &Database) -> i64 {
    match &execute(db, "MATCH (c:Customer) RETURN count(*)", &BTreeMap::new())
        .unwrap()
        .rows[0][0]
    {
        Out::Value(Value::Int(n)) => *n,
        _ => panic!("count"),
    }
}
#[test]
fn session_commit_preserves_overlay_visibility_and_reopen() {
    let (dir, db) = setup();
    let mut s = session(&db);
    begin(&mut s);
    create(&mut s, "one");
    create(&mut s, "two");
    assert_eq!(count(&db), 0);
    run(&mut s, "MATCH (c:Customer) RETURN count(*)");
    assert_eq!(pull(&mut s)[0], Response::Record(vec![PackValue::Int(2)]));
    assert!(matches!(s.handle(Request::Commit)[0], Response::Success(_)));
    assert_eq!(count(&db), 2);
    drop(s);
    drop(db);
    assert_eq!(count(&open(dir.path())), 2);
}
#[test]
fn rollback_reset_drop_and_late_query_failure_leave_no_graph_writes() {
    let (dir, db) = setup();
    for mode in 0..4 {
        let mut s = session(&db);
        begin(&mut s);
        create(&mut s, "one");
        create(&mut s, "two");
        match mode {
            0 => {
                s.handle(Request::Rollback);
            }
            1 => {
                s.handle(Request::Reset);
            }
            2 => {}
            _ => {
                assert!(matches!(
                    run(
                        &mut s,
                        "CREATE (c:Customer {entity_id: 'late'}) UNWIND [0] AS x RETURN 1 / x"
                    )[0],
                    Response::Failure { .. }
                ));
                assert_eq!(s.handle(Request::Commit), vec![Response::Ignored]);
            }
        }
        drop(s);
        assert_eq!(count(&db), 0);
    }
    drop(db);
    assert_eq!(count(&open(dir.path())), 0);
}
#[test]
fn implicit_write_commits_only_after_final_pull_and_reset_discards() {
    let (_dir, db) = setup();
    let mut s = session(&db);
    run(&mut s, "CREATE (c:Customer {entity_id: 'one'}) RETURN 1");
    assert_eq!(count(&db), 0);
    pull(&mut s);
    assert_eq!(count(&db), 1);
    run(&mut s, "CREATE (c:Customer {entity_id: 'two'}) RETURN 2");
    s.handle(Request::Reset);
    assert_eq!(count(&db), 1);
}
#[test]
fn concurrent_sessions_surface_commit_conflict_without_success() {
    let (_dir, db) = setup();
    let mut a = session(&db);
    let mut b = session(&db);
    begin(&mut a);
    begin(&mut b);
    create(&mut a, "shared");
    create(&mut b, "shared");
    assert!(matches!(a.handle(Request::Commit)[0], Response::Success(_)));
    assert!(
        matches!(&b.handle(Request::Commit)[0],Response::Failure{code,..} if code=="Neo.TransientError.Transaction.Conflict")
    );
    assert_eq!(b.state(), State::Failed);
    assert_eq!(count(&db), 1);
}

#[test]
fn implicit_empty_result_waits_for_discard_before_persisting() {
    let (_dir, db) = setup();
    let mut s = session(&db);
    run(&mut s, "CREATE (c:Customer {entity_id: 'one'})");
    assert_eq!(count(&db), 0);
    assert!(matches!(
        s.handle(Request::Discard { extra: map() })[0],
        Response::Success(_)
    ));
    assert_eq!(count(&db), 1);
}
