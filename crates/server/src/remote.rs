//! Bounded, read-only schema acquisition over Bolt.

use std::collections::HashSet;

use brainwire_bolt::packstream::PackValue;
use brainwire_tui::{OntologyEdge, OntologyNode, OntologySnapshot, OntologySummary};

use crate::probe::{success, BoltClient};

const QUERY: &str = "CALL db.schema() YIELD version, nodes, relationships, indexesReady RETURN version, nodes, relationships, indexesReady";
const FIELDS: [&str; 4] = ["version", "nodes", "relationships", "indexesReady"];
const MAX_WIRE: usize = 4 * 1024 * 1024;
const MAX_TYPES: usize = 4096;
const MAX_ENTRIES: usize = 65_536;
const MAX_TEXT: usize = 1024 * 1024;

pub fn schema(address: &str, database: &str, timeout_ms: u64) -> Result<OntologySnapshot, String> {
    if !(1..=10_000).contains(&timeout_ms) {
        return Err("--timeout-ms must be between 1 and 10000".into());
    }
    if !crate::databases::valid_name(database) {
        return Err(
            "database name must contain 1..128 ASCII letters, digits, underscores or hyphens"
                .into(),
        );
    }
    let mut client = BoltClient::connect(address, timeout_ms, MAX_WIRE)?;
    client.send(
        0x10,
        vec![
            PackValue::String(QUERY.into()),
            PackValue::Map(vec![]),
            PackValue::Map(vec![("db".into(), PackValue::String(database.into()))]),
        ],
    )?;
    let metadata = success(client.receive()?)?;
    let expected = PackValue::List(
        FIELDS
            .iter()
            .map(|s| PackValue::String((*s).into()))
            .collect(),
    );
    if metadata.get("fields") != Some(&expected) {
        return Err("schema RUN returned unexpected fields".into());
    }
    client.send(
        0x3f,
        vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
    )?;
    let snapshot = decode_record(client.receive()?)?;
    let metadata = success(client.receive()?)?;
    match metadata.get("has_more") {
        None | Some(PackValue::Bool(false)) => {}
        _ => return Err("schema result is incomplete (has_more)".into()),
    }
    client.finish()?;
    Ok(snapshot)
}

#[derive(Default)]
struct Limits {
    entries: usize,
    text: usize,
}

impl Limits {
    fn list<'a>(&mut self, value: &'a PackValue) -> Result<&'a [PackValue], String> {
        let PackValue::List(items) = value else {
            return Err("expected schema list".into());
        };
        self.entries += items.len();
        if self.entries > MAX_ENTRIES {
            return Err("schema exceeds list entry limit".into());
        }
        Ok(items)
    }

    fn text(&mut self, value: &PackValue) -> Result<String, String> {
        let PackValue::String(text) = value else {
            return Err("expected schema string".into());
        };
        self.text += text
            .chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().count()
                } else {
                    c.len_utf8()
                }
            })
            .sum::<usize>();
        if self.text > MAX_TEXT {
            return Err("schema exceeds expanded text limit".into());
        }
        Ok(text.clone())
    }

    fn strings(&mut self, value: &PackValue) -> Result<Vec<String>, String> {
        let items = self.list(value)?;
        let mut unique = HashSet::new();
        items
            .iter()
            .map(|item| {
                let text = self.text(item)?;
                if !unique.insert(text.clone()) {
                    return Err("duplicate schema field name".into());
                }
                Ok(text)
            })
            .collect()
    }
}

fn map<'a>(
    value: &'a PackValue,
    keys: &[&str],
    optional: &[&str],
) -> Result<Vec<&'a PackValue>, String> {
    let PackValue::Map(entries) = value else {
        return Err("expected schema map".into());
    };
    if entries.len() < keys.len() || entries.len() > keys.len() + optional.len() {
        return Err("unexpected schema map fields".into());
    }
    let mut names = HashSet::new();
    for (name, _) in entries {
        if !keys.contains(&name.as_str()) && !optional.contains(&name.as_str()) {
            return Err("unexpected schema map fields".into());
        }
        if !names.insert(name.as_str()) {
            return Err("duplicate schema map field".into());
        }
    }
    keys.iter()
        .map(|key| {
            let mut values = entries.iter().filter(|(name, _)| name == key);
            let value = values.next().ok_or("missing schema map field")?;
            if values.next().is_some() {
                return Err("duplicate schema map field".into());
            }
            Ok(&value.1)
        })
        .collect()
}

