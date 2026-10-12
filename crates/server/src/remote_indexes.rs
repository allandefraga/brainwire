//! Bounded, read-only configured index snapshot over Bolt.
use crate::probe::{success, BoltClient};
use brainwire_bolt::packstream::PackValue;
use brainwire_tui::{IndexSummary, IndexesSnapshot};

const QUERY: &str = "CALL db.indexes() YIELD label, property, state RETURN label, property, state";
const FIELDS: [&str; 3] = ["label", "property", "state"];
const MAX_WIRE: usize = 128 * 1024;
const MAX_ROWS: usize = 200;

pub fn indexes(address: &str, graph: &str, timeout_ms: u64) -> Result<IndexesSnapshot, String> {
    if !(1..=10_000).contains(&timeout_ms) {
        return Err("--timeout-ms must be between 1 and 10000".into());
    }
    if !crate::databases::valid_name(graph) {
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
            PackValue::Map(vec![("db".into(), PackValue::String(graph.into()))]),
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
        return Err("indexes RUN returned unexpected fields".into());
    }
    client.send(
        0x3f,
        vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
    )?;
    let mut snapshot = IndexesSnapshot::default();
    loop {
        let response = client.receive()?;
        if matches!(&response, PackValue::Struct { tag: 0x71, .. }) {
            if snapshot.indexes.len() >= MAX_ROWS {
                return Err("index snapshot exceeds 200 rows".into());
            }
            let index = decode_record(response)?;
            if snapshot
                .indexes
                .iter()
                .any(|old| old.label == index.label && old.property == index.property)
            {
                return Err("duplicate index declaration".into());
            }
            snapshot.indexes.push(index);
        } else {
            let metadata = success(response)?;
            match metadata.get("has_more") {
                None | Some(PackValue::Bool(false)) => {}
                _ => return Err("index result is incomplete (has_more)".into()),
            }
            break;
        }
    }
    client.finish()?;
    Ok(snapshot)
}

fn decode_record(value: PackValue) -> Result<IndexSummary, String> {
    let PackValue::Struct { tag: 0x71, fields } = value else {
        return Err("expected index RECORD".into());
    };
    let [PackValue::List(row)] = fields.as_slice() else {
        return Err("expected one index row list".into());
    };
    let [PackValue::String(label), PackValue::String(property), PackValue::String(state)] =
        row.as_slice()
    else {
        return Err("index row must contain three strings".into());
    };
    if !matches!(state.as_str(), "scalar_equality_ready" | "declared_only") {
        return Err("unknown index state".into());
    }
    Ok(IndexSummary {
        label: label.clone(),
        property: property.clone(),
        state: state.clone(),
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
        time::Duration,
    };
    fn message(tag: u8, value: PackValue) -> Vec<u8> {
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

    fn success_bytes(metadata: Vec<(String, PackValue)>) -> Vec<u8> {
        message(0x70, PackValue::Map(metadata))
    }

    fn read_request(stream: &mut TcpStream) -> PackValue {
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
    fn row(label: &str, state: &str) -> PackValue {
        PackValue::List(vec![
            PackValue::String(label.into()),
            PackValue::String("name".into()),
            PackValue::String(state.into()),
        ])
    }
    fn record(row: PackValue) -> PackValue {
        PackValue::Struct {
            tag: 0x71,
            fields: vec![row],
        }
    }
    #[test]
    fn strict_row_shape_and_states_preserve_catalog_strings() {
        let decoded = decode_record(record(row("\x1b[31m", "declared_only"))).unwrap();
        assert_eq!(decoded.label, "\x1b[31m");
        assert_eq!(
            decode_record(record(row("", "scalar_equality_ready")))
                .unwrap()
                .label,
            ""
        );
        for bad in [
            row("A", "unknown"),
            PackValue::List(vec![]),
            PackValue::List(vec![PackValue::Null; 3]),
            PackValue::String("A".into()),
        ] {
            assert!(decode_record(record(bad)).is_err());
        }
        assert!(decode_record(PackValue::Struct {
            tag: 0x71,
            fields: vec![row("A", "declared_only"), PackValue::Null]
        })
        .is_err());
    }
    #[test]
    fn accepts_empty_and_200_rows_rejects_excess_partial_wrong_fields_and_wire() {
        for count in [0, 200, 201] {
            let mut bytes = Vec::new();
            for i in 0..count {
                bytes.extend(message(
                    0x71,
                    row(&format!("Type{i}"), "scalar_equality_ready"),
                ));
            }
            bytes.extend(success_bytes(vec![]));
            let (address, server) = fake(bytes, fields());
            let result = indexes(&address, "selected", 1000);
            if count <= 200 {
                assert_eq!(result.unwrap().indexes.len(), count);
            } else {
                assert!(result.unwrap_err().contains("200 rows"));
            }
            server.join().unwrap();
        }
        let mut good = message(0x71, row("A", "declared_only"));
        good.extend(success_bytes(vec![]));
        let mut more = message(0x71, row("A", "declared_only"));
        more.extend(success_bytes(vec![(
            "has_more".into(),
            PackValue::Bool(true),
        )]));
        let mut duplicate = message(0x71, row("A", "declared_only"));
        duplicate.extend(good.clone());
        for bytes in [
            message(0x71, row("A", "declared_only")),
            more,
            duplicate,
            message(0x71, row(&"x".repeat(MAX_WIRE), "declared_only")),
            message(0x7f, PackValue::Map(vec![])),
        ] {
            let (address, server) = fake(bytes, fields());
            assert!(indexes(&address, "selected", 1000).is_err());
            server.join().unwrap();
        }
        let (address, server) = fake(good.clone(), PackValue::List(vec![]));
        assert!(indexes(&address, "selected", 1000)
            .unwrap_err()
            .contains("unexpected fields"));
        server.join().unwrap();
        let mut trailing = good.clone();
        trailing.extend(message(0x71, row("B", "declared_only")));
        let (address, server) = fake_with_delay(trailing, fields(), Some(good.len()));
        assert!(indexes(&address, "selected", 1000)
            .unwrap_err()
            .contains("trailing"));
        server.join().unwrap();
    }
    #[test]
    fn invalid_graph_and_timeout_fail_before_connect() {
        assert!(indexes("127.0.0.1:1", "bad/name", 100)
            .unwrap_err()
            .contains("database name"));
        for timeout in [0, 10001] {
            assert!(indexes("127.0.0.1:1", "graph", timeout)
                .unwrap_err()
                .contains("timeout"));
        }
    }
}
