use super::{config, AuthMode, Databases, DbEngine};
use brainwire_bolt::chunk::{encode_message, ChunkReader};
use brainwire_bolt::packstream::{pack, unpack, PackValue};
use brainwire_bolt::server::{serve_until_with_shared_admission, ConnectionLimits};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn request(
    stream: &mut TcpStream,
    reader: &mut ChunkReader,
    tag: u8,
    fields: Vec<PackValue>,
) -> (u8, Vec<PackValue>) {
    let mut bytes = Vec::new();
    pack(&PackValue::Struct { tag, fields }, &mut bytes);
    stream.write_all(&encode_message(&bytes)).await.unwrap();
    loop {
        if let Some(reply) = reader.next_message() {
            let PackValue::Struct { tag, fields } = unpack(&mut reply.as_slice()).unwrap() else {
                panic!("expected Bolt response structure");
            };
            if tag != 0x71 {
                return (tag, fields);
            }
        } else {
            let mut incoming = [0; 1024];
            let length = stream.read(&mut incoming).await.unwrap();
            assert_ne!(length, 0, "transport closed before completion");
            reader.push(&incoming[..length]);
        }
    }
}

#[tokio::test]
async fn query_only_pressure_preserves_first_bolt_control_message() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config::Config::default();
        config.server.data_dir = dir.path().join("data");
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        let databases = Arc::new(Databases::open_all(&config).unwrap());
        let database = databases.get("default").unwrap();
        database.apply_ontology(&serde_json::from_str(include_str!(
            "../../core/tests/fixtures/graph_mapping.json"
        )).unwrap()).unwrap();
        let pool = databases.query_pool();
        let ledger = brainwire_core::admission::MemoryAdmission::new(Arc::new(|| 1 << 20));
        pool.set_shared_admission(Arc::clone(&ledger));
        let margin = Arc::new(AtomicUsize::new(1 << 20));
        let query_margin = Arc::clone(&margin);
        pool.set_process_headroom(move || query_margin.load(Ordering::Acquire));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve_until_with_shared_admission(
            listener,
            move || DbEngine::new(Arc::clone(&databases), "default".into()).unwrap(),
            ConnectionLimits::default(),
            AuthMode::AllowAll,
            async { let _ = stopped.await; },
            Duration::from_secs(1),
            Some(Arc::clone(&ledger)),
        ));
        let mut client = TcpStream::connect(address).await.unwrap();
        let mut handshake = brainwire_bolt::handshake::MAGIC.to_vec();
        handshake.extend_from_slice(&[0, 0, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        client.write_all(&handshake).await.unwrap();
        assert_eq!(client.read_u32().await.unwrap(), 0x405);
        let mut reader = ChunkReader::default();
        let empty = PackValue::Map(vec![]);
        for tag in [0x01, 0x6a, 0x11] {
            assert_eq!(request(&mut client, &mut reader, tag, vec![empty.clone()]).await.0, 0x70);
        }
        let write = PackValue::String("CREATE (:Customer {entity_id:'retry'}) RETURN 1".into());
        let run = vec![write, empty.clone(), empty.clone()];
        let pull = vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])];
        assert_eq!(request(&mut client, &mut reader, 0x10, run.clone()).await.0, 0x70);
        margin.store(0, Ordering::Release);
        assert_eq!(request(&mut client, &mut reader, 0x3f, pull.clone()).await.0, 0x70);
        let sequence = database.commit_sequence();
        let (tag, fields) = request(&mut client, &mut reader, 0x12, vec![]).await;
        assert_eq!(tag, 0x7f);
        assert_eq!(fields[0].get("code"), Some(&PackValue::String(brainwire_bolt::session::MEMORY_REFUSED.into())));
        assert!(matches!(fields[0].get("message"), Some(PackValue::String(message)) if message.contains("outcome=rolled_back")));
        assert_eq!(database.commit_sequence(), sequence);
        assert_eq!(database.stats().unwrap().nodes, 0);
        assert_eq!(pool.usage().reserved_bytes, 0);
        assert_eq!(ledger.reserved(), 0);
        assert_eq!(request(&mut client, &mut reader, 0x0f, vec![]).await.0, 0x70);
        assert_eq!(request(&mut client, &mut reader, 0x11, vec![empty.clone()]).await.0, 0x70);
        assert_eq!(request(&mut client, &mut reader, 0x13, vec![]).await.0, 0x70);
        margin.store(1 << 20, Ordering::Release);
        assert_eq!(request(&mut client, &mut reader, 0x11, vec![empty]).await.0, 0x70);
        assert_eq!(request(&mut client, &mut reader, 0x10, run).await.0, 0x70);
        assert_eq!(request(&mut client, &mut reader, 0x3f, pull).await.0, 0x70);
        assert_eq!(request(&mut client, &mut reader, 0x12, vec![]).await.0, 0x70);
        assert_eq!(database.stats().unwrap().nodes, 1);
        assert_eq!(pool.usage().reserved_bytes, 0);

        drop(client);
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }).await.expect("Bolt query pressure scenario timed out");
}