fn optional<'a>(value: &'a PackValue, key: &str) -> Option<&'a PackValue> {
    let PackValue::Map(entries) = value else {
        return None;
    };
    entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn description(limits: &mut Limits, value: &PackValue) -> Result<(), String> {
    if let Some(value) = optional(value, "description") {
        let text = limits.text(value)?;
        if text.len() > 4096 || text.contains('\0') {
            return Err("invalid schema description".into());
        }
    }
    Ok(())
}

fn decode_record(value: PackValue) -> Result<OntologySnapshot, String> {
    let PackValue::Struct { tag: 0x71, fields } = value else {
        return Err("expected one schema RECORD".into());
    };
    if fields.len() != 1 {
        return Err("unexpected schema RECORD fields".into());
    }
    let PackValue::List(row) = &fields[0] else {
        return Err("expected schema row list".into());
    };
    if row.len() != 4 {
        return Err("schema row must contain four fields".into());
    }
    let PackValue::Int(version) = row[0] else {
        return Err("invalid schema version".into());
    };
    let version = u32::try_from(version).map_err(|_| "invalid schema version")?;
    let PackValue::Bool(indexes_ready) = row[3] else {
        return Err("invalid schema index readiness".into());
    };
    let mut limits = Limits::default();
    let node_values = limits.list(&row[1])?;
    if node_values.len() > MAX_TYPES {
        return Err("schema exceeds node limit".into());
    }
    let mut labels = HashSet::new();
    let mut nodes = Vec::new();
    for value in node_values {
        let fields = map(
            value,
            &["label", "properties", "indexedFields"],
            &["description"],
        )?;
        description(&mut limits, value)?;
        let label = limits.text(fields[0])?;
        if !labels.insert(label.clone()) {
            return Err("duplicate schema node label".into());
        }
        let properties = limits.strings(fields[1])?;
        let indexed_fields = limits.strings(fields[2])?;
        nodes.push(OntologyNode {
            label,
            properties,
            indexed_fields,
        });
    }
    let edge_values = limits.list(&row[2])?;
    if edge_values.len() > MAX_TYPES {
        return Err("schema exceeds relationship signature limit".into());
    }
    let mut signatures = HashSet::new();
    let mut edges = Vec::new();
    for value in edge_values {
        let fields = map(
            value,
            &["name", "source", "target", "properties"],
            &["description", "mergeKeyFields"],
        )?;
        description(&mut limits, value)?;
        let name = limits.text(fields[0])?;
        let source = limits.text(fields[1])?;
        let target = limits.text(fields[2])?;
        if !signatures.insert((name.clone(), source.clone(), target.clone())) {
            return Err("duplicate schema relationship signature".into());
        }
        if !labels.contains(&source) || !labels.contains(&target) {
            return Err("schema relationship references an unknown endpoint".into());
        }
        let properties = limits.strings(fields[3])?;
        if let Some(value) = optional(value, "mergeKeyFields") {
            let keys = limits.strings(value)?;
            if keys.is_empty()
                || keys.iter().any(|key| {
                    key.is_empty()
                        || key.len() > 128
                        || !properties.contains(key)
                        || key.to_ascii_lowercase().starts_with("acl_")
                        || !key.bytes().enumerate().all(|(i, b)| {
                            b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                        })
                })
            {
                return Err("invalid schema relationship merge keys".into());
            }
        }
        edges.push(OntologyEdge {
            name,
            source,
            target,
            properties,
        });
    }
    Ok(OntologySnapshot {
        ontology: OntologySummary { nodes, edges },
        version,
        indexes_ready,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use brainwire_bolt::{
        chunk::encode_message,
        packstream::{pack, unpack},
    };
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        time::{Duration, Instant},
    };

    pub(super) fn message(tag: u8, value: PackValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        pack(
            &PackValue::Struct {
                tag,
                fields: vec![value],
            },
            &mut bytes,
        );
        encode_message(&bytes)
    }

    pub(super) fn success_bytes(metadata: Vec<(String, PackValue)>) -> Vec<u8> {
        message(0x70, PackValue::Map(metadata))
    }

    pub(super) fn read_request(stream: &mut TcpStream) -> PackValue {
        let mut body = Vec::new();
        loop {
            let mut size = [0; 2];
            stream.read_exact(&mut size).unwrap();
            let size = u16::from_be_bytes(size) as usize;
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            stream.read_exact(&mut body[start..]).unwrap();
        }
        unpack(&mut body.as_slice()).unwrap()
    }

    fn fake(responses: Vec<u8>, fields: PackValue) -> (String, std::thread::JoinHandle<()>) {
        fake_with_delay(responses, fields, None)
    }

    fn fake_with_delay(
        responses: Vec<u8>,
        fields: PackValue,
        split: Option<usize>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut handshake = [0; 20];
            stream.read_exact(&mut handshake).unwrap();
            stream.write_all(&[0, 0, 4, 5]).unwrap();
            for _ in 0..2 {
                read_request(&mut stream);
                stream.write_all(&success_bytes(vec![])).unwrap();
            }
            let PackValue::Struct {
                tag: 0x10,
                fields: run,
            } = read_request(&mut stream)
            else {
                panic!("expected RUN")
            };
            assert_eq!(run[0], PackValue::String(QUERY.into()));
            assert_eq!(
                run[2].get("db"),
                Some(&PackValue::String("selected".into()))
            );
            stream
                .write_all(&success_bytes(vec![("fields".into(), fields)]))
                .unwrap();
            // A malformed RUN field list closes the client before PULL.
            let mut byte = [0];
            if stream.peek(&mut byte).unwrap_or(0) == 0 {
                return;
            }
            let PackValue::Struct { tag: 0x3f, .. } = read_request(&mut stream) else {
                panic!("expected PULL")
            };
            if let Some(split) = split {
                stream.write_all(&responses[..split]).unwrap();
                std::thread::sleep(Duration::from_millis(40));
                let _ = stream.write_all(&responses[split..]);
            } else {
                let _ = stream.write_all(&responses);
            }
            if stream.peek(&mut byte).unwrap_or(0) > 0 {
                let PackValue::Struct { tag: 0x02, .. } = read_request(&mut stream) else {
                    panic!("expected GOODBYE")
                };
            }
        });
        (address, thread)
    }

    fn fields() -> PackValue {
        PackValue::List(
            FIELDS
                .iter()
                .map(|s| PackValue::String((*s).into()))
                .collect(),
        )
    }

    fn row() -> PackValue {
        PackValue::List(vec![
            PackValue::Int(7),
            PackValue::List(vec![]),
            PackValue::List(vec![]),
            PackValue::Bool(true),
        ])
    }

    #[test]
    fn schema_accepts_one_complete_row_and_rejects_bad_or_partial_results() {
        let mut good = message(0x71, row());
        good.extend(success_bytes(vec![]));
        let (address, server) = fake(good.clone(), fields());
        let result = schema(&address, "selected", 1000).unwrap();
        assert_eq!(result.version, 7);
        assert!(result.indexes_ready);
        server.join().unwrap();

        let mut more = message(0x71, row());
        more.extend(success_bytes(vec![(
            "has_more".into(),
            PackValue::Bool(true),
        )]));
        let mut duplicate = message(0x71, row());
        duplicate.extend(message(0x71, row()));
        duplicate.extend(success_bytes(vec![]));
        let mut trailing = good.clone();
        trailing.extend(message(0x71, row()));
        for bytes in [
            message(0x71, row()),
            more,
            duplicate,
            trailing,
            vec![0, 2, 0xb1, 0x71, 0, 0],
            vec![0; 34],
        ] {
            let (address, server) = fake(bytes, fields());
            assert!(schema(&address, "selected", 1000).is_err());
            server.join().unwrap();
        }
        let mut delayed = good.clone();
        delayed.extend(message(0x71, row()));
        let (address, server) = fake_with_delay(delayed, fields(), Some(good.len()));
        assert!(schema(&address, "selected", 1000)
            .unwrap_err()
            .contains("trailing"));
        server.join().unwrap();
        let (address, server) = fake(message(0x71, PackValue::Bytes(vec![0; MAX_WIRE])), fields());
        assert!(schema(&address, "selected", 1000)
            .unwrap_err()
            .contains("wire limit"));
        server.join().unwrap();
        let (address, server) = fake(good, PackValue::List(vec![]));
        assert!(schema(&address, "selected", 1000)
            .unwrap_err()
            .contains("unexpected fields"));
        server.join().unwrap();
    }

    #[test]
    fn schema_rejects_shapes_duplicates_endpoints_and_limits() {
        assert!(decode_record(PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(vec![])]
        })
        .is_err());
        let node = PackValue::Map(vec![
            ("label".into(), PackValue::String("A".into())),
            ("properties".into(), PackValue::List(vec![])),
            ("indexedFields".into(), PackValue::List(vec![])),
        ]);
        let relationship = PackValue::Map(vec![
            ("name".into(), PackValue::String("R".into())),
            ("source".into(), PackValue::String("A".into())),
            ("target".into(), PackValue::String("missing".into())),
            ("properties".into(), PackValue::List(vec![])),
        ]);
        let mut valid_relationship = relationship.clone();
        if let PackValue::Map(fields) = &mut valid_relationship {
            fields[2].1 = PackValue::String("A".into());
        }
        let valid = decode_record(message_value(PackValue::List(vec![
            PackValue::Int(12),
            PackValue::List(vec![node.clone()]),
            PackValue::List(vec![valid_relationship.clone()]),
            PackValue::Bool(false),
        ])))
        .unwrap();
        assert_eq!(valid.ontology.nodes[0].label, "A");
        assert_eq!(valid.ontology.edges[0].source, "A");
        assert_eq!(valid.ontology.edges[0].name, "R");
        assert!(!valid.indexes_ready);
        for (nodes, edges) in [
            (
                vec![node.clone()],
                vec![valid_relationship.clone(), valid_relationship],
            ),
            (vec![node.clone(), node.clone()], vec![]),
            (vec![node.clone(); MAX_TYPES + 1], vec![]),
            (vec![node], vec![relationship]),
        ] {
            let record = message_value(PackValue::List(vec![
                PackValue::Int(0),
                PackValue::List(nodes),
                PackValue::List(edges),
                PackValue::Bool(false),
            ]));
            assert!(decode_record(record).is_err());
        }
        assert!(Limits::default()
            .strings(&PackValue::List(vec![
                PackValue::String("x".into());
                MAX_ENTRIES + 1
            ]))
            .is_err());
        assert!(Limits::default()
            .text(&PackValue::String("\0".repeat(MAX_TEXT / 2)))
            .is_err());
    }

    fn message_value(row: PackValue) -> PackValue {
        PackValue::Struct {
            tag: 0x71,
            fields: vec![row],
        }
    }

    #[test]
    fn schema_accepts_keyed_metadata_and_descriptions_but_rejects_bad_optional_fields() {
        let strings = |names: &[&str]| {
            PackValue::List(
                names
                    .iter()
                    .map(|s| PackValue::String((*s).into()))
                    .collect(),
            )
        };
        let node = PackValue::Map(vec![
            ("label".into(), PackValue::String("Ticket".into())),
            ("properties".into(), strings(&["entity_id"])),
            ("indexedFields".into(), strings(&[])),
            ("description".into(), PackValue::String("A ticket".into())),
        ]);
        let edge = PackValue::Map(vec![
            ("name".into(), PackValue::String("EVENT".into())),
            ("source".into(), PackValue::String("Ticket".into())),
            ("target".into(), PackValue::String("Ticket".into())),
            ("properties".into(), strings(&["event_key", "summary"])),
            (
                "description".into(),
                PackValue::String("A milestone".into()),
            ),
            ("mergeKeyFields".into(), strings(&["event_key"])),
        ]);
        let decode = |node, edge| {
            decode_record(message_value(PackValue::List(vec![
                PackValue::Int(1),
                PackValue::List(vec![node]),
                PackValue::List(vec![edge]),
                PackValue::Bool(true),
            ])))
        };
        let snapshot = decode(node.clone(), edge.clone()).unwrap();
        assert_eq!(
            snapshot.ontology.edges[0].properties,
            ["event_key", "summary"]
        );
        for (name, value) in [
            ("mergeKeyFields", PackValue::Bool(true)),
            ("mergeKeyFields", strings(&[])),
            ("mergeKeyFields", strings(&["event_key", "event_key"])),
            ("mergeKeyFields", strings(&["missing"])),
            ("description", PackValue::Int(1)),
            ("description", PackValue::String("x".repeat(4097))),
        ] {
            let mut bad = edge.clone();
            let PackValue::Map(fields) = &mut bad else {
                unreachable!()
            };
            fields.iter_mut().find(|(key, _)| key == name).unwrap().1 = value;
            assert!(decode(node.clone(), bad).is_err(), "{name}");
        }
        for (name, value) in [
            ("unexpected", PackValue::Bool(false)),
            ("mergeKeyFields", strings(&["event_key"])),
        ] {
            let mut bad = edge.clone();
            let PackValue::Map(fields) = &mut bad else {
                unreachable!()
            };
            fields.push((name.into(), value));
            assert!(decode(node.clone(), bad).is_err());
        }
    }

    #[test]
    fn schema_preserves_catalog_names_and_indexes_on_extra_properties() {
        // Catalog::apply permits empty names and indexes on unpromoted properties.
        let strings = |names: &[&str]| {
            PackValue::List(
                names
                    .iter()
                    .map(|s| PackValue::String((*s).into()))
                    .collect(),
            )
        };
        let node = PackValue::Map(vec![
            ("label".into(), PackValue::String("".into())),
            ("properties".into(), strings(&["entity_id", ""])),
            ("indexedFields".into(), strings(&["extra_only"])),
        ]);
        let edge = PackValue::Map(vec![
            ("name".into(), PackValue::String("".into())),
            ("source".into(), PackValue::String("".into())),
            ("target".into(), PackValue::String("".into())),
            ("properties".into(), strings(&[""])),
        ]);
        let snapshot = decode_record(message_value(PackValue::List(vec![
            PackValue::Int(1),
            PackValue::List(vec![node]),
            PackValue::List(vec![edge]),
            PackValue::Bool(true),
        ])))
        .unwrap();
        assert_eq!(snapshot.ontology.nodes[0].properties, ["entity_id", ""]);
        assert_eq!(snapshot.ontology.nodes[0].indexed_fields, ["extra_only"]);
        assert_eq!(snapshot.ontology.edges[0].source, "");
    }

    #[test]
    fn handshake_slow_drip_cannot_extend_absolute_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut handshake = [0; 20];
            stream.read_exact(&mut handshake).unwrap();
            for byte in [0, 0, 4, 5] {
                std::thread::sleep(Duration::from_millis(45));
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
            }
        });
        let start = Instant::now();
        assert!(schema(&address, "selected", 100)
            .unwrap_err()
            .contains("deadline"));
        assert!(start.elapsed() < Duration::from_millis(250));
        server.join().unwrap();
    }
}

