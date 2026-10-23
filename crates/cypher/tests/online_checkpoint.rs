use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{execute, ExecError, Out, QueryAccess, QueryTransaction, ReadVisibility};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, path::Path};

const QUERY: &str = "CALL db.checkpoint($name) YIELD name, path, sequence, tracked \
                     RETURN name, path, sequence, tracked";

fn open(path: &Path) -> Database {
    Database::open(path, &Resources::new(StoreConfig::for_tests())).unwrap()
}

fn source(path: &Path) -> Database {
    let db = open(path);
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    db
}

fn params(name: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("name".to_string(), Value::Str(name.into()))])
}

fn count(db: &Database) -> Vec<Vec<Out>> {
    execute(db, "MATCH (n:Customer) RETURN count(*)", &BTreeMap::new())
        .unwrap()
        .rows
}

fn checkpoint(
    db: &Database,
    root: Option<&Path>,
    name: &str,
    access: QueryAccess,
) -> Result<Vec<Vec<Out>>, ExecError> {
    let mut tx = QueryTransaction::begin(db)?;
    if let Some(root) = root {
        tx = tx.with_checkpoint_dir(root.to_path_buf());
    }
    let rows = tx.run_with_access(QUERY, &params(name), access)?.rows;
    tx.commit()?;
    Ok(rows)
}

#[test]
fn admin_checkpoint_captures_committed_data_while_open() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let db = source(&data.path().join("default"));
    execute(&db, "CREATE (:Customer {entity_id: 'a'})", &BTreeMap::new()).unwrap();

    let rows = checkpoint(
        &db,
        Some(backups.path()),
        "nightly-1",
        QueryAccess::UNRESTRICTED,
    )
    .unwrap();
    let path = backups.path().join("nightly-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Out::Value(Value::Str("nightly-1".into())));
    assert_eq!(
        rows[0][1],
        Out::Value(Value::Str(path.display().to_string()))
    );
    assert_eq!(rows[0][3], Out::Value(Value::Bool(true)));

    // Later source writes are not visible in the captured copy.
    execute(&db, "CREATE (:Customer {entity_id: 'b'})", &BTreeMap::new()).unwrap();
    let copy = open(&path);
    assert_eq!(count(&copy), vec![vec![Out::Value(Value::Int(1))]]);
    assert_eq!(count(&db), vec![vec![Out::Value(Value::Int(2))]]);

    // Names never overwrite an existing checkpoint.
    drop(copy);
    assert!(matches!(
        checkpoint(
            &db,
            Some(backups.path()),
            "nightly-1",
            QueryAccess::UNRESTRICTED
        ),
        Err(ExecError::Semantic(message)) if message.contains("already exists")
    ));
}

#[test]
fn checkpoint_is_refused_without_root_admin_or_safe_name() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let db = source(&data.path().join("default"));
    let root = Some(backups.path());

    let err = checkpoint(&db, None, "a", QueryAccess::UNRESTRICTED).unwrap_err();
    assert!(err.to_string().contains("checkpoint_dir"), "{err}");

    let writer = QueryAccess {
        write: true,
        instance_admin: false,
    };
    assert!(matches!(
        checkpoint(&db, root, "a", writer),
        Err(ExecError::Forbidden(_))
    ));

    for bad in ["", ".hidden", "../escape", "a/b", "a\\b", &"x".repeat(129)] {
        assert!(
            checkpoint(&db, root, bad, QueryAccess::UNRESTRICTED).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(std::fs::read_dir(backups.path()).unwrap().count(), 0);
}

#[test]
fn checkpoint_must_stand_alone_without_staged_writes_or_restricted_reads() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let db = source(&data.path().join("default"));

    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_checkpoint_dir(backups.path().to_path_buf());
    tx.run("CREATE (:Customer {entity_id: 'a'})", &BTreeMap::new())
        .unwrap();
    assert!(tx.run(QUERY, &params("staged")).is_err());

    let mut tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_checkpoint_dir(backups.path().to_path_buf());
    let err = tx
        .run(
            "UNWIND [1, 2] AS x CALL db.checkpoint('twice') YIELD name RETURN name",
            &BTreeMap::new(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("first clause"), "{err}");

    let tx = QueryTransaction::begin(&db)
        .unwrap()
        .with_checkpoint_dir(backups.path().to_path_buf());
    let mut tx = tx.with_read_visibility(ReadVisibility::default()).unwrap();
    assert!(tx.run(QUERY, &params("restricted")).is_err());

    assert_eq!(std::fs::read_dir(backups.path()).unwrap().count(), 0);
}
