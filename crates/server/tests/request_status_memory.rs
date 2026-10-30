//! The experimental status endpoint must account for every supplied parameter.
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
        .env("BRAINWIRE_SERVER_DEFAULT_DATABASE", "default")
        .env("BRAINWIRE_SERVER_AUTH_FILE", root.join("policy.json"))
        .env("BRAINWIRE_SERVER_REQUEST_LEDGER", "true")
        .env("BRAINWIRE_MEMORY_CACHE", "64MiB")
        .env("BRAINWIRE_MEMORY_MEMTABLES", "4MiB")
        .env("BRAINWIRE_MEMORY_QUERY", "64KiB")
        .env("BRAINWIRE_MEMORY_LIMIT", "0");
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
        let mut client = Self(stream);
        client.control(0x01, vec![PackValue::Map(vec![])]);
        client.control(
            0x6a,
            vec![PackValue::Map(vec![
                ("scheme".into(), PackValue::String("basic".into())),
                ("principal".into(), PackValue::String("writer".into())),
                ("credentials".into(), PackValue::String("a".repeat(64))),
            ])],
        );
        client
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

    fn control(&mut self, tag: u8, fields: Vec<PackValue>) {
        self.send(tag, fields);
        assert!(matches!(
            self.receive(),
            PackValue::Struct { tag: 0x70, .. }
        ));
    }

    fn query(
        &mut self,
        text: &str,
        params: Vec<(String, PackValue)>,
    ) -> Result<Vec<Vec<PackValue>>, PackValue> {
        self.send(
            0x10,
            vec![
                PackValue::String(text.into()),
                PackValue::Map(params),
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
                    other => panic!("invalid record: {other:?}"),
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
fn request_status_refuses_oversized_unused_parameter_and_recovers_with_reset() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("policy.json"), r#"{"principals":[{"id":"writer","token_sha256":"ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb","graphs":{"default":"write"},"instance_admin":true}]}"#).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        command(root.path())
            .arg("serve")
            .env("BRAINWIRE_SERVER_LISTEN", &address)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(&address).is_err() {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before readiness"
        );
        assert!(
            Instant::now() < deadline,
            "protected ledger server did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut bolt = Bolt::connect(&address);
    let query = "CALL db.requestStatus($seed)";
    let params = |extra: PackValue| {
        vec![
            ("seed".into(), PackValue::String("missing".into())),
            ("unused".into(), extra),
        ]
    };
    let small = bolt
        .query(query, params(PackValue::String("small".into())))
        .unwrap();
    assert_eq!(small.len(), 1);
    assert_eq!(small[0][0], PackValue::String("not_recorded".into()));

    let failure = bolt
        .query(query, params(PackValue::String("x".repeat(128 * 1024))))
        .expect_err("unused status parameters must count against memory.query");
    assert_eq!(
        failure.get("code"),
        Some(&PackValue::String(
            "Neo.TransientError.General.MemoryPressure".into()
        ))
    );
    let Some(PackValue::String(message)) = failure.get("message") else {
        panic!("missing memory details: {failure:?}");
    };
    assert!(message.contains("reason=statement_limit"), "{message}");
    assert!(message.contains("rolled_back=true"), "{message}");
    assert!(
        message.contains("requested_bytes=") && message.contains("available_bytes="),
        "{message}"
    );
    bolt.send(0x12, vec![]);
    assert!(matches!(
        bolt.receive(),
        PackValue::Struct { tag: 0x7e, .. }
    ));
    bolt.control(0x0f, vec![]);
    assert_eq!(
        bolt.query("RETURN 42", vec![]).unwrap(),
        vec![vec![PackValue::Int(42)]]
    );
    assert_eq!(bolt.query(query, params(PackValue::Null)).unwrap(), small);
}