/// Read one outcome through an isolated autocommit CALL, scoped by server auth.
pub fn request_outcome(
    address: &str,
    database: &str,
    timeout_ms: u64,
    seed: &str,
) -> Result<brainwire_tui::RequestOutcome, String> {
    if seed.is_empty() || seed.len() > 128 || !seed.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("request ID must be 1..128 printable ASCII characters without spaces".into());
    }
    if !(1..=10_000).contains(&timeout_ms) || !crate::databases::valid_name(database) {
        return Err("invalid request lookup timeout or database".into());
    }
    let mut client = BoltClient::connect(address, timeout_ms, 16 * 1024)?;
    client.send(
        0x10,
        vec![
            PackValue::String("CALL db.requestStatus($seed)".into()),
            PackValue::Map(vec![("seed".into(), PackValue::String(seed.into()))]),
            PackValue::Map(vec![("db".into(), PackValue::String(database.into()))]),
        ],
    )?;
    let metadata = success(client.receive()?)?;
    let fields = PackValue::List(
        ["status", "requestId", "incarnation", "sequence"]
            .map(|value| PackValue::String(value.into()))
            .to_vec(),
    );
    if metadata.get("fields") != Some(&fields) {
        return Err("request outcome RUN returned unexpected fields".into());
    }
    client.send(
        0x3f,
        vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
    )?;
    let outcome = decode_request_outcome(client.receive()?, seed)?;
    let metadata = success(client.receive()?)?;
    if !matches!(
        metadata.get("has_more"),
        None | Some(PackValue::Bool(false))
    ) {
        return Err("request outcome is incomplete".into());
    }
    client.finish()?;
    Ok(outcome)
}

