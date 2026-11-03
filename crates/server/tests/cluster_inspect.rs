//! Real CLI parsing and authenticated framing, with bounded synthetic status peers.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const TOKEN: &str = "synthetic0123456789abcdef0123456789abcdef";
const CAP: u64 = 1 << 20;

fn inspect(unavailable: Option<u64>, mismatch: bool, uninitialized: bool, invalid: u8) -> Output {
    inspect_with_overlap_check(unavailable, mismatch, uninitialized, invalid, false, false)
}

fn inspect_with_overlap_check(
    unavailable: Option<u64>,
    mismatch: bool,
    uninitialized: bool,
    invalid: u8,
    require_overlap: bool,
    missing_indexes: bool,
) -> Output {
    let received = Arc::new((Mutex::new(0), Condvar::new()));
    let dir = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, TOKEN).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let listeners = (1..=3)
        .map(|id| (id, TcpListener::bind("127.0.0.1:0").unwrap()))
        .collect::<Vec<_>>();
    let peers = listeners
        .iter()
        .map(|(id, listener)| {
            format!(
                "[[peers]]\nid = {id}\naddress = \"{}\"\n",
                listener.local_addr().unwrap()
            )
        })
        .collect::<String>();
    let config = dir.path().join("cluster.toml");
    std::fs::write(&config, format!("node_id = 1\ncluster_id = \"inspection-test\"\nlisten = \"127.0.0.1:10001\"\ntoken_file = {:?}\nmax_log_bytes = {CAP}\n{peers}", token_file.to_str().unwrap())).unwrap();
    let mut workers = Vec::new();
    for (id, listener) in listeners {
        if unavailable == Some(id) {
            drop(listener);
            continue;
        }
        let received = received.clone();
        workers.push(thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let started = Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() { Ok(peer) => break peer, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => { assert!(started.elapsed() < Duration::from_secs(10), "CLI never contacted configured peer"); thread::sleep(Duration::from_millis(10)); }, Err(error) => panic!("accept failed: {error}") }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut prefix=[0;4]; stream.read_exact(&mut prefix).unwrap();
            let length=u32::from_be_bytes(prefix) as usize; assert!(length <= 8 << 20);
            let mut body=vec![0;length]; stream.read_exact(&mut body).unwrap();
            let request:serde_json::Value=serde_json::from_slice(&body).unwrap();
            assert_eq!(request["payload"], "Status"); assert_eq!(request["recipient"],id); assert_eq!(request["token"],TOKEN);
            if require_overlap {
                let (count, ready) = &*received;
                let mut count = count.lock().unwrap();
                *count += 1;
                ready.notify_all();
                let (count, _) = ready
                    .wait_timeout_while(count, Duration::from_secs(3), |count| *count < 3)
                    .unwrap();
                assert_eq!(*count, 3, "all voters must receive requests before any responds");
            }
            let cap = if mismatch && id == 3 { CAP * 2 } else { CAP };
            let membership = if uninitialized { vec![] } else { vec![1,2,3] };
            let status=serde_json::json!({"cluster_id":"inspection-test","node_id":if invalid == 1 && id == 2 {99} else {id},
                "state":if uninitialized {"Learner"} else {"Follower"},"current_term":1,"leader":if uninitialized {None} else {Some(1)},
                "last_log_index":if uninitialized || missing_indexes {None} else {Some(100 + id)},
                "last_applied":if uninitialized || missing_indexes {None} else {Some(40 + id)},
                "committed_index":if uninitialized || missing_indexes {None} else {Some(42 + id)},"membership":membership,
                "log_bytes":0,"max_log_bytes":cap,"log_remaining_bytes":cap,"running":!(matches!(invalid, 2 | 3) && id == 2),
                "running_issue":if invalid == 3 && id == 2 {Some("storage_error")} else {None}});
            let response=serde_json::to_vec(&serde_json::json!({"protocol":1,"cluster_id":"inspection-test","sender":id,"recipient":1,"token":TOKEN,"payload":{"Status":status}})).unwrap();
            stream.write_all(&(response.len() as u32).to_be_bytes()).unwrap(); stream.write_all(&response).unwrap();
        }));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_brainwire"))
        .args(["cluster", "--cluster-config"])
        .arg(&config)
        .arg("inspect")
        .output()
        .unwrap();
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    output
}
fn parse_report(output: &Output) -> serde_json::Value {
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for member in report["members"].as_array().unwrap() {
        assert!(member["response_time_ms"].as_f64().is_some());
        assert!(member["response_time_ms"].as_f64().unwrap() >= 0.0);
    }
    report
}
#[test]
fn all_three_members_and_uninitialized_are_diagnostics_only() {
    let output = inspect(None, false, false, 0);
    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["kind"], "diagnostic_sample");
    assert_eq!(report["complete"], true);
    assert_eq!(report["all_initialized"], true);
    assert_eq!(report["retained_log_cap_agreement"], true);
    assert_eq!(report["members"].as_array().unwrap().len(), 3);
    assert!(report["scope"].as_str().unwrap().contains("not a quorum"));
    assert_eq!(
        report["replication_progress"]["committed_index"]["spread"],
        2
    );
    assert_eq!(report["replication_progress"]["applied_index"]["spread"], 2);
    assert_eq!(report["members"][0]["committed_index_lag"], 2);
    assert_eq!(report["members"][2]["applied_index_lag"], 0);
    let output = inspect(None, false, true, 0);
    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], true);
    assert_eq!(report["all_initialized"], false);
    assert_eq!(report["replication_progress"], serde_json::Value::Null);
}

