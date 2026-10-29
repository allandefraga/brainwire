use brainwire_core::{schema::GraphMapping, value::Value};
use brainwire_cypher::{
    execute, execute_with_limits, ExecError, ExecutionLimits, Out, QueryMemoryPool,
    QueryTransaction,
};
use brainwire_store::db::{Database, Resources, StoreConfig};
use std::{collections::BTreeMap, sync::Arc};

fn setup() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    (dir, db)
}
fn value(value: Value) -> Out {
    Out::Value(value)
}

#[test]
fn full_match_flags_unicode_and_alternatives() {
    let (_dir, db) = setup();
    let output = execute(&db, "RETURN 'Alice' =~ 'Ali.*', 'Alice' =~ 'lic', 'Alice' =~ '(?i)alice', 'éß' =~ '.{2}', 'abc' =~ 'a|bc', 'bc' =~ 'a|bc', 'abc\\n' =~ 'abc', 'a\\nb' =~ '(?s)a.b'", &BTreeMap::new()).unwrap();
    assert_eq!(
        output.rows,
        vec![vec![true, false, true, true, false, true, false, true]
            .into_iter()
            .map(|v| value(Value::Bool(v)))
            .collect::<Vec<_>>()]
    );
}

#[test]
fn null_and_non_string_operands_produce_null() {
    let (_dir, db) = setup();
    let output = execute(
        &db,
        "RETURN null =~ 'x', 'x' =~ null, 12 =~ '.*', 'x' =~ true, [] =~ 'x'",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![value(Value::Null); 5]]);
}

#[test]
fn parameterized_where_and_map_projection_use_the_same_predicate() {
    let (_dir, db) = setup();
    execute(&db, "CREATE (:Customer {entity_id:'a', email:'ada@example.test'}), (:Customer {entity_id:'b', email:'other@invalid.test'})", &BTreeMap::new()).unwrap();
    let params = BTreeMap::from([("pattern".into(), Value::Str(".*@example[.]test".into()))]);
    let output = execute(&db, "MATCH (n:Customer) WHERE n.email =~ $pattern RETURN n.entity_id AS id, {matched:n.email =~ $pattern} AS flags", &params).unwrap();
    assert_eq!(output.rows.len(), 1);
    assert_eq!(output.rows[0][0], value(Value::Str("a".into())));
}

#[test]
fn invalid_or_unsupported_patterns_fail_atomically() {
    let (_dir, db) = setup();
    for pattern in ["[", "(?=x)x", r"(x)\1", "a)|(?:b"] {
        let params = BTreeMap::from([("pattern".into(), Value::Str(pattern.into()))]);
        assert!(matches!(
            execute(
                &db,
                "CREATE (:Customer {entity_id:'rollback'}) RETURN 'ax' =~ $pattern",
                &params
            ),
            Err(ExecError::Semantic(_))
        ));
        assert_eq!(
            execute(&db, "MATCH (n:Customer) RETURN count(n)", &BTreeMap::new())
                .unwrap()
                .rows,
            vec![vec![value(Value::Int(0))]]
        );
    }
}

#[test]
fn pattern_length_and_compiled_program_are_bounded() {
    let (_dir, db) = setup();
    for pattern in ["a".repeat(4097), "a{1000000}".into(), r"\w{1000000}".into()] {
        let params = BTreeMap::from([("pattern".into(), Value::Str(pattern))]);
        assert!(matches!(
            execute(&db, "RETURN 'a' =~ $pattern", &params),
            Err(ExecError::Semantic(_))
        ));
    }
}

#[test]
fn regex_temporary_memory_is_admitted_and_shared_pool_is_released() {
    let (_dir, db) = setup();
    let limits = ExecutionLimits {
        memory_bytes: 64 * 1024,
        timeout: None,
    };
    assert!(matches!(
        execute_with_limits(&db, "RETURN 'abc' =~ 'a.*'", &BTreeMap::new(), limits),
        Err(ExecError::MemoryLimit { .. })
    ));
    let pool = Arc::new(QueryMemoryPool::new(64 * 1024));
    {
        let mut tx =
            QueryTransaction::begin_with_pool(&db, ExecutionLimits::default(), Arc::clone(&pool))
                .unwrap();
        assert!(matches!(
            tx.run("RETURN 'abc' =~ 'a.*'", &BTreeMap::new()),
            Err(ExecError::MemoryPoolExhausted { .. })
        ));
    }
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert_eq!(
        execute(&db, "RETURN 1", &BTreeMap::new()).unwrap().rows,
        vec![vec![value(Value::Int(1))]]
    );
}

#[test]
fn repeated_rows_release_temporary_admission_and_timeout_is_checked() {
    let (_dir, db) = setup();
    let output = execute_with_limits(
        &db,
        "UNWIND range(1,50) AS n WITH n WHERE 'abc' =~ 'a.*' RETURN count(*)",
        &BTreeMap::new(),
        ExecutionLimits {
            memory_bytes: 2 << 20,
            timeout: None,
        },
    )
    .unwrap();
    assert_eq!(output.rows, vec![vec![value(Value::Int(50))]]);
    assert!(matches!(
        execute_with_limits(
            &db,
            "RETURN 'abc' =~ 'a.*'",
            &BTreeMap::new(),
            ExecutionLimits {
                memory_bytes: 2 << 20,
                timeout: Some(std::time::Duration::ZERO)
            },
        ),
        Err(ExecError::TimedOut)
    ));
}