fn decode_request_outcome(
    value: PackValue,
    seed: &str,
) -> Result<brainwire_tui::RequestOutcome, String> {
    let invalid = || "invalid request outcome response".to_string();
    if matches!(&value, PackValue::Struct { tag: 0x7f, .. }) {
        success(value)?;
        return Err(invalid());
    }
    let PackValue::Struct { tag: 0x71, fields } = value else {
        return Err(invalid());
    };
    let [PackValue::List(values)] = fields.as_slice() else {
        return Err(invalid());
    };
    let [PackValue::String(status), PackValue::String(request_id), PackValue::String(incarnation), sequence] =
        values.as_slice()
    else {
        return Err(invalid());
    };
    if request_id != seed || uuid::Uuid::parse_str(incarnation).is_err() {
        return Err(invalid());
    }
    let (committed, sequence) = match (status.as_str(), sequence) {
        ("committed", PackValue::String(sequence))
            if !sequence.is_empty()
                && sequence.len() <= 20
                && sequence.bytes().all(|b| b.is_ascii_digit())
                && sequence.parse::<u64>().is_ok() =>
        {
            (true, Some(sequence.clone()))
        }
        ("not_recorded", PackValue::Null) => (false, None),
        _ => return Err(invalid()),
    };
    Ok(brainwire_tui::RequestOutcome {
        committed,
        request_id: request_id.clone(),
        incarnation: incarnation.clone(),
        sequence,
    })
}