#[test]
fn inspect_surfaces_only_the_safe_fatal_category() {
    let output = inspect(None, false, false, 3);
    let report = parse_report(&output);
    let member = report["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["node_id"] == 2)
        .unwrap();
    assert_eq!(member["diagnostic"], "fatal_storage_error");
    assert_eq!(member["status"]["running_issue"], "storage_error");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-error-detail"));
}
#[test]
fn unavailable_member_does_not_turn_two_samples_into_cap_or_quorum_proof() {
    let output = inspect(Some(3), false, false, 0);
    assert!(!output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], false);
    assert_eq!(
        report["retained_log_cap_agreement"],
        serde_json::Value::Null
    );
    assert_eq!(
        report["configured_log_cap_agreement"],
        serde_json::Value::Null
    );
    assert_eq!(report["members"][2]["diagnostic"], "unavailable");
    assert_eq!(report["replication_progress"], serde_json::Value::Null);
}
#[test]
fn mismatched_caps_and_wrong_identity_are_reported_without_raw_errors() {
    let output = inspect(None, true, false, 0);
    assert!(!output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], true);
    assert_eq!(report["retained_log_cap_agreement"], false);
    assert_eq!(report["configured_log_cap_agreement"], false);
    let output = inspect(None, false, false, 1);
    assert!(!output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], false);
    assert_eq!(report["members"][1]["diagnostic"], "invalid_status");
    assert_eq!(report["members"][1]["status"], serde_json::Value::Null);
    assert_eq!(report["replication_progress"], serde_json::Value::Null);
}

#[test]
fn stopped_raft_state_is_not_a_complete_sample() {
    let output = inspect(None, false, false, 2);
    assert!(!output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], false);
    assert_eq!(report["members"][1]["diagnostic"], "not_running");
    assert_eq!(report["replication_progress"], serde_json::Value::Null);
}

#[test]
fn all_voters_are_contacted_before_waiting_for_any_response() {
    let output = inspect_with_overlap_check(None, false, false, 0, true, false);
    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], true);
    let ids = report["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["node_id"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn missing_indexes_suppress_the_replication_progress_summary() {
    let output = inspect_with_overlap_check(None, false, false, 0, false, true);
    assert!(output.status.success());
    let report = parse_report(&output);
    assert_eq!(report["complete"], true);
    assert_eq!(report["all_initialized"], true);
    assert_eq!(report["replication_progress"], serde_json::Value::Null);
    for member in report["members"].as_array().unwrap() {
        assert!(member.get("committed_index_lag").is_none());
        assert!(member.get("applied_index_lag").is_none());
    }
}
