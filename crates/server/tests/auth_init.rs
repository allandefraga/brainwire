//! Native setup must keep generated service credentials out of diagnostics.
#![cfg(unix)]
use std::process::Command;

#[test]
fn cli_never_prints_tokens_and_refuses_credential_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("private-auth");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_brainwire"))
            .args(["auth", "init", "--directory"])
            .arg(&directory)
            .args(["--graph", "tickets"])
            .output()
            .unwrap()
    };
    let created = run();
    assert!(created.status.success());
    let before: Vec<_> = ["admin.json", "health.json", "policy.json"]
        .map(|name| std::fs::read(directory.join(name)).unwrap())
        .into_iter()
        .collect();
    let denied = run();
    assert!(!denied.status.success());
    for bytes in &before[..2] {
        let credential: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        let token = credential["credentials"].as_str().unwrap();
        for output in [&created, &denied] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
        }
    }
    for (name, bytes) in ["admin.json", "health.json", "policy.json"]
        .iter()
        .zip(before)
    {
        assert_eq!(std::fs::read(directory.join(name)).unwrap(), bytes);
    }
}