#[cfg(test)]
mod request_outcome_tests {
    use super::*;
    fn record(status: &str, id: &str, incarnation: &str, sequence: PackValue) -> PackValue {
        PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(vec![
                PackValue::String(status.into()),
                PackValue::String(id.into()),
                PackValue::String(incarnation.into()),
                sequence,
            ])],
        }
    }
    #[test]
    fn validates_committed_and_inconclusive_outcomes() {
        let uuid = "12345678-1234-4234-8234-123456789abc";
        let found = decode_request_outcome(
            record("committed", "a'\\", uuid, PackValue::String("42".into())),
            "a'\\",
        )
        .unwrap();
        assert!(found.committed);
        assert_eq!(found.sequence.as_deref(), Some("42"));
        assert!(
            !decode_request_outcome(record("not_recorded", "a", uuid, PackValue::Null), "a")
                .unwrap()
                .committed
        );
        for value in [
            record("unknown", "a", uuid, PackValue::Null),
            record("committed", "a", uuid, PackValue::Int(1)),
            record(
                "committed",
                "a",
                uuid,
                PackValue::String("18446744073709551616".into()),
            ),
            record("committed", "a", uuid, PackValue::String("-1".into())),
            record("not_recorded", "a", uuid, PackValue::String("1".into())),
            record("not_recorded", "b", uuid, PackValue::Null),
            record("not_recorded", "a", "bad", PackValue::Null),
        ] {
            assert!(decode_request_outcome(value, "a").is_err());
        }
    }
    #[test]
    fn invalid_seed_refuses_before_network() {
        for seed in ["", "has space", "new\nline", "é"] {
            assert!(request_outcome("invalid-address", "default", 100, seed)
                .unwrap_err()
                .contains("request ID"));
        }
        assert!(
            request_outcome("invalid-address", "default", 100, &"a".repeat(129))
                .unwrap_err()
                .contains("request ID")
        );
    }
}

