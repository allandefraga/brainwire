use std::process::{Command, Output};
use tempfile::TempDir;
fn command(dir: &TempDir) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    c.current_dir(dir.path());
    for (k, _) in std::env::vars().filter(|(k, _)| k.starts_with("BRAINWIRE_")) {
        c.env_remove(k);
    }
    c.env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB");
    c
}
fn query(dir: &TempDir, q: &str, extra: &[&str]) -> Output {
    command(dir)
        .args(["cypher", "--database", "db", "--query", q])
        .args(extra)
        .output()
        .unwrap()
}
fn json(out: Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[test]
fn initialize_create_parameter_reopen_and_atomic_failure() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    let created = json(query(
        &d,
        "CREATE (c:Customer {entity_id: $id, name: $name}) RETURN c",
        &[
            "--ontology",
            "schema.json",
            "--params",
            r#"{"id":"synthetic-1","name":"Ana"}"#,
        ],
    ));
    assert_eq!(created["fields"], serde_json::json!(["c"]));
    assert_eq!(created["rows"][0][0]["type"], "node");
    assert_eq!(
        created["rows"][0][0]["properties"]["entity_id"],
        "synthetic-1"
    );
    let failure = query(
        &d,
        "UNWIND ['new', 'synthetic-1'] AS id CREATE (c:Customer {entity_id: id})",
        &[],
    );
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("constraint"));
    assert_eq!(
        json(query(&d, "MATCH (c:Customer) RETURN count(*)", &[]))["rows"],
        serde_json::json!([[1]])
    );
    assert_eq!(
        json(query(
            &d,
            "RETURN $x, $y",
            &[
                "--params",
                r#"{"x":-9223372036854775808,"y":[true,null,1.5,{"a":"b"}]}"#
            ]
        ))["rows"],
        serde_json::json!([[-9223372036854775808_i64,[true,null,1.5,{"a":"b"}]]])
    );
}

#[test]
fn analytics_through_cli_returns_real_nodes_scores_and_projection_counts() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    json(query(&d, "CREATE (c:Customer {entity_id:'c'}), (k:Company {entity_id:'k'}) CREATE (c)-[:WORKS_AT]->(k)", &["--ontology", "schema.json"]));
    let summary = json(query(&d, "CALL graph.project($opts) YIELD nodeCount, relationshipCount RETURN nodeCount, relationshipCount", &["--params", r#"{"opts":{"relationship":"WORKS_AT"}}"#]));
    assert_eq!(summary["rows"], serde_json::json!([[2, 1]]));
    let rank = json(query(
        &d,
        "CALL graph.pagerank() YIELD node, score RETURN node, score LIMIT 1",
        &[],
    ));
    assert_eq!(rank["rows"][0][0]["properties"]["entity_id"], "k");
    assert!(rank["rows"][0][1].as_f64().unwrap() > 0.5);
    let failure = query(
        &d,
        "CALL graph.project({maxNodes:1}) YIELD nodeCount RETURN nodeCount",
        &[],
    );
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("maxNodes"));
}
#[test]
fn invalid_inputs_do_not_create_database() {
    for params in [
        "[]",
        r#"{"x":9223372036854775808}"#,
        r#"{"x":-9223372036854775809}"#,
        r#"{"x":184467440737095516160}"#,
    ] {
        let d = tempfile::tempdir().unwrap();
        let out = query(&d, "RETURN $x", &["--params", params]);
        assert!(!out.status.success());
        assert!(!d.path().join("db").exists());
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("bad.json"), "{").unwrap();
    assert!(!query(&d, "RETURN 1", &["--ontology", "bad.json"])
        .status
        .success());
    assert!(!d.path().join("db").exists());
}

#[test]
fn parameter_file_imports_nested_rows_and_reopens() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    std::fs::write(
        d.path().join("params.json"),
        r#"{"rows":[{"id":"file-1","name":"Ana"},{"id":"file-2","name":"Béa"}],"finite":1.5}"#,
    )
    .unwrap();
    assert_eq!(
        json(query(
            &d,
            "UNWIND $rows AS row CREATE (c:Customer {entity_id: row.id, name: row.name}) RETURN c.name, $finite",
            &["--ontology", "schema.json", "--params-file", "params.json"],
        ))["rows"],
        serde_json::json!([["Ana", 1.5], ["Béa", 1.5]])
    );
    assert_eq!(
        json(query(&d, "MATCH (c:Customer) RETURN count(*)", &[]))["rows"],
        serde_json::json!([[2]])
    );
    assert_eq!(
        json(query(
            &d,
            "MATCH (c:Customer {entity_id: 'file-2'}) RETURN c.name",
            &[],
        ))["rows"],
        serde_json::json!([["Béa"]])
    );
}

