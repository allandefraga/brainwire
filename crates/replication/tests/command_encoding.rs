use brainwire_replication::{Command, MAX_COMMAND_BYTES};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[test]
fn digest_preserves_compact_json_identity_for_every_command() {
    let mut graphs = BTreeMap::new();
    graphs.insert("z".into(), vec![0, 10, 255]);
    graphs.insert("á\"".into(), vec![255, 1]);
    let commands = [
        Command::Bootstrap {
            request_id: "boot-1".into(),
            graphs,
        },
        Command::CreateGraph {
            request_id: "create-1".into(),
            graph: "á\"".into(),
            schema: vec![0, 10, 255],
        },
        Command::Write {
            request_id: "write-1".into(),
            graph: "g".into(),
            expected_revision: 42,
            batch: vec![0, 10, 255],
        },
    ];
    for command in commands {
        let expected: [u8; 32] = Sha256::digest(serde_json::to_vec(&command).unwrap()).into();
        assert_eq!(command.digest().unwrap(), expected);
    }
}

#[test]
fn existing_request_digest_is_stable() {
    let command = Command::Write {
        request_id: "write-1".into(),
        graph: "g".into(),
        expected_revision: 42,
        batch: vec![0, 10, 255],
    };
    let digest = command.digest().unwrap();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        hex,
        "1b521060f64b3ae1affce79d75988a11f7595869e0fd8e31f2e118352dd92361"
    );
}

#[test]
fn command_limit_counts_actual_json_bytes_at_the_boundary() {
    let command = |length| Command::Write {
        request_id: "r".into(),
        graph: "g".repeat(length),
        expected_revision: 0,
        batch: vec![0, 10, 255],
    };
    let overhead = serde_json::to_vec(&command(0)).unwrap().len();
    for offset in [-1isize, 0, 1] {
        let target = MAX_COMMAND_BYTES.checked_add_signed(offset).unwrap();
        let value = command(target - overhead);
        let json = serde_json::to_vec(&value).unwrap();
        assert_eq!(json.len(), target);
        assert_eq!(value.validate().is_ok(), offset <= 0);
        let expected: [u8; 32] = Sha256::digest(&json).into();
        assert_eq!(value.digest().unwrap(), expected);
    }
}
