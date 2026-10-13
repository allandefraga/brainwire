//! The connected inspector must never open the client's local RocksDB directory.
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("BRAINWIRE_")) {
        c.env_remove(key);
    }
    c.env("BRAINWIRE_SERVER_DATA_DIR", dir.join("data"))
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB");
    c
}

#[test]
fn connected_schema_selects_named_graphs_while_server_owns_storage() {
    let dir = tempfile::tempdir().unwrap();
    for (name, schema) in [
        (
            "tickets",
            include_str!("../../core/tests/fixtures/graph_mapping.json"),
        ),
        (
            "knowledge",
            include_str!("../../../examples/knowledge/schema.json"),
        ),
    ] {
        std::fs::write(dir.path().join("schema.json"), schema).unwrap();
        let created = command(dir.path())
            .args(["db", "create", name, "--ontology", "schema.json"])
            .output()
            .unwrap();
        assert!(created.status.success(), "{created:?}");
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let child = command(dir.path())
        .arg("serve")
        .env("BRAINWIRE_SERVER_LISTEN", &address)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let mut ready = false;
    for _ in 0..100 {
        if command(dir.path())
            .args(["probe", "--address", &address, "--timeout-ms", "100"])
            .output()
            .unwrap()
            .status
            .success()
        {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(ready, "server not ready");
    assert!(
        !command(dir.path())
            .args(["db", "stats", "tickets"])
            .output()
            .unwrap()
            .status
            .success(),
        "offline access must be locked by the server"
    );
    for (graph, lang, expected, excluded) in [
        ("tickets", "en", "Customer", "KnowledgeSource"),
        ("knowledge", "pt-BR", "KnowledgeSource", "Customer"),
        ("client_memory", "en", "No ontology declared", "Customer"),
    ] {
        let output = command(dir.path())
            .args([
                "shell",
                "--connect",
                &address,
                "--graph",
                graph,
                "--view",
                "ontology",
                "--snapshot",
                "--lang",
                lang,
            ])
            .env(
                "BRAINWIRE_MEMORY_CACHE",
                "deliberately-invalid-client-config",
            )
            .env(
                "BRAINWIRE_SERVER_DATA_DIR",
                dir.path().join("not-created-by-client"),
            )
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains(excluded), "{text}");
        assert!(
            text.contains(if lang == "en" {
                "SERVER SNAPSHOT"
            } else {
                "SNAPSHOT DO SERVIDOR"
            }),
            "{text}"
        );
    }
    assert!(!dir.path().join("not-created-by-client").exists());
    let args = [
        "shell",
        "--connect",
        &address,
        "--graph",
        "missing",
        "--view",
        "ontology",
        "--snapshot",
    ];
    let missing = command(dir.path()).args(args).output().unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("database"),
        "{missing:?}"
    );
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let stopped = command(dir.path()).args(args).output().unwrap();
    assert!(!stopped.status.success());
    assert!(stopped.stdout.is_empty());
    let stats = command(dir.path())
        .args(["db", "stats", "tickets", "--json"])
        .output()
        .unwrap();
    assert!(stats.status.success(), "{stats:?}");
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["nodes"], 0);
    assert_eq!(stats["edges"], 0);
}

#[test]
fn connected_flags_cannot_fall_back_to_offline_or_demo() {
    let dir = tempfile::tempdir().unwrap();
    for extra in ["--demo", "--overview", "--estimated"] {
        let result = command(dir.path())
            .args([
                "shell",
                "--connect",
                "127.0.0.1:1",
                "--graph",
                "tickets",
                "--view",
                "ontology",
                "--snapshot",
                extra,
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    let result = command(dir.path())
        .args([
            "shell",
            "--connect",
            "127.0.0.1:1",
            "--graph",
            "tickets",
            "--snapshot",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot connect"));
    assert!(!dir.path().join("data").exists());
}
