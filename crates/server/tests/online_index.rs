//! CREATE INDEX over Bolt on a running server becomes online in the background.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use brainwire_bolt::{
    chunk::encode_message,
    handshake::MAGIC,
    packstream::{pack, unpack, PackValue},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_brainwire"));
    c.current_dir(dir);
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("BRAINWIRE_")) {
        c.env_remove(key);
    }
    c.env("BRAINWIRE_SERVER_DATA_DIR", dir.join("data"))
        .env("BRAINWIRE_SERVER_DEFAULT_DATABASE", "tickets")
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "16MiB")
        .env("BRAINWIRE_MEMORY_QUERY", "16MiB");
    c
}

struct Bolt(TcpStream);
impl Bolt {
    fn connect(address: &str) -> Self {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut greeting = [0u8; 20];
        greeting[..4].copy_from_slice(&MAGIC);
        greeting[4..8].copy_from_slice(&[0, 0, 4, 5]);
        stream.write_all(&greeting).unwrap();
        let mut version = [0u8; 4];
        stream.read_exact(&mut version).unwrap();
        assert_eq!(version, [0, 0, 4, 5]);
        let mut bolt = Self(stream);
        bolt.request(0x01, vec![PackValue::Map(vec![])]);
        bolt.request(0x6a, vec![PackValue::Map(vec![])]);
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
            let mut size = [0u8; 2];
            self.0.read_exact(&mut size).unwrap();
            let size = usize::from(u16::from_be_bytes(size));
            if size == 0 && !body.is_empty() {
                return unpack(&mut body.as_slice()).unwrap();
            }
            let start = body.len();
            body.resize(start + size, 0);
            self.0.read_exact(&mut body[start..]).unwrap();
        }
    }

    fn request(&mut self, tag: u8, fields: Vec<PackValue>) {
        self.send(tag, fields);
        match self.receive() {
            PackValue::Struct { tag: 0x70, .. } => {}
            other => panic!("request {tag:#x} failed: {other:?}"),
        }
    }

    /// Auto-commit RUN + PULL; returns the records.
    fn query(&mut self, query: &str) -> Vec<Vec<PackValue>> {
        self.request(
            0x10,
            vec![
                PackValue::String(query.into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        );
        self.send(0x3f, vec![PackValue::Map(vec![])]);
        let mut rows = vec![];
        loop {
            match self.receive() {
                PackValue::Struct { tag: 0x71, fields } => match fields.into_iter().next() {
                    Some(PackValue::List(row)) => rows.push(row),
                    other => panic!("bad record {other:?}"),
                },
                PackValue::Struct { tag: 0x70, .. } => return rows,
                other => panic!("{query}: {other:?}"),
            }
        }
    }
}

#[test]
fn create_index_over_bolt_becomes_online_on_a_running_server() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("schema.json"),
        include_str!("../../core/tests/fixtures/graph_mapping.json"),
    )
    .unwrap();
    let created = command(dir.path())
        .args(["db", "create", "tickets", "--ontology", "schema.json"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        command(dir.path())
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
        assert!(Instant::now() < deadline, "server not ready");
        std::thread::sleep(Duration::from_millis(30));
    }
    let mut bolt = Bolt::connect(&address);
    bolt.query(
        "UNWIND range(1, 3000) AS i CREATE (:Customer {entity_id: 'c' + toString(i), name: 'n' + toString(i % 7)})",
    );
    bolt.query("CREATE INDEX by_name IF NOT EXISTS FOR (c:Customer) ON (c.name)");
    let state = "CALL db.indexes() YIELD name, state WHERE name = 'by_name' RETURN state";
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let rows = bolt.query(state);
        assert_eq!(rows.len(), 1, "{rows:?}");
        if rows[0][0] == PackValue::String("scalar_equality_ready".into()) {
            break;
        }
        assert_eq!(rows[0][0], PackValue::String("populating".into()));
        assert!(Instant::now() < deadline, "index never became online");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        bolt.query("MATCH (c:Customer {name: 'n3'}) RETURN count(*)"),
        vec![vec![PackValue::Int(429)]]
    );
    drop(bolt);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let check = command(dir.path())
        .args(["db", "check", "tickets"])
        .output()
        .unwrap();
    assert!(check.status.success(), "{check:?}");
}
