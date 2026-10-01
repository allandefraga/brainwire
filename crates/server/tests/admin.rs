use std::process::Command;

fn command(dir: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    command.current_dir(dir.path());
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("BRAINWIRE_")) {
        command.env_remove(key);
    }
    command
        .env("BRAINWIRE_SERVER_DATA_DIR", dir.path().join("data"))
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB");
    command
}

#[test]
fn create_list_and_apply_ontology_survive_process_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let ontology = dir.path().join("schema.json");
    std::fs::write(
        &ontology,
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    let created = command(&dir)
        .args(["db", "create", "research", "--ontology", "schema.json"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{:?}", created);
    assert!(dir.path().join("data/research/CURRENT").is_file());
    let listed = command(&dir).args(["db", "list"]).output().unwrap();
    assert!(listed.status.success(), "{:?}", listed);
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "research\nclient_memory\n"
    );
    let query = command(&dir)
        .args([
            "cypher",
            "--database",
            "data/research",
            "--query",
            "CREATE (c:Customer {entity_id:'synthetic-1', name:'Ada'}) RETURN c.name",
        ])
        .output()
        .unwrap();
    assert!(query.status.success(), "{:?}", query);
    let stats = command(&dir)
        .args(["db", "stats", "research"])
        .output()
        .unwrap();
    assert!(stats.status.success(), "{:?}", stats);
    assert_eq!(
        String::from_utf8(stats.stdout).unwrap(),
        "database=research nodes=1 edges=0\n"
    );
    let json_stats = command(&dir)
        .args(["db", "stats", "research", "--json"])
        .output()
        .unwrap();
    assert!(json_stats.status.success(), "{:?}", json_stats);
    let value: serde_json::Value = serde_json::from_slice(&json_stats.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"database":"research","nodes":1,"edges":0})
    );
    let schema = command(&dir)
        .args(["db", "schema", "research", "--json"])
        .output()
        .unwrap();
    assert!(schema.status.success(), "{:?}", schema);
    let schema: serde_json::Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(schema["database"], "research");
    assert_eq!(schema["catalog"]["version"], 1);
    assert_eq!(schema["catalog"]["node_types"][0]["label"], "Customer");
    assert_eq!(schema["secondary_index_status"], "declared_only");
    let metrics = command(&dir)
        .args(["db", "metrics", "research", "--json"])
        .output()
        .unwrap();
    assert!(metrics.status.success(), "{:?}", metrics);
    let metrics: serde_json::Value = serde_json::from_slice(&metrics.stdout).unwrap();
    assert_eq!(metrics["kind"], "rocksdb_estimates");
    assert!(metrics["column_families"]["node"]["estimated_keys"].is_number());
    let applied = command(&dir)
        .args(["db", "apply-ontology", "research", "schema.json"])
        .output()
        .unwrap();
    assert!(applied.status.success(), "{:?}", applied);
    let revised = command(&dir)
        .args(["db", "schema", "research", "--json"])
        .output()
        .unwrap();
    assert!(revised.status.success(), "{:?}", revised);
    let revised: serde_json::Value = serde_json::from_slice(&revised.stdout).unwrap();
    assert_eq!(revised["catalog"]["version"], 2);
    assert_eq!(
        revised["catalog"]["node_types"][0]["id"],
        schema["catalog"]["node_types"][0]["id"]
    );
    assert_eq!(
        revised["catalog"]["node_types"][0]["columns"],
        schema["catalog"]["node_types"][0]["columns"]
    );
    let reopened = command(&dir)
        .args([
            "cypher",
            "--database",
            "data/research",
            "--query",
            "MATCH (c:Customer {entity_id:'synthetic-1'}) RETURN c.name",
        ])
        .output()
        .unwrap();
    assert!(reopened.status.success(), "{:?}", reopened);
    assert!(String::from_utf8_lossy(&reopened.stdout).contains("Ada"));
}

#[test]
fn invalid_name_never_escapes_configured_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let result = command(&dir)
        .args(["db", "create", "../escape"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid database name"));
    assert!(!dir.path().join("escape").exists());
}

#[test]
fn invalid_ontology_does_not_create_database() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.json"), "not json").unwrap();
    let result = command(&dir)
        .args(["db", "create", "research", "--ontology", "bad.json"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid ontology"));
    assert!(!dir.path().join("data/research").exists());
}
