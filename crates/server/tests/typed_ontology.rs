#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::process::{Command, Output};

const ONTOLOGY: &str = r#"{
    "version":"1",
    "nodes":[
        {"label":"Project","properties":["priority"],"property_types":{"priority":"integer"},"indexed_fields":["priority"]},
        {"label":"Decision","properties":["approved"],"property_types":{"approved":"boolean"}}
    ],
    "edges":[
        {"type":"HAS_DECISION","from":{"label":"Project"},"to":{"label":"Decision"},"properties":["confidence"],"property_types":{"confidence":"float"}}
    ]
}"#;

fn run(root: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    for (key, _) in
        std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("BRAINWIRE_"))
    {
        cmd.env_remove(key);
    }
    cmd.current_dir(root)
        .env("BRAINWIRE_SERVER_DATA_DIR", root.join("data"))
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "8MiB")
        .args(args)
        .output()
        .unwrap()
}

fn query(root: &std::path::Path, database: &str, query: &str) -> Output {
    run(root, &["cypher", "--database", database, "--query", query])
}

#[test]
fn standalone_ontology_checks_types_across_writes_and_checkpoint_reopen() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("typed.json"), ONTOLOGY).unwrap();
    let created = run(
        root,
        &["db", "create", "default", "--ontology", "typed.json"],
    );
    assert!(created.status.success(), "{created:?}");
    let seeded = query(root, "data/default", "CREATE (:Project {entity_id:'p', priority:1}) CREATE (:Decision {entity_id:'d', approved:true})");
    assert!(seeded.status.success(), "{seeded:?}");
    let seeded_edge = query(root, "data/default", "MATCH (p:Project {entity_id:'p'}), (d:Decision {entity_id:'d'}) MERGE (p)-[:HAS_DECISION {confidence:0.9}]->(d)");
    assert!(seeded_edge.status.success(), "{seeded_edge:?}");

    let bad = query(root, "data/default", "CREATE (:Project {entity_id:'rolled-back', priority:3}) CREATE (:Decision {entity_id:'bad', approved:'true'})");
    assert!(!bad.status.success(), "{bad:?}");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("boolean"),
        "{bad:?}"
    );
    let absent = query(
        root,
        "data/default",
        "MATCH (p:Project {entity_id:'rolled-back'}) RETURN count(p)",
    );
    assert!(absent.status.success(), "{absent:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&absent.stdout).unwrap()["rows"],
        serde_json::json!([[0]])
    );

    let bad_edge = query(
        root,
        "data/default",
        "MATCH (:Project)-[r:HAS_DECISION]->(:Decision) SET r.confidence=1",
    );
    assert!(!bad_edge.status.success(), "{bad_edge:?}");
    assert!(
        String::from_utf8_lossy(&bad_edge.stderr).contains("float"),
        "{bad_edge:?}"
    );

    let reapplied = run(root, &["db", "apply-ontology", "default", "typed.json"]);
    assert!(reapplied.status.success(), "{reapplied:?}");
    std::fs::write(
        root.join("untyped.json"),
        ONTOLOGY.replace(",\"property_types\":{\"priority\":\"integer\"}", ""),
    )
    .unwrap();
    let omitted = run(root, &["db", "apply-ontology", "default", "untyped.json"]);
    assert!(omitted.status.success(), "{omitted:?}");
    let ddl = query(root, "data/default", "CALL db.createLabel('Note')");
    assert!(ddl.status.success(), "{ddl:?}");
    let ddl = query(
        root,
        "data/default",
        "CALL db.createRelationshipType('HAS_DECISION', 'Decision', 'Project')",
    );
    assert!(ddl.status.success(), "{ddl:?}");
    let schema = query(
        root,
        "data/default",
        "CALL db.schema() YIELD nodes, relationships RETURN nodes, relationships",
    );
    assert!(schema.status.success(), "{schema:?}");
    let json = serde_json::from_slice::<serde_json::Value>(&schema.stdout).unwrap();
    assert_eq!(
        json["rows"][0][0][0]["propertyTypes"]["priority"],
        "integer"
    );
    assert_eq!(
        json["rows"][0][1][1]["propertyTypes"]["confidence"],
        "float"
    );
    let ddl = query(
        root,
        "data/default",
        "CALL db.dropProperty('Decision', 'approved')",
    );
    assert!(!ddl.status.success(), "{ddl:?}");
    assert!(
        String::from_utf8_lossy(&ddl.stderr).contains("cannot drop typed"),
        "{ddl:?}"
    );
    let attempted_change = query(
        root,
        "data/default",
        "MATCH (p:Project) SET p.priority='wrong'",
    );
    assert!(!attempted_change.status.success(), "{attempted_change:?}");
    let removed = query(
        root,
        "data/default",
        "MATCH (:Project)-[r:HAS_DECISION]->(:Decision) SET r.confidence=NULL",
    );
    assert!(removed.status.success(), "{removed:?}");

    let checkpoint = run(
        root,
        &["db", "checkpoint", "default", "--destination", "copied"],
    );
    assert!(checkpoint.status.success(), "{checkpoint:?}");
    let copy = query(
        root,
        "copied",
        "MATCH (p:Project) WHERE p.priority=1 RETURN p.entity_id",
    );
    assert!(copy.status.success(), "{copy:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&copy.stdout).unwrap()["rows"],
        serde_json::json!([["p"]])
    );
    let copy_rejection = query(root, "copied", "MATCH (p:Project) SET p.priority=false");
    assert!(!copy_rejection.status.success(), "{copy_rejection:?}");
    let checked = run(root, &["db", "check", "default"]);
    assert!(checked.status.success(), "{checked:?}");
}

