use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    command.current_dir(dir);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("BRAINWIRE_")) {
        command.env_remove(key);
    }
    command
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB")
        .env("BRAINWIRE_SERVER_DATA_DIR", dir.join("data"));
    command
}

#[test]
fn ready_probe_requires_a_serving_database_and_fails_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);

    let probe = |mode: &str| {
        command(dir.path())
            .args([
                "probe",
                "--address",
                &address,
                "--mode",
                mode,
                "--timeout-ms",
                "200",
            ])
            .output()
            .unwrap()
    };
    assert!(!probe("ready").status.success());
    let server = command(dir.path())
        .args(["serve"])
        .env("BRAINWIRE_SERVER_LISTEN", &address)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(server);
    let mut ready = false;
    for _ in 0..100 {
        if probe("ready").status.success() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "server did not become query-ready");
    assert!(probe("live").status.success());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    assert!(!probe("ready").status.success());
}

#[test]
fn probe_rejects_zero_timeout_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path())
        .args(["probe", "--timeout-ms", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--timeout-ms"));
}

#[test]
fn handshake_probe_needs_no_credential_against_a_protected_server() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("policy.json"),
        r#"{"principals":[{"id":"admin","token_sha256":"ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb","graphs":{"default":"read"},"instance_admin":true}]}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let probe = |mode: &str| {
        command(dir.path())
            .args([
                "probe",
                "--address",
                &address,
                "--mode",
                mode,
                "--timeout-ms",
                "200",
            ])
            .output()
            .unwrap()
    };
    assert!(!probe("handshake").status.success(), "server is down");
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
    let mut up = false;
    for _ in 0..100 {
        if probe("handshake").status.success() {
            up = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(up, "protected server did not answer the handshake probe");
    assert!(
        !probe("ready").status.success(),
        "queries still need credentials"
    );
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    assert!(!probe("handshake").status.success());
}
