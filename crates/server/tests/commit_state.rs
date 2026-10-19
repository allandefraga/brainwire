use std::process::{Command, Output};

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

fn json(output: Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn commit_state_is_read_only_durable_and_scoped_to_each_graph() {
    let dir = tempfile::tempdir().unwrap();
    let created = command(&dir)
        .args(["db", "create", "research"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let state = || {
        json(
            command(&dir)
                .args(["db", "commit-state", "research", "--json"])
                .output()
                .unwrap(),
        )
    };
    assert_eq!(
        state(),
        serde_json::json!({"database":"research","sequence":"0","tracked":false,"scope":"store_latest"})
    );
    std::fs::write(
        dir.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    let applied = command(&dir)
        .args(["db", "apply-ontology", "research", "schema.json"])
        .output()
        .unwrap();
    assert!(applied.status.success(), "{applied:?}");
    let committed = state();
    assert_eq!(committed["sequence"], "1");
    assert_eq!(committed["tracked"], true);
    assert_eq!(state(), committed);
    let overview = json(
        command(&dir)
            .args(["db", "overview", "--estimated", "--json"])
            .output()
            .unwrap(),
    );
    let graph = overview["graphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "research")
        .unwrap();
    assert_eq!(
        graph["commit_state"],
        serde_json::json!({"sequence":"1","tracked":true,"scope":"store_latest"})
    );
    for (language, label) in [
        ("en", "Persisted store sequence: 1"),
        ("pt-BR", "Sequência persistida do store: 1"),
    ] {
        let rendered = command(&dir)
            .args([
                "shell",
                "--overview",
                "--estimated",
                "--graph",
                "research",
                "--view",
                "query",
                "--snapshot",
                "--width",
                "160",
                "--height",
                "48",
                "--lang",
                language,
            ])
            .output()
            .unwrap();
        assert!(rendered.status.success(), "{rendered:?}");
        assert!(String::from_utf8(rendered.stdout).unwrap().contains(label));
        assert_eq!(state(), committed);
    }
    let untouched = json(
        command(&dir)
            .args(["db", "commit-state", "default", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(untouched["tracked"], false);
    let missing = command(&dir)
        .args(["db", "commit-state", "missing", "--json"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("does not exist"));
    let text = command(&dir)
        .args(["db", "commit-state", "research"])
        .output()
        .unwrap();
    assert!(text.status.success());
    assert_eq!(
        String::from_utf8(text.stdout).unwrap(),
        "database=research commit_sequence=1 tracked=true scope=store_latest\n"
    );
}

#[test]
fn checkpoint_receipt_matches_reopened_checkpoint_and_explicit_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("brainwire.toml"),
        "[server]\ndata_dir = 'configured'\ndefault_database = 'research'\n",
    )
    .unwrap();
    // An explicit config is still overridden by the configured data-dir environment.
    let state = json(
        command(&dir)
            .args([
                "db",
                "commit-state",
                "research",
                "--json",
                "--config",
                "brainwire.toml",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(state["tracked"], false);
    let receipt = json(
        command(&dir)
            .args([
                "db",
                "checkpoint",
                "research",
                "--destination",
                "data/copied",
                "--config",
                "brainwire.toml",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(receipt["commit_sequence"], "0");
    assert_eq!(receipt["commit_sequence_tracked"], false);
    let copy = json(
        command(&dir)
            .args([
                "db",
                "commit-state",
                "copied",
                "--json",
                "--config",
                "brainwire.toml",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(copy["sequence"], receipt["commit_sequence"]);
    assert_eq!(copy["tracked"], receipt["commit_sequence_tracked"]);
    std::fs::write(
        dir.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    assert!(command(&dir)
        .args([
            "db",
            "apply-ontology",
            "research",
            "schema.json",
            "--config",
            "brainwire.toml"
        ])
        .status()
        .unwrap()
        .success());
    let receipt = json(
        command(&dir)
            .args([
                "db",
                "checkpoint",
                "research",
                "--destination",
                "data/tracked",
                "--config",
                "brainwire.toml",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(receipt["commit_sequence"], "1");
    assert_eq!(receipt["commit_sequence_tracked"], true);
    let copy = json(
        command(&dir)
            .args([
                "db",
                "commit-state",
                "tracked",
                "--json",
                "--config",
                "brainwire.toml",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(copy["sequence"], receipt["commit_sequence"]);
}
