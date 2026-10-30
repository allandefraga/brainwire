//! Blocking operators refuse oversized results without persisting prior writes.
use brainwire_bolt::{
    chunk::encode_message,
    handshake::MAGIC,
    packstream::{pack, unpack, PackValue},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    command.current_dir(root);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("BRAINWIRE_")) {
        command.env_remove(key);
    }
    command
        .env("BRAINWIRE_SERVER_DATA_DIR", root.join("data"))
        .env("BRAINWIRE_SERVER_DEFAULT_DATABASE", "pages")
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "4MiB")
        .env("BRAINWIRE_MEMORY_QUERY", "64KiB");
    command
}

struct Bolt(TcpStream);
impl Bolt {
    fn connect(address: &str) -> Self {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut greeting = [0; 20];
        greeting[..4].copy_from_slice(&MAGIC);
        greeting[4..8].copy_from_slice(&[0, 0, 4, 5]);
        stream.write_all(&greeting).unwrap();
        let mut version = [0; 4];
        stream.read_exact(&mut version).unwrap();
        assert_eq!(version, [0, 0, 4, 5]);
        let mut bolt = Self(stream);
        bolt.control(0x01);
        bolt.control(0x6a);
        bolt
    }

    fn send(&mut self, tag: u8, fields: Vec<PackValue>) {
        let mut payload = Vec::new();
        pack(&PackValue::Struct { tag, fields }, &mut payload);
        self.0.write_all(&encode_message(&payload)).unwrap();
    }

    fn receive(&mut self) -> PackValue {
        let mut body = Vec::new();
        loop {
            let mut header = [0; 2];
            self.0.read_exact(&mut header).unwrap();
            let size = usize::from(u16::from_be_bytes(header));
            if size == 0 && !body.is_empty() {
                return unpack(&mut body.as_slice()).unwrap();
            }
            let start = body.len();
            body.resize(start + size, 0);
            self.0.read_exact(&mut body[start..]).unwrap();
        }
    }

    fn control(&mut self, tag: u8) {
        let fields = match tag {
            0x01 | 0x6a | 0x11 => vec![PackValue::Map(vec![])],
            _ => vec![],
        };
        self.send(tag, fields);
        assert!(matches!(
            self.receive(),
            PackValue::Struct { tag: 0x70, .. }
        ));
    }

    fn query(&mut self, text: &str) -> Result<Vec<Vec<PackValue>>, PackValue> {
        self.send(
            0x10,
            vec![
                PackValue::String(text.into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        );
        match self.receive() {
            PackValue::Struct { tag: 0x70, .. } => {}
            PackValue::Struct {
                tag: 0x7f,
                mut fields,
            } => return Err(fields.remove(0)),
            other => panic!("RUN {text}: {other:?}"),
        }
        self.send(0x3f, vec![PackValue::Map(vec![])]);
        let mut rows = Vec::new();
        loop {
            match self.receive() {
                PackValue::Struct {
                    tag: 0x71,
                    mut fields,
                } => match fields.remove(0) {
                    PackValue::List(row) => rows.push(row),
                    other => panic!("bad record: {other:?}"),
                },
                PackValue::Struct { tag: 0x70, .. } => return Ok(rows),
                PackValue::Struct {
                    tag: 0x7f,
                    mut fields,
                } => return Err(fields.remove(0)),
                other => panic!("PULL {text}: {other:?}"),
            }
        }
    }
}

#[test]
fn order_by_distinct_and_aggregation_refuse_with_rollback_and_release_memory() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("schema.json"), r#"{"version":"1","nodes":[{"label":"Page","properties":["text"],"indexed_fields":["text"]}]}"#).unwrap();
    let created = command(root.path())
        .args(["db", "create", "pages", "--ontology", "schema.json"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let seed = format!("UNWIND range(1,256) AS i CREATE (:Page {{entity_id:'p-'+toString(i),text:'{}'+toString(i)}})", "x".repeat(256));
    let seeded = command(root.path())
        .env("BRAINWIRE_MEMORY_QUERY", "16MiB")
        .args(["cypher", "--database", "data/pages", "--query", &seed])
        .output()
        .unwrap();
    assert!(seeded.status.success(), "{seeded:?}");
    let before = format!("{}1", "x".repeat(256));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        command(root.path())
            .arg("serve")
            .env("BRAINWIRE_SERVER_LISTEN", &address)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(&address).is_err() {
        assert!(server.0.try_wait().unwrap().is_none(), "server exited");
        assert!(Instant::now() < deadline, "server did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut bolt = Bolt::connect(&address);
    // A bounded top-k and a single aggregate work over the same fixture.
    assert_eq!(
        bolt.query("MATCH (p:Page) RETURN p.text ORDER BY p.text LIMIT 1")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        bolt.query("MATCH (p:Page) RETURN count(*)").unwrap(),
        vec![vec![PackValue::Int(256)]]
    );
    for query in [
        "MATCH (p:Page) RETURN p.text ORDER BY toLower(p.text)",
        "MATCH (p:Page) RETURN DISTINCT p.text",
        "MATCH (p:Page) RETURN p.text,count(*)",
        "MATCH (p:Page) RETURN collect(p.text)",
    ] {
        // Exercise the read-only route before the explicit write transaction.
        let failure = bolt.query(query).expect_err(query);
        assert_eq!(
            failure.get("code"),
            Some(&PackValue::String(
                "Neo.TransientError.General.MemoryPressure".into()
            )),
            "read-only {query}: {failure:?}"
        );
        bolt.control(0x0f);
        assert_eq!(
            bolt.query("RETURN 42").unwrap(),
            vec![vec![PackValue::Int(42)]]
        );
        bolt.control(0x11); // Explicit BEGIN: the pending overwrite must be discarded.
        bolt.query("MATCH (p:Page {entity_id:'p-1'}) SET p.text='pending'")
            .unwrap();
        let failure = bolt.query(query).expect_err(query);
        assert_eq!(
            failure.get("code"),
            Some(&PackValue::String(
                "Neo.TransientError.General.MemoryPressure".into()
            )),
            "{query}: {failure:?}"
        );
        let Some(PackValue::String(message)) = failure.get("message") else {
            panic!("missing memory details: {failure:?}");
        };
        assert!(message.contains("rolled_back=true"), "{query}: {message}");
        assert!(
            message.contains("reason=statement_limit"),
            "{query}: {message}"
        );
        assert!(
            message.contains("requested_bytes=") && message.contains("available_bytes="),
            "{query}: {message}"
        );
        bolt.send(0x12, vec![]); // COMMIT cannot revive the failed transaction.
        assert!(matches!(
            bolt.receive(),
            PackValue::Struct { tag: 0x7e, .. }
        ));
        bolt.control(0x0f); // RESET restores the session after the memory failure.
        assert_eq!(
            bolt.query("MATCH (p:Page {entity_id:'p-1'}) RETURN p.text")
                .unwrap(),
            vec![vec![PackValue::String(before.clone())]]
        );
        let memory = bolt.query("CALL db.memory() YIELD poolReservedBytes,poolPeakBytes RETURN poolReservedBytes,poolPeakBytes").unwrap();
        assert!(
            matches!(memory[0][0], PackValue::Int(bytes) if bytes < 4096),
            "{query}: leaked reservation: {memory:?}"
        );
        assert!(
            matches!(memory[0][1], PackValue::Int(bytes) if bytes <= 64 * 1024),
            "{query}: pool exceeded: {memory:?}"
        );
    }
    drop(bolt);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let audit = command(root.path())
        .args(["db", "check", "pages"])
        .output()
        .unwrap();
    assert!(audit.status.success(), "{audit:?}");
}