#[test]
fn extended_mode_comments_cannot_change_full_string_anchoring() {
    let (_dir, db) = setup();
    for (text, pattern, expected) in [
        ("a", "(?x)a # trailing comment", true),
        ("ab", "(?x)a # trailing comment", false),
        ("a#", r"(?x)a\#", true),
        ("a b#", r"(?x)a\ b\#", true),
        ("a b", "(?x)a(?-x: )b", true),
        ("ab", "(?x)a(?-x: )b", false),
        ("a", "(?x)(a) # trailing comment", true),
        ("ba", "(?m)^a$", false),
    ] {
        let params = BTreeMap::from([
            ("text".into(), Value::Str(text.into())),
            ("pattern".into(), Value::Str(pattern.into())),
        ]);
        assert_eq!(
            execute(&db, "RETURN $text =~ $pattern", &params)
                .unwrap()
                .rows,
            vec![vec![value(Value::Bool(expected))]],
            "{pattern}"
        );
    }
}

#[test]
fn unicode_classes_have_explicit_program_limits() {
    let (_dir, db) = setup();
    for pattern in [r"\w+", r"\p{L}+"] {
        let params = BTreeMap::from([("pattern".into(), Value::Str(pattern.into()))]);
        let result = execute(&db, "RETURN 'é' =~ $pattern", &params);
        assert_eq!(
            result.unwrap().rows,
            vec![vec![value(Value::Bool(true))]],
            "{pattern}"
        );
    }
    let params = BTreeMap::from([("pattern".into(), Value::Str(r"(?-u:\w+)".into()))]);
    assert_eq!(
        execute(&db, "RETURN 'abc123' =~ $pattern", &params)
            .unwrap()
            .rows,
        vec![vec![value(Value::Bool(true))]]
    );
}

#[test]
#[ignore = "opt-in compilation-per-row diagnostic; run without other builds/tests"]
fn regex_recompile_diagnostic() {
    let (_dir, db) = setup();
    for rows in [1000, 10000] {
        for (name, predicate) in [
            ("regex", "'example@example.test' =~ 'example@.*'"),
            (
                "starts_with",
                "'example@example.test' STARTS WITH 'example@'",
            ),
        ] {
            let query =
                format!("UNWIND range(1,{rows}) AS n WITH n WHERE {predicate} RETURN count(*)");
            let mut times = Vec::new();
            for _ in 0..3 {
                let start = std::time::Instant::now();
                let output = execute(&db, &query, &BTreeMap::new()).unwrap();
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(output.rows, vec![vec![value(Value::Int(rows))]]);
            }
            times.sort_by(f64::total_cmp);
            eprintln!(
                "rows={rows} operator={name} median_ms={:.3} samples_ms={times:?}",
                times[1]
            );
        }
    }
}

#[test]
fn changing_pattern_drops_cache_before_temporary_compilation() {
    let (_dir, db) = setup();
    let limit = (1 << 20) + 16 * 1024;
    let pool = Arc::new(QueryMemoryPool::new(limit));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: limit,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    let output = tx
        .run(
            "UNWIND ['a','b','b'] AS pattern RETURN 'a' =~ pattern",
            &BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(
        output.rows,
        vec![
            vec![value(Value::Bool(true))],
            vec![value(Value::Bool(false))],
            vec![value(Value::Bool(false))]
        ]
    );
    tx.commit().unwrap();
    assert_eq!(pool.usage().reserved_bytes, 0);
}

#[test]
fn invalid_pattern_after_cache_rolls_back_and_releases_shared_pool() {
    let (_dir, db) = setup();
    let pool = Arc::new(QueryMemoryPool::new(2 << 20));
    let mut tx = QueryTransaction::begin_with_pool(
        &db,
        ExecutionLimits {
            memory_bytes: 2 << 20,
            timeout: None,
        },
        Arc::clone(&pool),
    )
    .unwrap();
    assert!(matches!(tx.run("UNWIND ['a','['] AS pattern CREATE (:Customer {entity_id:pattern}) WITH pattern RETURN 'a' =~ pattern", &BTreeMap::new()), Err(ExecError::Semantic(_))));
    assert!(tx.commit().is_err());
    assert_eq!(pool.usage().reserved_bytes, 0);
    assert_eq!(
        execute(&db, "MATCH (n:Customer) RETURN count(n)", &BTreeMap::new())
            .unwrap()
            .rows,
        vec![vec![value(Value::Int(0))]]
    );
}

#[test]
fn cached_regex_does_not_survive_into_another_run() {
    let (_dir, db) = setup();
    let mut tx = QueryTransaction::begin(&db).unwrap();
    let a = BTreeMap::from([("pattern".into(), Value::Str("a".into()))]);
    let b = BTreeMap::from([("pattern".into(), Value::Str("b".into()))]);
    assert_eq!(
        tx.run("RETURN 'a' =~ $pattern", &a).unwrap().rows,
        vec![vec![value(Value::Bool(true))]]
    );
    assert_eq!(
        tx.run("RETURN 'a' =~ $pattern", &b).unwrap().rows,
        vec![vec![value(Value::Bool(false))]]
    );
    tx.rollback();
}