#[test]
fn applying_a_typed_ontology_to_an_existing_graph_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("typed.json"), ONTOLOGY).unwrap();
    let created = run(root, &["db", "create", "existing"]);
    assert!(created.status.success(), "{created:?}");
    let rejected = run(root, &["db", "apply-ontology", "existing", "typed.json"]);
    assert!(!rejected.status.success(), "{rejected:?}");
    let rejected = run(
        root,
        &["db", "create", "existing", "--ontology", "typed.json"],
    );
    assert!(!rejected.status.success(), "{rejected:?}");
}

#[test]
fn invalid_or_conflicting_declarations_do_not_publish_or_change_a_graph() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let bad_rule = ONTOLOGY.replace("\"priority\":\"integer\"", "\"undeclared\":\"integer\"");
    std::fs::write(root.join("bad.json"), bad_rule).unwrap();
    let rejected = run(root, &["db", "create", "never", "--ontology", "bad.json"]);
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(!root.join("data/never").exists());
    let bad_rule = ONTOLOGY.replace("\"priority\"", "\"aCL_Hidden\"");
    std::fs::write(root.join("bad.json"), bad_rule).unwrap();
    let rejected = run(root, &["db", "create", "never", "--ontology", "bad.json"]);
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(!root.join("data/never").exists());

    std::fs::write(root.join("typed.json"), ONTOLOGY).unwrap();
    let created = run(
        root,
        &["db", "create", "default", "--ontology", "typed.json"],
    );
    assert!(created.status.success(), "{created:?}");
    let before = run(root, &["db", "schema", "default", "--json"]);
    assert!(before.status.success(), "{before:?}");
    let bad_rule = ONTOLOGY.replace("\"priority\":\"integer\"", "\"priority\":\"float\"");
    std::fs::write(root.join("bad.json"), bad_rule).unwrap();
    let rejected = run(root, &["db", "apply-ontology", "default", "bad.json"]);
    assert!(!rejected.status.success(), "{rejected:?}");
    let after = run(root, &["db", "schema", "default", "--json"]);
    assert_eq!(before.stdout, after.stdout);
}

#[test]
fn embedded_create_installs_the_contract_before_running_the_query() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("typed.json"), ONTOLOGY).unwrap();
    let rejected = run(
        root,
        &[
            "cypher",
            "--database",
            "embedded",
            "--ontology",
            "typed.json",
            "--query",
            "CREATE (:Project {entity_id:'bad', priority:1.0})",
        ],
    );
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("integer"),
        "{rejected:?}"
    );
    let valid = query(
        root,
        "embedded",
        "CREATE (:Project {entity_id:'valid', priority:1})",
    );
    assert!(valid.status.success(), "{valid:?}");
    let match_all = query(root, "embedded", "MATCH (p:Project) RETURN p.entity_id");
    assert!(match_all.status.success(), "{match_all:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&match_all.stdout).unwrap()["rows"],
        serde_json::json!([["valid"]])
    );
}

