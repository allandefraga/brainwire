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
fn authenticated_non_loopback_probe_requires_explicit_opt_in_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let token = "a".repeat(64);
    let auth = dir.path().join("credentials.json");
    std::fs::write(
        &auth,
        format!(r#"{{"principal":"test-probe","credentials":"{token}"}}"#),
    )
    .unwrap();
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();

    for (mode, override_value) in [
        ("live", None),
        ("live", Some("")),
        ("live", Some("0")),
        ("ready", Some("true")),
        ("ready", Some(" 1")),
    ] {
        let mut probe = command(dir.path());
        probe
            .args(["probe", "--address", &address, "--mode", mode])
            .env("BRAINWIRE_BOLT_AUTH_FILE", &auth);
        if let Some(value) = override_value {
            probe.env("BRAINWIRE_ALLOW_PLAINTEXT_BOLT_AUTH", value);
        }
        let output = probe.output().unwrap();
        assert!(!output.status.success());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("refusing to send Bolt credentials"),
            "{error}"
        );
        assert!(!error.contains(&token));
    }
}

#[cfg(unix)]
#[test]
fn native_probe_preserves_loopback_auth_opt_in_and_credentialless_modes() {
    use std::io::{Read, Write};
    use std::time::Instant;

    fn message(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut size = [0_u8; 2];
        stream.read_exact(&mut size).unwrap();
        let mut body = vec![0; usize::from(u16::from_be_bytes(size))];
        stream.read_exact(&mut body).unwrap();
        stream.read_exact(&mut size).unwrap();
        assert_eq!(size, [0, 0]);
        body
    }

    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("credentials.json");
    let token = "a".repeat(64);
    std::fs::write(
        &auth,
        format!(r#"{{"principal":"test-probe","credentials":"{token}"}}"#),
    )
    .unwrap();

    for (ip, mode, auth_file, opt_in, expected_token) in [
        ("127.0.0.1", "live", Some(auth.clone()), false, true),
        (
            "[::ffff:127.0.0.1]",
            "live",
            Some(auth.clone()),
            false,
            true,
        ),
        ("0.0.0.0", "live", Some(auth.clone()), true, true),
        ("0.0.0.0", "live", None, false, false),
        (
            "0.0.0.0",
            "handshake",
            Some(dir.path().join("missing")),
            false,
            false,
        ),
    ] {
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("{ip}:{}", listener.local_addr().unwrap().port());
        let mock_token = token.clone();
        let peer = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("probe did not connect: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut greeting = [0; 20];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(&greeting[..4], &[0x60, 0x60, 0xb0, 0x17]);
            stream.write_all(&[0, 0, 4, 5]).unwrap();
            assert_eq!(message(&mut stream), [0xb1, 0x01, 0xa0]);
            let success = [0, 3, 0xb1, 0x70, 0xa0, 0, 0];
            stream.write_all(&success).unwrap();
            if mode == "handshake" {
                assert_eq!(stream.read(&mut [0]).unwrap(), 0);
            } else {
                let logon = message(&mut stream);
                assert_eq!(&logon[..2], &[0xb1, 0x6a]);
                if expected_token {
                    assert!(logon
                        .windows(mock_token.len())
                        .any(|part| part == mock_token.as_bytes()));
                } else {
                    assert_eq!(logon, [0xb1, 0x6a, 0xa0]);
                }
                stream.write_all(&success).unwrap();
            }
        });
        let mut probe = command(dir.path());
        probe.args(["probe", "--address", &address, "--mode", mode]);
        if let Some(path) = auth_file {
            probe.env("BRAINWIRE_BOLT_AUTH_FILE", path);
        }
        if opt_in {
            probe.env("BRAINWIRE_ALLOW_PLAINTEXT_BOLT_AUTH", "1");
        }
        let output = probe.output().unwrap();
        assert!(
            output.status.success(),
            "{address} {mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        peer.join().unwrap();
    }
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
