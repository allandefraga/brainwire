use std::collections::BTreeMap;

use brainwire_core::value::Value;
use brainwire_cypher::{
    execute, execute_with_limits, parse, ExecError, ExecutionLimits, Out, QueryAccess,
    QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let resources = Resources::new(StoreConfig::for_tests());
    let mut db = Database::open(dir.path(), &resources).unwrap();
    db.enable_relationship_index_format(&resources).unwrap();
    for query in [
        "CALL db.createLabel('Item')",
        "CALL db.createRelationshipType('EVENT', 'Item', 'Item')",
        "CREATE INDEX item_name FOR (n:Item) ON (n.name)",
        "CREATE INDEX event_at FOR ()-[r:EVENT]-() ON (r.at)",
    ] {
        execute(&db, query, &BTreeMap::new()).unwrap();
    }
    (dir, db)
}

#[test]
fn standalone_show_indexes_is_the_existing_procedure_ast() {
    let expected = parse("CALL db.indexes()").unwrap();
    for query in ["SHOW INDEXES", "show indexes;", "sHoW InDeXeS // inventory"] {
        assert_eq!(parse(query).unwrap(), expected, "{query}");
    }
}

#[test]
fn unsupported_show_variants_fail_instead_of_silently_changing_semantics() {
    for query in [
        "SHOW INDEX",
        "SHOW RANGE INDEXES",
        "SHOW NODE INDEXES",
        "SHOW INDEXES YIELD name",
        "SHOW INDEXES WHERE entityType = 'node'",
        "SHOW INDEXES RETURN name",
        "SHOW INDEXES()",
        "SHOW INDEXES UNION CALL db.indexes()",
        "RETURN 1 UNION SHOW INDEXES",
        "MATCH (n) SHOW INDEXES",
        "SHOW INDEXES; RETURN 1",
    ] {
        let error = parse(query).expect_err(query);
        assert!(error.offset <= query.len(), "{query}: {error}");
    }
}

#[test]
fn show_indexes_returns_exact_existing_columns_rows_and_population_states() {
    let (_dir, db) = setup();
    for ready in [false, true] {
        if ready {
            db.build_pending_indexes().unwrap();
        }
        let expected = execute(&db, "CALL db.indexes()", &BTreeMap::new()).unwrap();
        let actual = execute(&db, "SHOW INDEXES", &BTreeMap::new()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            actual.fields,
            ["label", "property", "state", "name", "entityType"]
        );
        for (name, entity, state) in [
            (
                "item_name",
                "node",
                if ready {
                    "scalar_equality_ready"
                } else {
                    "populating"
                },
            ),
            (
                "event_at",
                "relationship",
                if ready {
                    "scalar_range_ready"
                } else {
                    "populating"
                },
            ),
        ] {
            assert!(
                actual.rows.iter().any(|row| {
                    row[3] == Out::Value(Value::Str(name.into()))
                        && row[4] == Out::Value(Value::Str(entity.into()))
                        && row[2] == Out::Value(Value::Str(state.into()))
                }),
                "missing {name} state {state}: {:?}",
                actual.rows
            );
        }
    }
}

#[test]
fn show_indexes_preserves_reader_access_and_memory_error_contract() {
    let (_dir, db) = setup();
    let read = QueryAccess {
        write: false,
        instance_admin: false,
    };
    let mut transaction = QueryTransaction::begin(&db).unwrap();
    let expected = transaction
        .run_with_access("CALL db.indexes()", &BTreeMap::new(), read)
        .unwrap();
    let actual = transaction
        .run_with_access("SHOW INDEXES", &BTreeMap::new(), read)
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(transaction.commit().unwrap(), None);

    let limits = ExecutionLimits {
        memory_bytes: 1,
        ..Default::default()
    };
    let expected =
        execute_with_limits(&db, "CALL db.indexes()", &BTreeMap::new(), limits).unwrap_err();
    let actual = execute_with_limits(&db, "SHOW INDEXES", &BTreeMap::new(), limits).unwrap_err();
    match (actual, expected) {
        (
            ExecError::MemoryLimit { limit: actual, .. },
            ExecError::MemoryLimit {
                limit: expected, ..
            },
        ) => assert_eq!(actual, expected),
        (actual, expected) => panic!("different error contracts: {actual:?}, {expected:?}"),
    }
}
