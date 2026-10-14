//! Local-only CLI probes; synthetic peers assert exact Status/Lookup framing.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
const TOKEN: &str = "synthetic-probe-0123456789abcdef0123456789abcdef";
fn status() -> serde_json::Value {
    serde_json::json!({"cluster_id":"probe-test","node_id":1,"state":"Leader","current_term":1,"leader":1,"last_log_index":0,"last_applied":0,"committed_index":0,"membership":[1,2,3],"log_bytes":0,"max_log_bytes":1048576,"log_remaining_bytes":1048576,"running":true})
}
fn probe(
    kind: Option<&str>,
    status: serde_json::Value,
    lookup: Option<serde_json::Value>,
) -> Output {
    let dir = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let token = dir.path().join("token");
    std::fs::write(&token, TOKEN).unwrap();
    let mut listeners = (1..=3)
        .map(|id| (id, TcpListener::bind("127.0.0.1:0").unwrap()))
        .collect::<Vec<_>>();
    let peers = listeners
        .iter()
        .map(|(id, listener)| {
            format!(
                "[[peers]]\nid={id}\naddress=\"{}\"\n",
                listener.local_addr().unwrap()
            )
        })
        .collect::<String>();
    let config = dir.path().join("cluster.toml");
    std::fs::write(&config,format!("node_id=1\ncluster_id=\"probe-test\"\nlisten=\"127.0.0.1:10001\"\ntoken_file={:?}\n{peers}",token.to_str().unwrap())).unwrap();
    let (_, listener) = listeners.remove(0);
    let worker = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut responses = vec![(
            serde_json::json!("Status"),
            serde_json::json!({"Status":status}),
        )];
        if let Some(reply) = lookup {
            responses.push((
                serde_json::json!({"Lookup":{"request_id":"brainwire-readiness-v1"}}),
                reply,
            ));
        }
        for (expected, response) in responses {
            let start = Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(peer) => break peer,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(10));
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut prefix = [0; 4];
            stream.read_exact(&mut prefix).unwrap();
            let size = u32::from_be_bytes(prefix) as usize;
            assert!(size <= 8 << 20);
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["recipient"], 1);
            assert_eq!(request["token"], TOKEN);
            assert_eq!(request["payload"], expected);
            let body=serde_json::to_vec(&serde_json::json!({"protocol":1,"cluster_id":"probe-test","sender":1,"recipient":1,"token":TOKEN,"payload":response})).unwrap();
            stream
                .write_all(&(body.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    command
        .args(["cluster", "--cluster-config"])
        .arg(config)
        .arg("probe");
    if let Some(kind) = kind {
        command.args(["--kind", kind]);
    }
    let output = command.output().unwrap();
    worker.join().unwrap();
    drop(listeners);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    output
}
fn parsed(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn readiness_defaults_and_discards_both_absent_and_existing_lookup_payloads() {
    for reply in [
        serde_json::json!({"Lookup":null}),
        serde_json::json!({"Lookup":{"Applied":{"revision":987654}}}),
    ] {
        let output = probe(None, status(), Some(reply));
        assert!(output.status.success());
        let report = parsed(&output);
        assert_eq!(report["kind"], "raft_probe");
        assert_eq!(report["probe_kind"], "readiness");
        assert_eq!(report["success"], true);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("987654"));
    }
}
#[test]
fn follower_and_uninitialized_are_live_but_not_ready() {
    let mut follower = status();
    follower["state"] = "Follower".into();
    follower["leader"] = 2.into();
    let mut uninitialized = status();
    uninitialized["state"] = "Learner".into();
    uninitialized["membership"] = serde_json::json!([]);
    uninitialized["leader"] = serde_json::Value::Null;
    for status in [follower, uninitialized] {
        assert!(probe(Some("liveness"), status.clone(), None)
            .status
            .success());
        let output = probe(Some("readiness"), status, None);
        assert!(!output.status.success());
        assert_eq!(parsed(&output)["success"], false);
    }
}
#[test]
fn stopped_wrong_identity_and_redirect_do_not_pass_readiness() {
    let mut stopped = status();
    stopped["running"] = false.into();
    assert!(!probe(Some("liveness"), stopped, None).status.success());
    let mut wrong = status();
    wrong["node_id"] = 2.into();
    assert!(!probe(Some("liveness"), wrong, None).status.success());
    let output = probe(
        Some("readiness"),
        status(),
        Some(serde_json::json!({"Rejected":{"code":"lookup_unavailable","leader":2}})),
    );
    assert!(!output.status.success());
    assert_eq!(parsed(&output)["reason"], "quorum_unavailable");
}
#[test]
fn cli_rejects_unknown_probe_kind() {
    let output = Command::new(env!("CARGO_BIN_EXE_brainwire"))
        .args([
            "cluster",
            "--cluster-config",
            "unused",
            "probe",
            "--kind",
            "write",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
}
