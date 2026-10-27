//! Protected bounded graph previews retain typed IDs and never open client storage.
use std::{
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    c.current_dir(dir);
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("BRAINWIRE_")) {
        c.env_remove(key);
    }
    c.env("BRAINWIRE_SERVER_DATA_DIR", dir.join("data"))
        .env("BRAINWIRE_SERVER_DEFAULT_DATABASE", "tickets")
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB")
        .env("BRAINWIRE_MEMORY_QUERY", "16MiB");
    c
}
#[test]
fn protected_graph_preview_bounds_relationship_rows_and_preserves_missing_and_isolated_sources() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("schema.json"), r#"{"version":"1","nodes":[{"label":"NodeA","properties":[]},{"label":"NodeB","properties":[]},{"label":"Other","properties":[]}],"edges":[{"type":"LINK","from":{"label":"NodeA"},"to":{"label":"NodeB"}}]}"#).unwrap();
    let created = command(dir.path())
        .args(["db", "create", "tickets", "--ontology", "schema.json"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let query = |q: &str| {
        let out = command(dir.path())
            .args(["cypher", "--database", "data/tickets", "--query", q])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    query("UNWIND ['many','exact','parallel','isolated'] AS id CREATE (n:NodeA {entity_id:id})");
    for (source, count) in [("many", 1000), ("exact", 20)] {
        let ids = (0..count)
            .map(|i| format!("'{source}-{i}'"))
            .collect::<Vec<_>>()
            .join(",");
        query(&format!("UNWIND [{ids}] AS id MATCH (n:NodeA {{entity_id:'{source}'}}) CREATE (m:NodeB {{entity_id:id}}) CREATE (n)-[:LINK]->(m)"));
    }
    query("CREATE (m:NodeB {entity_id:'shared'}) MATCH (n:NodeA {entity_id:'parallel'}) CREATE (n)-[:LINK]->(m) CREATE (n)-[:LINK]->(m)");
    query("CREATE (n:Other {entity_id:'many'})");
    std::fs::write(dir.path().join("policy.json"), r#"{"principals":[{"id":"reader","token_sha256":"a0fab1377f49a759b57f63318262ebe89fabfc990e8e93ceac2984561482b9d4","graphs":{"tickets":"read"},"instance_admin":false}]}"#).unwrap();
    std::fs::write(
        dir.path().join("reader.json"),
        serde_json::json!({"principal":"reader","credentials":"b".repeat(64)}).to_string(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        command(dir.path())
            .arg("serve")
            .env("BRAINWIRE_SERVER_LISTEN", &address)
            .env("BRAINWIRE_SERVER_AUTH_FILE", dir.path().join("policy.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ready = false;
    for _ in 0..100 {
        if command(dir.path())
            .args(["probe", "--address", &address, "--timeout-ms", "100"])
            .env("BRAINWIRE_BOLT_AUTH_FILE", dir.path().join("reader.json"))
            .output()
            .unwrap()
            .status
            .success()
        {
            ready = true;
            break;
        }
        assert!(server.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(ready);
    let preview = |id: &str, lang: &str, graph: &str, target: &str| {
        command(dir.path())
            .args([
                "shell",
                "--connect",
                &address,
                "--graph",
                graph,
                "--view",
                "graph",
                "--node-label",
                "NodeA",
                "--entity-id",
                id,
                "--edge-type",
                "LINK",
                "--target-label",
                target,
                "--snapshot",
                "--width",
                "80",
                "--height",
                "24",
                "--lang",
                lang,
            ])
            .env("BRAINWIRE_BOLT_AUTH_FILE", dir.path().join("reader.json"))
            .env(
                "BRAINWIRE_SERVER_DATA_DIR",
                dir.path().join("client-must-not-open"),
            )
            .env("BRAINWIRE_MEMORY_CACHE", "invalid-client-config")
            .output()
            .unwrap()
    };
    for (id, count, more) in [
        ("many", 20, true),
        ("exact", 20, false),
        ("parallel", 1, false),
        ("isolated", 0, false),
        ("missing", 0, false),
    ] {
        for lang in ["en", "pt-BR"] {
            let out = preview(id, lang, "tickets", "NodeB");
            assert!(out.status.success(), "{out:?}");
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(
                text.contains(&format!(
                    "{count} {}",
                    if lang == "en" {
                        "outgoing rows"
                    } else {
                        "relações de saída"
                    }
                )),
                "{text}"
            );
            assert_eq!(
                text.contains(if lang == "en" {
                    "MORE AVAILABLE"
                } else {
                    "MAIS DISPONÍVEIS"
                }),
                more,
                "{text}"
            );
            if id == "isolated" {
                assert!(
                    text.contains(if lang == "en" {
                        "No matching outgoing"
                    } else {
                        "Sem relações deste tipo"
                    }),
                    "{text}"
                );
            }
            if id == "missing" {
                assert!(
                    text.contains(if lang == "en" {
                        "Entity not found"
                    } else {
                        "Entidade não encontrada"
                    }),
                    "{text}"
                );
            }
            if id == "parallel" {
                assert!(text.contains("shared"), "{text}");
            }
        }
    }
    assert!(!dir.path().join("client-must-not-open").exists());
    for (graph, target) in [("secret", "NodeB"), ("tickets", "Other")] {
        let out = preview("many", "en", graph, target);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
}