#[test]
fn concurrent_typed_creates_cannot_replace_each_other() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::write(root.join("typed.json"), ONTOLOGY).unwrap();
    let alternative = ONTOLOGY.replace("\"priority\":\"integer\"", "\"priority\":\"string\"");
    std::fs::write(root.join("alternative.json"), alternative).unwrap();
    let start = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            run(
                root,
                &["db", "create", "racing", "--ontology", "typed.json"],
            )
        });
        start.wait();
        let second = run(
            root,
            &["db", "create", "racing", "--ontology", "alternative.json"],
        );
        (first.join().unwrap(), second)
    });
    assert_ne!(
        first.status.success(),
        second.status.success(),
        "{first:?} {second:?}"
    );
    let schema = run(root, &["db", "schema", "racing", "--json"]);
    assert!(schema.status.success(), "{schema:?}");
    let json = serde_json::from_slice::<serde_json::Value>(&schema.stdout).unwrap();
    let expected = if first.status.success() {
        "integer"
    } else {
        "string"
    };
    assert_eq!(
        json["catalog"]["node_types"][0]["columns"][0]["property_type"],
        expected
    );
    assert!(root.join("data").read_dir().unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".brainwire-typed-")));
}

#[test]
fn unwind_obeys_node_and_keyed_relationship_types_and_rolls_back() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let mut ontology: serde_json::Value = serde_json::from_str(ONTOLOGY).unwrap();
    ontology["edges"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "type":"MILESTONE", "from":{"label":"Project"}, "to":{"label":"Decision"},
            "properties":["kind","rating"], "merge_key_fields":["kind"],
            "property_types":{"kind":"string","rating":"integer"}
        }));
    std::fs::write(
        root.join("typed.json"),
        serde_json::to_vec(&ontology).unwrap(),
    )
    .unwrap();
    let created = run(
        root,
        &["db", "create", "default", "--ontology", "typed.json"],
    );
    assert!(created.status.success(), "{created:?}");
    let db = "data/default";
    let seeded = query(
        root,
        db,
        "CREATE (:Project {entity_id:'p',priority:1}) CREATE (:Decision {entity_id:'d'})",
    );
    assert!(seeded.status.success(), "{seeded:?}");
    let nodes = query(root, db, "UNWIND [{id:'p',value:2},{id:'bad',value:'wrong'}] AS row MERGE (n:Project {entity_id:row.id}) SET n.priority=row.value");
    assert!(!nodes.status.success(), "{nodes:?}");
    assert!(
        String::from_utf8_lossy(&nodes.stderr).contains("priority requires integer"),
        "{nodes:?}"
    );
    let nodes = query(root, db, "MATCH (n:Project) RETURN n.entity_id,n.priority");
    assert!(nodes.status.success(), "{nodes:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&nodes.stdout).unwrap()["rows"],
        serde_json::json!([["p", 1]])
    );

    let valid = query(root, db, "MATCH (p:Project),(d:Decision) UNWIND [{kind:'open',rating:2},{kind:'close',rating:3}] AS row MERGE (p)-[r:MILESTONE {kind:row.kind}]->(d) SET r.rating=row.rating");
    assert!(valid.status.success(), "{valid:?}");
    let rejected = query(root, db, "MATCH (p:Project),(d:Decision) UNWIND [{kind:'open',rating:6},{kind:'invalid',rating:2.0}] AS row MERGE (p)-[r:MILESTONE {kind:row.kind}]->(d) SET r.rating=row.rating");
    assert!(!rejected.status.success(), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("rating requires integer"),
        "{rejected:?}"
    );
    let edges = query(
        root,
        db,
        "MATCH (:Project)-[r:MILESTONE]->(:Decision) RETURN r.kind,r.rating ORDER BY r.kind",
    );
    assert!(edges.status.success(), "{edges:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&edges.stdout).unwrap()["rows"],
        serde_json::json!([["close", 3], ["open", 2]])
    );
}
