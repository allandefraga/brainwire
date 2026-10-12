//! Connected index telemetry honors server permissions and never opens local storage.
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
fn protected_indexes_allow_reader_deny_unauthorized_graph_and_keeps_client_storage_closed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    let created = command(dir.path())
        .args(["db", "create", "tickets", "--ontology", "schema.json"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    std::fs::write(dir.path().join("policy.json"),r#"{"principals":[{"id":"admin","token_sha256":"ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb","graphs":{"tickets":"read"},"instance_admin":true},{"id":"reader","token_sha256":"a0fab1377f49a759b57f63318262ebe89fabfc990e8e93ceac2984561482b9d4","graphs":{"tickets":"read"},"instance_admin":false}]}"#).unwrap();
    for (principal, token) in [("admin", "a"), ("reader", "b")] {
        std::fs::write(
            dir.path().join(format!("{principal}.json")),
            serde_json::json!({"principal":principal,"credentials":token.repeat(64)}).to_string(),
        )
        .unwrap();
    }
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
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(ready, "protected server not ready");
    let args = [
        "shell",
        "--connect",
        &address,
        "--graph",
        "tickets",
        "--view",
        "indexes",
        "--snapshot",
        "--width",
        "80",
        "--height",
        "24",
    ];
    for lang in ["en", "pt-BR"] {
        let output = command(dir.path())
            .args(args)
            .args(["--lang", lang])
            .env("BRAINWIRE_BOLT_AUTH_FILE", dir.path().join("reader.json"))
            .env(
                "BRAINWIRE_SERVER_DATA_DIR",
                dir.path().join("client-must-not-open"),
            )
            .env("BRAINWIRE_MEMORY_CACHE", "invalid-client-config")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        for expected in [
            "Customer",
            "Company",
            "industry",
            if lang == "en" {
                "SERVER SNAPSHOT"
            } else {
                "SNAPSHOT DO SERVIDOR"
            },
            if lang == "en" { "READY" } else { "PRONTO" },
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
    }
    assert!(!dir.path().join("client-must-not-open").exists());
    let mut unauthorized = args;
    unauthorized[4] = "secret";
    let denied = command(dir.path())
        .args(unauthorized)
        .env("BRAINWIRE_BOLT_AUTH_FILE", dir.path().join("reader.json"))
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(denied.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("Bolt failure"),
        "{denied:?}"
    );
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let stopped = command(dir.path())
        .args(args)
        .env("BRAINWIRE_BOLT_AUTH_FILE", dir.path().join("reader.json"))
        .output()
        .unwrap();
    assert!(!stopped.status.success());
    assert!(stopped.stdout.is_empty());
}