#[test]
fn invalid_parameter_files_fail_before_opening_store() {
    for content in [
        &b"{"[..],
        &b"[]"[..],
        &b"\xff"[..],
        &br#"{"x":9223372036854775808}"#[..],
        &br#"{"x":-9223372036854775809}"#[..],
        &br#"{"x":184467440737095516160}"#[..],
        &br#"{"x":1e999}"#[..],
    ] {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("params.json"), content).unwrap();
        let out = query(&d, "RETURN $x", &["--params-file", "params.json"]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("parameters:"));
        assert!(out.stdout.is_empty());
        assert!(!d.path().join("db").exists());
    }
    for path in ["missing.json", "."] {
        let d = tempfile::tempdir().unwrap();
        let out = query(&d, "RETURN 1", &["--params-file", path]);
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(error.contains("parameters:"));
        assert!(error.contains("cannot read"));
        assert!(error.contains(path));
        assert!(!d.path().join("db").exists());
    }
}

#[test]
fn parameter_file_conflicts_with_explicit_parameters() {
    let d = tempfile::tempdir().unwrap();
    let out = query(
        &d,
        "RETURN 1",
        &["--params", "{}", "--params-file", "missing.json"],
    );
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("cannot be used with"));
    assert!(error.contains("--params-file"));
    assert!(error.contains("--params"));
    assert!(!d.path().join("db").exists());
}
#[test]
fn config_and_shell_routes_survive() {
    let d = tempfile::tempdir().unwrap();
    assert!(command(&d)
        .args(["config", "show"])
        .output()
        .unwrap()
        .status
        .success());
    let shell = command(&d)
        .args([
            "shell",
            "--demo",
            "--snapshot",
            "--lang",
            "en",
            "--width",
            "80",
            "--height",
            "24",
        ])
        .output()
        .unwrap();
    assert!(shell.status.success());
    assert!(String::from_utf8_lossy(&shell.stdout).contains("SYNTHETIC"));
    assert!(!command(&d)
        .args(["shell", "--demo"])
        .output()
        .unwrap()
        .status
        .success());
    std::fs::write(
        d.path().join("small.toml"),
        "[memory]\ncache='128MiB'\nmemtables='32MiB'\n",
    )
    .unwrap();
    assert_eq!(
        json(query(&d, "RETURN 1", &["--config", "small.toml"]))["rows"],
        serde_json::json!([[1]])
    );
    assert!(!command(&d)
        .env("BRAINWIRE_MEMORY_CACHE", "bad")
        .args(["cypher", "--database", "other", "--query", "RETURN 1"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!d.path().join("other").exists());
}

#[test]
fn exclusive_lock_and_nonfinite_stored_output_are_actionable() {
    use brainwire_core::{schema::GraphMapping, value::Value};
    use brainwire_store::db::{Database, Resources, StoreConfig};
    use std::collections::BTreeMap;
    let d = tempfile::tempdir().unwrap();
    let db = Database::open(
        &d.path().join("db"),
        &Resources::new(StoreConfig::for_tests()),
    )
    .unwrap();
    let mapping: GraphMapping =
        serde_json::from_str(include_str!("../../core/tests/fixtures/graph_mapping.json")).unwrap();
    db.apply_ontology(&mapping).unwrap();
    let locked = query(&d, "RETURN 1", &[]);
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("exclusive local access"));
    brainwire_cypher::execute(
        &db,
        "CREATE (c:Customer {entity_id: 'nonfinite', score: $score})",
        &BTreeMap::from([("score".into(), Value::Float(f64::INFINITY))]),
    )
    .unwrap();
    drop(db);
    for q in [
        "MATCH (c:Customer) RETURN c.score",
        "MATCH (c:Customer) RETURN c",
    ] {
        let output = query(&d, q, &[]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("output: nonfinite stored float"));
    }
}
