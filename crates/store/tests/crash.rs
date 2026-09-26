//! Acknowledged graph writes survive repeated forced writer termination.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use brainwire_core::ids::EdgeId;
use brainwire_store::db::{Database, Resources, StoreConfig};
use brainwire_store::graph::{check_invariants, find_by_entity_id, out_edges};

const ACK_TIMEOUT: Duration = Duration::from_secs(30);

struct Writer(Option<Child>);

impl Writer {
    fn stop(&mut self) {
        let child = self.0.as_mut().expect("writer already reaped");
        child.kill().expect("kill writer");
        let status = child.wait().expect("reap writer");
        self.0.take();
        assert_eq!(status.signal(), Some(9), "writer did not die by SIGKILL");
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn survives_five_kill_and_reopen_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let mut next = 0u64;
    let mut seen_ids = HashSet::new();

    for round in 0..5u64 {
        let mut writer = Writer(Some(
            Command::new(env!("CARGO_BIN_EXE_crash_writer"))
                .arg(dir.path())
                .arg(next.to_string())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn writer"),
        ));
        let stdout = writer.0.as_mut().unwrap().stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        let first = next;
        let count = 150 + round * 37;
        for expected in first..first + count {
            let line = receiver
                .recv_timeout(ACK_TIMEOUT)
                .unwrap_or_else(|e| panic!("round {round}: no acknowledgment for {expected}: {e}"))
                .unwrap_or_else(|e| panic!("round {round}: read acknowledgment: {e}"));
            let acknowledged: u64 = line.parse().expect("numeric acknowledgment");
            assert_eq!(
                acknowledged, expected,
                "round {round}: skipped acknowledgment"
            );
        }
        writer.stop();
        reader.join().expect("join acknowledgment reader");

        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).expect("reopen after SIGKILL");
        check_invariants(&db).unwrap_or_else(|e| panic!("round {round}: {e}"));
        let catalog = db.catalog().unwrap().expect("persisted ontology");
        let customer_type = catalog.node_type("Customer").unwrap().id;
        let company_type = catalog.node_type("Company").unwrap().id;
        let works_at = catalog.edge_type("WORKS_AT").unwrap().id;
        let txn = db.begin();

        let companies: Vec<_> = (0..100)
            .filter(|&i| i < first + count)
            .map(|i| {
                let ids = find_by_entity_id(&txn, &format!("k:{i}"), Some(company_type))
                    .expect("find company");
                assert_eq!(ids.len(), 1, "round {round}: company k:{i} missing");
                ids[0]
            })
            .collect();

        for i in 0..first + count {
            let ids = find_by_entity_id(&txn, &format!("c:{i}"), Some(customer_type))
                .expect("find customer");
            assert_eq!(
                ids.len(),
                1,
                "round {round}: acknowledged customer c:{i} missing"
            );
            let id = ids[0];
            if round == 4 {
                assert!(seen_ids.insert(id), "customer c:{i} reused a node id");
            }
            let edges = out_edges(&txn, id).expect("read WORKS_AT edge");
            assert_eq!(edges.len(), 1, "round {round}: customer c:{i} edge count");
            let edge = edges[0];
            assert_eq!(edge.edge_type, works_at, "round {round}: c:{i} edge type");
            assert_eq!(
                edge.other,
                companies[(i % 100) as usize],
                "round {round}: c:{i} company"
            );
            assert_eq!(edge.id, EdgeId::MERGED, "round {round}: c:{i} edge id");
        }
        if round == 4 {
            for company in companies {
                assert!(
                    seen_ids.insert(company),
                    "company reused a customer node id"
                );
            }
        }
        next = first + count;
    }
}
