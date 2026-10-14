use brainwire_replication::network::{
    request, serve, NetworkFactory, RpcRequest, RpcResponse, TlsFiles, TransportSettings,
    MAX_BODY_BYTES,
};
use brainwire_replication::{Application, BasicNode, Command, Raft, Reply, Storage};
use std::{
    collections::BTreeMap, path::Path, process::Command as Process, sync::Arc, time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
struct App;
impl Application for App {
    fn apply(&self, _: &Command) -> Result<Reply, String> {
        Ok(Reply::Applied { revision: 1 })
    }
}
struct Fixture {
    dir: TempDir,
    peers: BTreeMap<u64, BasicNode>,
    settings: Vec<Arc<TransportSettings>>,
    rafts: Vec<Raft>,
    tasks: Vec<JoinHandle<Result<(), brainwire_replication::network::TransportError>>>,
}
impl Fixture {
    async fn new(tls: bool) -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("token"), "0123456789abcdef0123456789abcdef").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("token"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        if tls {
            certificates(dir.path());
        }
        let mut listeners = Vec::new();
        let mut peers = BTreeMap::new();
        for id in 1..=3 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            peers.insert(
                id,
                BasicNode {
                    addr: listener.local_addr().unwrap().to_string(),
                },
            );
            listeners.push(listener);
        }
        let mut settings = Vec::new();
        let mut rafts = Vec::new();
        let mut tasks = Vec::new();
        for (index, listener) in listeners.into_iter().enumerate() {
            let id = index as u64 + 1;
            let config = Arc::new(
                TransportSettings::load(
                    id,
                    "fixture".into(),
                    peers.clone(),
                    &dir.path().join("token"),
                    tls.then(|| files(dir.path(), "leaf")),
                )
                .unwrap(),
            );
            let store =
                Storage::open(dir.path().join(format!("db{id}")), 32 << 20, Arc::new(App)).unwrap();
            let raft_config = openraft::Config {
                snapshot_policy: openraft::SnapshotPolicy::Never,
                ..Default::default()
            }
            .validate()
            .unwrap();
            let raft = Raft::new(
                id,
                Arc::new(raft_config),
                NetworkFactory::new(config.clone()),
                store.log_store(),
                store.state_machine(),
            )
            .await
            .unwrap();
            tasks.push(tokio::spawn(serve(
                listener,
                config.clone(),
                raft.clone(),
                store,
            )));
            settings.push(config);
            rafts.push(raft);
        }
        Self {
            dir,
            peers,
            settings,
            rafts,
            tasks,
        }
    }
    async fn shutdown(mut self) {
        for task in &self.tasks {
            task.abort();
        }
        for raft in &self.rafts {
            raft.shutdown().await.unwrap();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
    fn altered(
        &self,
        node: u64,
        cluster: &str,
        token: &str,
        tls: Option<TlsFiles>,
    ) -> TransportSettings {
        let path = self.dir.path().join("alternate-token");
        std::fs::write(&path, token).unwrap();
        TransportSettings::load(node, cluster.into(), self.peers.clone(), &path, tls).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Abort owned sockets even if an assertion panics.
        for task in &self.tasks {
            task.abort();
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            for raft in &self.rafts {
                let raft = raft.clone();
                runtime.spawn(async move {
                    let _ = raft.shutdown().await;
                });
            }
        }
    }
}
fn files(path: &Path, stem: &str) -> TlsFiles {
    TlsFiles {
        cert: path.join(format!("{stem}.crt")),
        key: path.join(format!("{stem}.key")),
        ca: path.join("ca.crt"),
    }
}
fn openssl(path: &Path, args: &[&str]) {
    let result = Process::new("openssl")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn certificates(path: &Path) {
    std::fs::write(path.join("ca.cnf"), "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=synthetic-private-ca\n[v3_ca]\nbasicConstraints=critical,CA:true,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n").unwrap();
    openssl(
        path,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
            "-config",
            "ca.cnf",
            "-extensions",
            "v3_ca",
        ],
    );
    for (stem, san) in [
        ("leaf", "IP:127.0.0.1,DNS:localhost"),
        ("wrong", "DNS:wrong.example"),
    ] {
        std::fs::write(path.join(format!("{stem}.cnf")), format!("[leaf]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always,issuer\nsubjectAltName={san}\n")).unwrap();
        openssl(
            path,
            &[
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                "/CN=synthetic-peer",
                "-keyout",
                &format!("{stem}.key"),
                "-out",
                &format!("{stem}.csr"),
            ],
        );
        openssl(
            path,
            &[
                "x509",
                "-req",
                "-days",
                "1",
                "-in",
                &format!("{stem}.csr"),
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                &format!("{stem}.crt"),
                "-extfile",
                &format!("{stem}.cnf"),
                "-extensions",
                "leaf",
            ],
        );
    }
}
async fn closed(mut stream: TcpStream) {
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(7), stream.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_three_peer_quorum_proposal_lookup_and_status() {
    let fixture = Fixture::new(false).await;
    assert!(matches!(
        request(
            &fixture.settings[0],
            1,
            RpcRequest::Initialize {
                members: fixture.peers.clone()
            }
        )
        .await
        .unwrap(),
        RpcResponse::Initialized
    ));
    let started = tokio::time::Instant::now();
    let leader = loop {
        if let Some(id) = fixture.rafts[0].metrics().borrow().current_leader {
            break id;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    let command = Command::Write {
        request_id: "test-1".into(),
        graph: "g".into(),
        expected_revision: 0,
        batch: vec![1],
    };
    assert!(matches!(
        request(
            &fixture.settings[0],
            leader,
            RpcRequest::Propose { command }
        )
        .await
        .unwrap(),
        RpcResponse::Proposed {
            reply: Reply::Applied { revision: 1 },
            ..
        }
    ));
    assert!(matches!(
        request(
            &fixture.settings[0],
            leader,
            RpcRequest::Lookup {
                request_id: "test-1".into()
            }
        )
        .await
        .unwrap(),
        RpcResponse::Lookup(Some(Reply::Applied { revision: 1 }))
    ));
    assert!(
        matches!(request(&fixture.settings[0], leader, RpcRequest::Status).await.unwrap(), RpcResponse::Status(s) if s.membership == vec![1,2,3] && s.committed_index.is_some() && s.running && s.max_log_bytes == 32 << 20 && s.log_bytes > 0 && s.log_remaining_bytes == s.max_log_bytes - s.log_bytes)
    );
    let follower = (1..=3).find(|id| *id != leader).unwrap();
    let began = tokio::time::Instant::now();
    while fixture.rafts[follower as usize - 1]
        .metrics()
        .borrow()
        .current_leader
        != Some(leader)
    {
        assert!(began.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        matches!(request(&fixture.settings[0], follower, RpcRequest::Lookup { request_id: "test-1".into() }).await.unwrap(), RpcResponse::Rejected { code, leader: Some(id) } if code == "lookup_unavailable" && id == leader)
    );
    // Without quorum, even an absent ID must never become an authoritative None.
    for id in 1..=3 {
        if id != leader {
            fixture.tasks[id as usize - 1].abort();
            fixture.rafts[id as usize - 1].shutdown().await.unwrap();
        }
    }
    for request_id in ["test-1", "possibly-lost-ack"] {
        let outcome = request(
            &fixture.settings[0],
            leader,
            RpcRequest::Lookup {
                request_id: request_id.into(),
            },
        )
        .await;
        assert!(
            matches!(outcome, Err(_) | Ok(RpcResponse::Rejected { .. })),
            "isolated leader must not expose local lookup as authoritative"
        );
    }
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_membership_bounds_and_total_deadline() {
    let fixture = Fixture::new(false).await;
    for settings in [
        fixture.altered(1, "other", "0123456789abcdef0123456789abcdef", None),
        fixture.altered(1, "fixture", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", None),
    ] {
        assert!(request(&settings, 2, RpcRequest::Status).await.is_err());
    }
    let mut members = fixture.peers.clone();
    members.remove(&3);
    assert!(
        matches!(request(&fixture.settings[0], 2, RpcRequest::Initialize { members }).await.unwrap(), RpcResponse::Rejected { code, .. } if code == "invalid_members")
    );
    let mut stream = TcpStream::connect(&fixture.peers[&2].addr).await.unwrap();
    stream.write_u32(MAX_BODY_BYTES as u32 + 1).await.unwrap();
    closed(stream).await;
    let mut stream = TcpStream::connect(&fixture.peers[&2].addr).await.unwrap();
    let body = serde_json::json!({"protocol":1,"cluster_id":"fixture","sender":99,"recipient":2,"token":"0123456789abcdef0123456789abcdef","payload":"Status"}).to_string();
    stream.write_u32(body.len() as u32).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    closed(stream).await;
    let mut stream = TcpStream::connect(&fixture.peers[&2].addr).await.unwrap();
    stream.write_u32(100).await.unwrap();
    stream.write_all(b"{").await.unwrap();
    closed(stream).await;
    assert!(matches!(
        request(&fixture.settings[0], 2, RpcRequest::Status)
            .await
            .unwrap(),
        RpcResponse::Status(_)
    ));
    fixture.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutual_tls_and_claimed_sender_hostname_required() {
    let fixture = Fixture::new(true).await;
    assert!(matches!(
        request(&fixture.settings[0], 2, RpcRequest::Status)
            .await
            .unwrap(),
        RpcResponse::Status(_)
    ));
    let wrong = fixture.altered(
        1,
        "fixture",
        "0123456789abcdef0123456789abcdef",
        Some(files(fixture.dir.path(), "wrong")),
    );
    assert!(request(&wrong, 2, RpcRequest::Status).await.is_err());
    let plaintext = fixture.altered(1, "fixture", "0123456789abcdef0123456789abcdef", None);
    assert!(request(&plaintext, 2, RpcRequest::Status).await.is_err());
    // A trusted client certificate is mandatory, even with the valid shared token.
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(fixture.dir.path().join("ca.crt")).unwrap(),
    )) {
        roots.add(cert.unwrap()).unwrap();
    }
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(&fixture.peers[&2].addr).await.unwrap();
    let attempt = tokio::time::timeout(Duration::from_secs(7), async {
        let mut stream = tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(
                rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap(),
                tcp,
            )
            .await?;
        stream.write_all(b"not-an-authenticated-rpc").await?;
        let mut byte = [0];
        stream.read(&mut byte).await
    })
    .await
    .unwrap();
    assert!(matches!(attempt, Ok(0) | Err(_)));
    // Outgoing TLS also checks the configured destination hostname.
    let wrong_chain = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(fixture.dir.path().join("wrong.crt")).unwrap(),
    ))
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(fixture.dir.path().join("wrong.key")).unwrap(),
    ))
    .unwrap()
    .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(wrong_chain, key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peers = fixture.peers.clone();
    peers.get_mut(&2).unwrap().addr = listener.local_addr().unwrap().to_string();
    let settings = TransportSettings::load(
        1,
        "fixture".into(),
        peers,
        &fixture.dir.path().join("token"),
        Some(files(fixture.dir.path(), "leaf")),
    )
    .unwrap();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let _ = tokio_rustls::TlsAcceptor::from(Arc::new(config))
            .accept(tcp)
            .await;
    });
    assert!(request(&settings, 2, RpcRequest::Status).await.is_err());
    task.abort();
    let _ = task.await;
    // A different CA cannot authenticate the peer's certificate.
    let other = TempDir::new().unwrap();
    certificates(other.path());
    let mut wrong_ca = files(fixture.dir.path(), "leaf");
    wrong_ca.ca = other.path().join("ca.crt");
    let client = fixture.altered(
        1,
        "fixture",
        "0123456789abcdef0123456789abcdef",
        Some(wrong_ca),
    );
    assert!(request(&client, 2, RpcRequest::Status).await.is_err());
    fixture.shutdown().await;
}
#[test]
fn nonloopback_requires_tls_and_debug_hides_secret() {
    let dir = TempDir::new().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, "0123456789abcdef0123456789abcdef").unwrap();
    let peers = (1..=3)
        .map(|id| {
            (
                id,
                BasicNode {
                    addr: format!("127.0.0.1:{}", 2000 + id),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let settings = TransportSettings::load(1, "test".into(), peers.clone(), &token, None).unwrap();
    assert!(!format!("{settings:?}").contains("0123456789abcdef"));
    let mut remote = peers;
    remote.get_mut(&2).unwrap().addr = "192.0.2.1:3000".into();
    assert!(TransportSettings::load(1, "test".into(), remote, &token, None).is_err());
}
