//! Bounded, read-only shared query-pool telemetry over Bolt.
use crate::probe::{success, BoltClient};
use brainwire_bolt::packstream::PackValue;
use brainwire_tui::MemorySnapshot;

const QUERY: &str = "CALL db.memory() YIELD poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections RETURN poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections";
const FIELDS: [&str; 4] = [
    "poolLimitBytes",
    "poolReservedBytes",
    "poolPeakBytes",
    "poolRejections",
];
const MAX_WIRE: usize = 64 * 1024;

pub fn memory(address: &str, graph: &str, timeout_ms: u64) -> Result<MemorySnapshot, String> {
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
        return Err("memory RUN returned unexpected fields".into());
    }
    client.send(
        0x3f,
        vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
    )?;
    let snapshot = decode_record(client.receive()?)?;
    let metadata = success(client.receive()?)?;
    match metadata.get("has_more") {
        None | Some(PackValue::Bool(false)) => {}
        _ => return Err("memory result is incomplete (has_more)".into()),
    }
    client.finish()?;
    Ok(snapshot)
}

fn decode_record(value: PackValue) -> Result<MemorySnapshot, String> {
    let PackValue::Struct { tag: 0x71, fields } = value else {
        return Err("expected one memory RECORD".into());
    };
    if fields.len() != 1 {
        return Err("unexpected memory RECORD fields".into());
    }
    let PackValue::List(row) = &fields[0] else {
        return Err("expected memory row list".into());
    };
    if row.len() != 4 {
        return Err("memory row must contain four fields".into());
    }
    if row.iter().all(|v| matches!(v, PackValue::Null)) {
        return Ok(MemorySnapshot::default());
    }
    let mut counters = [0; 4];
    for (counter, value) in counters.iter_mut().zip(row) {
        let PackValue::Int(value) = value else {
            return Err("memory counters must be nonnegative integers or all null".into());
        };
        *counter =
            u64::try_from(*value).map_err(|_| "memory counters must be nonnegative integers")?;
    }
    Ok(MemorySnapshot {
        pool_limit_bytes: Some(counters[0]),
        pool_reserved_bytes: Some(counters[1]),
        pool_peak_bytes: Some(counters[2]),
        pool_rejections: Some(counters[3]),
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

    fn record(row: Vec<PackValue>) -> PackValue {
        PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(row)],
        }
    }
    #[test]
    fn strict_counters_preserve_unknown_and_reject_malformed_values() {
        let good = decode_record(record(vec![
            PackValue::Int(10),
            PackValue::Int(0),
            PackValue::Int(i64::MAX),
            PackValue::Int(3),
        ]))
        .unwrap();
        assert_eq!(good.pool_peak_bytes, Some(i64::MAX as u64));
        assert_eq!(good.pool_reserved_bytes, Some(0));
        assert_eq!(
            decode_record(record(vec![PackValue::Null; 4])).unwrap(),
            MemorySnapshot::default()
        );
        for bad in [
            PackValue::Null,
            PackValue::Int(-1),
            PackValue::Float(0.0),
            PackValue::String("0".into()),
            PackValue::Bool(false),
        ] {
            for index in 0..4 {
                let mut row = vec![PackValue::Int(0); 4];
                row[index] = bad.clone();
                assert!(decode_record(record(row)).is_err());
            }
        }
        for row in [
            vec![],
            vec![PackValue::Int(1); 3],
            vec![PackValue::Int(1); 5],
        ] {
            assert!(decode_record(record(row)).is_err());
        }
        assert!(decode_record(PackValue::Struct {
            tag: 0x70,
            fields: vec![PackValue::Map(vec![])]
        })
        .is_err());
        assert!(decode_record(PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(vec![PackValue::Int(0); 4]), PackValue::Null]
        })
        .is_err());
    }
    #[test]
    fn rejects_invalid_options_before_connecting() {
        assert!(memory("127.0.0.1:1", "bad/name", 100)
            .unwrap_err()
            .contains("database name"));
        for timeout in [0, 10001] {
            assert!(memory("127.0.0.1:1", "graph", timeout)
                .unwrap_err()
                .contains("timeout"));
        }
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
            PackValue::Int(10),
            PackValue::Int(2),
            PackValue::Int(4),
            PackValue::Int(0),
        ])
    }
    #[test]
    fn bounded_wire_requires_exact_fields_one_record_and_complete_summary() {
        let mut good = message(0x71, row());
        good.extend(success_bytes(vec![]));
        let (address, server) = fake(good.clone(), fields());
        assert_eq!(
            memory(&address, "selected", 1000)
                .unwrap()
                .pool_reserved_bytes,
            Some(2)
        );
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
            message(0x7f, PackValue::Map(vec![])),
            success_bytes(vec![]),
            message(0x71, PackValue::Bytes(vec![0; MAX_WIRE])),
        ] {
            let (address, server) = fake(bytes, fields());
            assert!(memory(&address, "selected", 1000).is_err());
            server.join().unwrap();
        }
        let (address, server) = fake(good.clone(), PackValue::List(vec![]));
        assert!(memory(&address, "selected", 1000)
            .unwrap_err()
            .contains("unexpected fields"));
        server.join().unwrap();
        let mut delayed = good.clone();
        delayed.extend(message(0x71, row()));
        let (address, server) = fake_with_delay(delayed, fields(), Some(good.len()));
        assert!(memory(&address, "selected", 1000)
            .unwrap_err()
            .contains("trailing"));
        server.join().unwrap();
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
        let start = std::time::Instant::now();
        assert!(memory(&address, "selected", 100)
            .unwrap_err()
            .contains("deadline"));
        assert!(start.elapsed() < Duration::from_millis(250));
        server.join().unwrap();
    }
}
