use std::collections::BTreeMap;

use brainwire_core::value::Value;
use brainwire_cypher::{
    execute, execute_with_limits, parse, ExecError, ExecutionLimits, Out, QueryAccess,
    QueryMemoryPool, QueryTransaction,
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
        "SHOW INDEXES YIELD *",
        "SHOW INDEXES YIELD name",
        "SHOW INDEXES YIELD name RETURN name UNION RETURN 'other'",
        "SHOW INDEXES YIELD name RETURN name MATCH (n) RETURN n",
        "SHOW INDEXES YIELD name CREATE (:Item) RETURN name",
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
fn show_indexes_yield_filters_ready_node_and_relationship_properties() {
    let (_dir, db) = setup();
    db.build_pending_indexes().unwrap();
    for (label, property) in [("Item", "name"), ("EVENT", "at")] {
        let result = execute(
            &db,
            "SHOW INDEXES YIELD labelsOrTypes, properties WHERE $lbl IN labelsOrTypes RETURN properties",
            &BTreeMap::from([("lbl".into(), Value::Str(label.into()))]),
        ).unwrap();
        assert_eq!(result.fields, ["properties"]);
        assert_eq!(
            result.rows,
            vec![vec![Out::Value(Value::List(vec![Value::Str(
                property.into()
            )]))]]
        );
    }
    let result = execute(&db,
        "SHOW INDEXES YIELD labelsOrTypes AS kinds, properties AS fields WHERE 'Absent' IN kinds RETURN fields",
        &BTreeMap::new()).unwrap();
    assert!(result.rows.is_empty());
}

#[test]
fn show_indexes_yield_keeps_native_states_and_reader_memory_contracts() {
    let (_dir, db) = setup();
    for ready in [false, true] {
        if ready {
            db.build_pending_indexes().unwrap();
        }
        let mut tx = QueryTransaction::begin(&db).unwrap();
        let result = tx.run_with_access(
            "SHOW INDEXES YIELD name AS indexName, state, entityType WHERE indexName = 'item_name' RETURN indexName, state, entityType",
            &BTreeMap::new(), QueryAccess { write: false, instance_admin: false }).unwrap();
        assert_eq!(
            result.rows,
            vec![vec![
                Out::Value(Value::Str("item_name".into())),
                Out::Value(Value::Str(
                    if ready {
                        "scalar_equality_ready"
                    } else {
                        "populating"
                    }
                    .into()
                )),
                Out::Value(Value::Str("NODE".into())),
            ]]
        );
        let alias = tx.run_with_access(
            "CALL db.showIndexes() YIELD name AS indexName, state, entityType WHERE indexName = 'item_name' RETURN indexName, state, entityType",
            &BTreeMap::new(), QueryAccess { write: false, instance_admin: false }).unwrap();
        assert_eq!(alias, result);
    }
    let error = execute_with_limits(
        &db,
        "SHOW INDEXES YIELD properties RETURN properties",
        &BTreeMap::new(),
        ExecutionLimits {
            memory_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ExecError::MemoryLimit { .. }));
    assert!(execute(
        &db,
        "SHOW INDEXES YIELD unknown RETURN unknown",
        &BTreeMap::new()
    )
    .is_err());
    assert!(execute(
        &db,
        "CALL db.indexes() YIELD labelsOrTypes RETURN labelsOrTypes",
        &BTreeMap::new()
    )
    .is_err());
    for query in [
        "SHOW INDEXES YIELD name AS field, state AS field RETURN field",
        "SHOW INDEXES YIELD labelsOrTypes WHERE $missing IN labelsOrTypes RETURN labelsOrTypes",
    ] {
        assert!(execute(&db, query, &BTreeMap::new()).is_err(), "{query}");
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

#[test]
fn projected_inventory_accounts_for_lists_and_releases_pool_after_refusal() {
    let (_dir, db) = setup();
    let query = "SHOW INDEXES YIELD labelsOrTypes, properties RETURN labelsOrTypes, properties";
    let pool = std::sync::Arc::new(QueryMemoryPool::new(1 << 20));
    let run = || {
        let mut tx = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: 8 << 10,
                ..Default::default()
            },
            std::sync::Arc::clone(&pool),
        )
        .unwrap();
        let output = tx.run(query, &BTreeMap::new());
        drop(tx);
        assert_eq!(pool.usage().reserved_bytes, 0);
        output
    };
    assert_eq!(run().unwrap().rows.len(), 2);
    for n in 0..32 {
        execute(
            &db,
            &format!("CREATE INDEX extra_{n} FOR (n:Item) ON (n.extra_{n})"),
            &BTreeMap::new(),
        )
        .unwrap();
    }
    assert!(matches!(
        run(),
        Err(ExecError::MemoryLimit { limit: 8192, .. })
    ));
}
