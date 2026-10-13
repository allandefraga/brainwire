//! Bounded typed outgoing-neighborhood acquisition in one read transaction.
use crate::probe::{success, BoltClient};
use brainwire_bolt::packstream::PackValue;
use brainwire_tui::GraphSnapshot;
use std::time::{Duration, Instant};

const MAX_WIRE: usize = 128 * 1024;
const MAX_ROWS: usize = 21;

fn identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

pub fn graph(
    address: &str,
    database: &str,
    timeout_ms: u64,
    node_label: &str,
    entity_id: &str,
    edge_type: &str,
    target_label: &str,
) -> Result<GraphSnapshot, String> {
    if !(1..=10_000).contains(&timeout_ms) {
        return Err("--timeout-ms must be between 1 and 10000".into());
    }
    if !crate::databases::valid_name(database) {
        return Err("invalid database name".into());
    }
    if ![node_label, edge_type, target_label]
        .into_iter()
        .all(identifier)
    {
        return Err("graph type identifiers require 1..128 ASCII letters, digits or underscores, starting with a letter or underscore".into());
    }
    if !(1..=1024).contains(&entity_id.len()) {
        return Err("entity ID requires 1..1024 bytes".into());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let schema = crate::remote::schema(address, database, timeout_ms)?;
    if !schema
        .ontology
        .edges
        .iter()
        .any(|e| e.name == edge_type && e.source == node_label && e.target == target_label)
    {
        return Err("requested edge signature is absent from the server ontology".into());
    }
    // Floor remaining milliseconds so opening the second connection cannot extend the total budget.
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or("graph snapshot deadline exceeded")?
        .as_millis();
    let remaining = u64::try_from(remaining).map_err(|_| "graph snapshot deadline exceeded")?;
    if remaining == 0 {
        return Err("graph snapshot deadline exceeded".into());
    }
    let mut client = BoltClient::connect(address, remaining, MAX_WIRE)?;
    client.send(
        0x11,
        vec![PackValue::Map(vec![
            ("db".into(), PackValue::String(database.into())),
            ("mode".into(), PackValue::String("r".into())),
        ])],
    )?;
    success(client.receive()?)?;
    let exists = rows(
        &mut client,
        &format!(
            "MATCH (n:{node_label} {{entity_id:$id}}) RETURN n.entity_id AS entity_id LIMIT 1"
        ),
        entity_id,
        1,
    )?;
    if exists.first().is_some_and(|id| id != entity_id) {
        return Err("graph existence result returned a different entity ID".into());
    }
    let mut neighbors = rows(&mut client,
        &format!("MATCH (n:{node_label} {{entity_id:$id}})-[:{edge_type}]->(m:{target_label}) RETURN m.entity_id AS entity_id LIMIT 21"),
        entity_id, MAX_ROWS)?;
    if exists.is_empty() && !neighbors.is_empty() {
        return Err("graph neighborhood returned rows for a missing source".into());
    }
    client.send(0x12, vec![])?;
    success(client.receive()?)?;
    client.finish()?;
    if Instant::now() >= deadline {
        return Err("graph snapshot deadline exceeded".into());
    }
    let has_more = neighbors.len() == MAX_ROWS;
    neighbors.truncate(20);
    Ok(GraphSnapshot {
        node_label: node_label.into(),
        entity_id: entity_id.into(),
        edge_type: edge_type.into(),
        target_label: target_label.into(),
        found: !exists.is_empty(),
        neighbors,
        has_more,
    })
}

fn rows(client: &mut BoltClient, query: &str, id: &str, cap: usize) -> Result<Vec<String>, String> {
    client.send(
        0x10,
        vec![
            PackValue::String(query.into()),
            PackValue::Map(vec![("id".into(), PackValue::String(id.into()))]),
            PackValue::Map(vec![]),
        ],
    )?;
    let metadata = success(client.receive()?)?;
    if metadata.get("fields")
        != Some(&PackValue::List(vec![PackValue::String(
            "entity_id".into(),
        )]))
    {
        return Err("graph RUN returned unexpected fields".into());
    }
    client.send(
        0x3f,
        vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
    )?;
    let mut result = Vec::new();
    loop {
        let response = client.receive()?;
        if matches!(&response, PackValue::Struct { tag: 0x71, .. }) {
            if result.len() >= cap {
                return Err("graph result exceeds its row limit".into());
            }
            result.push(decode_record(response)?);
        } else {
            let metadata = success(response)?;
            if !matches!(
                metadata.get("has_more"),
                None | Some(PackValue::Bool(false))
            ) {
                return Err("graph result is incomplete (has_more)".into());
            }
            break;
        }
    }
    Ok(result)
}

fn decode_record(value: PackValue) -> Result<String, String> {
    let PackValue::Struct { tag: 0x71, fields } = value else {
        return Err("expected graph RECORD".into());
    };
    let [PackValue::List(row)] = fields.as_slice() else {
        return Err("expected one graph row list".into());
    };
    let [PackValue::String(id)] = row.as_slice() else {
        return Err("graph row must contain one entity ID string".into());
    };
    if !(1..=1024).contains(&id.len()) {
        return Err("graph target entity ID requires 1..1024 bytes".into());
    }
    Ok(id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_identifiers_ids_and_deadlines_before_network() {
        for label in ["", "A) MATCH (x", "9A", "á", &"A".repeat(129)] {
            assert!(graph("127.0.0.1:1", "db", 100, label, "id", "LINK", "B")
                .unwrap_err()
                .contains("identifiers"));
        }
        for id in ["", &"x".repeat(1025)] {
            assert!(graph("127.0.0.1:1", "db", 100, "A", id, "LINK", "B")
                .unwrap_err()
                .contains("entity ID"));
        }
        for timeout in [0, 10001] {
            assert!(graph("127.0.0.1:1", "db", timeout, "A", "id", "LINK", "B")
                .unwrap_err()
                .contains("timeout"));
        }
    }
    #[test]
    fn record_shape_and_identifier_size_are_strict() {
        for row in [
            vec![],
            vec![PackValue::Null],
            vec![PackValue::String(String::new())],
            vec![PackValue::String("x".repeat(1025))],
            vec![PackValue::String("a".into()), PackValue::Null],
        ] {
            assert!(decode_record(PackValue::Struct {
                tag: 0x71,
                fields: vec![PackValue::List(row)]
            })
            .is_err());
        }
        assert_eq!(
            decode_record(PackValue::Struct {
                tag: 0x71,
                fields: vec![PackValue::List(vec![PackValue::String("x".repeat(1024))])]
            })
            .unwrap()
            .len(),
            1024
        );
    }
    fn fake_rows(
        count: usize,
        fields_ok: bool,
        has_more: bool,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        fn read(stream: &mut std::net::TcpStream) -> Option<PackValue> {
            let mut bytes = Vec::new();
            loop {
                let mut header = [0; 2];
                stream.read_exact(&mut header).ok()?;
                let count = u16::from_be_bytes(header) as usize;
                if count == 0 {
                    break;
                }
                let start = bytes.len();
                bytes.resize(start + count, 0);
                stream.read_exact(&mut bytes[start..]).ok()?;
            }
            brainwire_bolt::packstream::unpack(&mut bytes.as_slice()).ok()
        }
        fn write(stream: &mut std::net::TcpStream, tag: u8, value: PackValue) {
            let mut body = Vec::new();
            brainwire_bolt::packstream::pack(
                &PackValue::Struct {
                    tag,
                    fields: vec![value],
                },
                &mut body,
            );
            let _ = stream.write_all(&brainwire_bolt::chunk::encode_message(&body));
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut handshake = [0; 20];
            stream.read_exact(&mut handshake).unwrap();
            stream.write_all(&[0, 0, 4, 5]).unwrap();
            for _ in 0..2 {
                assert!(read(&mut stream).is_some());
                write(&mut stream, 0x70, PackValue::Map(vec![]));
            }
            let PackValue::Struct { tag: 0x10, fields } = read(&mut stream).unwrap() else {
                panic!("RUN");
            };
            assert_eq!(
                fields[1].get("id"),
                Some(&PackValue::String("parameter-only' ID".into()))
            );
            write(
                &mut stream,
                0x70,
                PackValue::Map(vec![(
                    "fields".into(),
                    PackValue::List(if fields_ok {
                        vec![PackValue::String("entity_id".into())]
                    } else {
                        vec![]
                    }),
                )]),
            );
            if read(&mut stream).is_none() {
                return;
            }
            for _ in 0..count {
                write(
                    &mut stream,
                    0x71,
                    PackValue::List(vec![PackValue::String("repeated".into())]),
                );
            }
            write(
                &mut stream,
                0x70,
                PackValue::Map(vec![("has_more".into(), PackValue::Bool(has_more))]),
            );
        });
        (address, thread)
    }

    #[test]
    fn rows_preserve_repeated_ids_and_reject_excess_wrong_fields_and_partial_summary() {
        for (count, fields_ok, has_more, accepted) in [
            (21, true, false, true),
            (22, true, false, false),
            (1, false, false, false),
            (1, true, true, false),
        ] {
            let (address, server) = fake_rows(count, fields_ok, has_more);
            let mut client = BoltClient::connect(&address, 1000, MAX_WIRE).unwrap();
            let result = rows(
                &mut client,
                "RETURN $id AS entity_id",
                "parameter-only' ID",
                21,
            );
            if accepted {
                assert_eq!(result.unwrap(), vec!["repeated".to_string(); 21]);
            } else {
                assert!(result.is_err());
            }
            drop(client);
            server.join().unwrap();
        }
    }
}