#[cfg(test)]
mod request_wire_tests {
    use super::tests::{message, read_request, success_bytes};
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };
    #[test]
    fn request_lookup_is_one_parameterized_autocommit_and_rejects_extra_or_failed_results() {
        for response in ["committed", "not_recorded", "extra", "has_more", "failure"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let seed = "quote'\\;request";
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut handshake = [0; 20];
                stream.read_exact(&mut handshake).unwrap();
                stream.write_all(&[0, 0, 4, 5]).unwrap();
                for expected in [0x01, 0x6a] {
                    assert!(
                        matches!(read_request(&mut stream), PackValue::Struct {tag,..} if tag==expected)
                    );
                    stream.write_all(&success_bytes(vec![])).unwrap();
                }
                let PackValue::Struct { tag: 0x10, fields } = read_request(&mut stream) else {
                    panic!("RUN expected, no BEGIN");
                };
                assert_eq!(
                    fields[0],
                    PackValue::String("CALL db.requestStatus($seed)".into())
                );
                assert_eq!(
                    fields[1],
                    PackValue::Map(vec![("seed".into(), PackValue::String(seed.into()))])
                );
                assert_eq!(
                    fields[2].get("db"),
                    Some(&PackValue::String("selected".into()))
                );
                let fields = PackValue::List(
                    ["status", "requestId", "incarnation", "sequence"]
                        .map(|s| PackValue::String(s.into()))
                        .to_vec(),
                );
                stream
                    .write_all(&success_bytes(vec![("fields".into(), fields)]))
                    .unwrap();
                assert!(matches!(
                    read_request(&mut stream),
                    PackValue::Struct { tag: 0x3f, .. }
                ));
                if response == "failure" {
                    stream
                        .write_all(&message(
                            0x7f,
                            PackValue::Map(vec![
                                (
                                    "code".into(),
                                    PackValue::String("Neo.ClientError.Request.Invalid".into()),
                                ),
                                (
                                    "message".into(),
                                    PackValue::String("ledger disabled".into()),
                                ),
                            ]),
                        ))
                        .unwrap();
                    return;
                }
                let record = message(
                    0x71,
                    PackValue::List(vec![
                        PackValue::String(
                            if response == "committed" {
                                "committed"
                            } else {
                                "not_recorded"
                            }
                            .into(),
                        ),
                        PackValue::String(seed.into()),
                        PackValue::String("12345678-1234-4234-8234-123456789abc".into()),
                        if response == "committed" {
                            PackValue::String("42".into())
                        } else {
                            PackValue::Null
                        },
                    ]),
                );
                let mut bytes = record.clone();
                if response == "extra" {
                    bytes.extend(record);
                }
                bytes.extend(success_bytes(if response == "has_more" {
                    vec![("has_more".into(), PackValue::Bool(true))]
                } else {
                    vec![]
                }));
                stream.write_all(&bytes).unwrap();
                if matches!(response, "committed" | "not_recorded") {
                    // Finish is GOODBYE, never a transaction COMMIT.
                    assert!(matches!(
                        read_request(&mut stream),
                        PackValue::Struct { tag: 0x02, .. }
                    ));
                }
            });
            let result = request_outcome(&address, "selected", 2000, seed);
            match response {
                "committed" => {
                    let r = result.unwrap();
                    assert!(r.committed);
                    assert_eq!(r.request_id, seed);
                }
                "not_recorded" => assert!(!result.unwrap().committed),
                "failure" => assert!(result.unwrap_err().contains("ledger disabled")),
                _ => assert!(result.is_err()),
            }
            server.join().unwrap();
        }
    }
}
