//! Bounded authenticated peer RPC. Plain TCP is confined to explicit loopback peers.
//! Non-loopback peers require CA-verified mutual TLS. No snapshot RPC exists yet.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{InstallSnapshotError, PayloadTooLarge, RPCError, RaftError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::{BasicNode, Command, Raft, Reply, Storage, TypeConfig};

pub const MAX_BODY_BYTES: usize = 8 << 20;
pub const RPC_DEADLINE: Duration = Duration::from_secs(5);
const PROTOCOL: u16 = 1;
const MAX_CONNECTIONS: usize = 64;

#[derive(Debug, Clone)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("invalid peer transport configuration: {0}")]
    Configuration(&'static str),
    #[error("peer RPC exceeds the payload limit")]
    TooLarge,
    #[error("invalid peer RPC protocol")]
    Protocol,
    #[error("peer authentication rejected")]
    Authentication,
    #[error("peer RPC deadline exceeded; write outcome may be unknown")]
    Timeout,
    #[error("peer is unreachable")]
    Unreachable,
    #[error("peer TLS validation failed")]
    Tls,
}

#[derive(Clone)]
pub struct TransportSettings {
    node_id: u64,
    cluster_id: String,
    peers: BTreeMap<u64, BasicNode>,
    token: String,
    token_digest: [u8; 32],
    peer_verifier: Option<Arc<rustls::client::WebPkiServerVerifier>>,
    tls: Option<(Arc<rustls::ClientConfig>, Arc<rustls::ServerConfig>)>,
}
impl std::fmt::Debug for TransportSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportSettings")
            .field("node_id", &self.node_id)
            .field("cluster_id", &self.cluster_id)
            .field("peers", &self.peers)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

fn endpoint(address: &str) -> Result<(String, String, bool), TransportError> {
    if let Ok(socket) = address.parse::<SocketAddr>() {
        if socket.port() == 0 {
            return Err(TransportError::Configuration("peer port must be nonzero"));
        }
        return Ok((
            socket.to_string(),
            socket.ip().to_string(),
            socket.ip().is_loopback(),
        ));
    }
    let (host, port) = address
        .rsplit_once(':')
        .ok_or(TransportError::Configuration("peer requires host:port"))?;
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|n| *n != 0)
        .ok_or(TransportError::Configuration("invalid peer port"))?;
    let host = host.to_ascii_lowercase();
    if host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(TransportError::Configuration("invalid peer hostname"));
    }
    let loopback = host == "localhost";
    Ok((format!("{host}:{port}"), host, loopback))
}

fn tls_configs(
    files: &TlsFiles,
) -> Result<(Arc<rustls::ClientConfig>, Arc<rustls::ServerConfig>), TransportError> {
    let load = |path: &Path| {
        std::fs::File::open(path)
            .map(BufReader::new)
            .map_err(|_| TransportError::Configuration("TLS file unavailable"))
    };
    let certificates = rustls_pemfile::certs(&mut load(&files.cert)?)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TransportError::Configuration("invalid certificate PEM"))?;
    if certificates.is_empty() {
        return Err(TransportError::Configuration("empty certificate chain"));
    }
    let key = rustls_pemfile::private_key(&mut load(&files.key)?)
        .map_err(|_| TransportError::Configuration("invalid private key PEM"))?
        .ok_or(TransportError::Configuration("missing private key"))?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut load(&files.ca)?) {
        roots
            .add(certificate.map_err(|_| TransportError::Configuration("invalid CA PEM"))?)
            .map_err(|_| TransportError::Configuration("invalid CA certificate"))?;
    }
    if roots.is_empty() {
        return Err(TransportError::Configuration("empty CA trust store"));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| TransportError::Configuration("TLS protocols unavailable"))?
        .with_root_certificates(roots.clone())
        .with_client_auth_cert(certificates.clone(), key.clone_key())
        .map_err(|_| TransportError::Configuration("TLS certificate/key mismatch"))?;
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|_| TransportError::Configuration("invalid client trust store"))?;
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| TransportError::Configuration("TLS protocols unavailable"))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, key)
        .map_err(|_| TransportError::Configuration("TLS certificate/key mismatch"))?;
    Ok((Arc::new(client), Arc::new(server)))
}

fn peer_verifier(
    files: &TlsFiles,
) -> Result<Arc<rustls::client::WebPkiServerVerifier>, TransportError> {
    let mut roots = rustls::RootCertStore::empty();
    let mut file = BufReader::new(
        std::fs::File::open(&files.ca)
            .map_err(|_| TransportError::Configuration("CA file unavailable"))?,
    );
    for cert in rustls_pemfile::certs(&mut file) {
        roots
            .add(cert.map_err(|_| TransportError::Configuration("invalid CA PEM"))?)
            .map_err(|_| TransportError::Configuration("invalid CA certificate"))?;
    }
    rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .map_err(|_| TransportError::Configuration("invalid peer trust store"))
}

impl TransportSettings {
    pub fn load(
        node_id: u64,
        cluster_id: String,
        peers: BTreeMap<u64, BasicNode>,
        token_file: &Path,
        tls: Option<TlsFiles>,
    ) -> Result<Self, TransportError> {
        if cluster_id.is_empty()
            || cluster_id.len() > 128
            || !cluster_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(TransportError::Configuration("invalid cluster ID"));
        }
        if peers.len() != 3 || !peers.contains_key(&node_id) {
            return Err(TransportError::Configuration(
                "exactly three members including this node are required",
            ));
        }
        let metadata = std::fs::metadata(token_file)
            .map_err(|_| TransportError::Configuration("token file unavailable"))?;
        if !metadata.is_file() || metadata.len() > 258 {
            return Err(TransportError::Configuration(
                "invalid token file length/type",
            ));
        }
        let mut token = String::new();
        std::fs::File::open(token_file)
            .map_err(|_| TransportError::Configuration("token file unavailable"))?
            .take(259)
            .read_to_string(&mut token)
            .map_err(|_| TransportError::Configuration("invalid token file"))?;
        if token.len() > 258 {
            return Err(TransportError::Configuration("invalid token file length"));
        }
        let token = token.trim_end_matches(['\r', '\n']).to_owned();
        if !(32..=256).contains(&token.len())
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_+/=".contains(&b))
        {
            return Err(TransportError::Configuration(
                "token must contain 32..256 ASCII token characters",
            ));
        }
        let mut normalized = BTreeMap::new();
        let mut addresses = BTreeSet::new();
        for (id, node) in peers {
            let (address, _, loopback) = endpoint(&node.addr)?;
            if !loopback && tls.is_none() {
                return Err(TransportError::Configuration(
                    "non-loopback peers require mutual TLS",
                ));
            }
            if !addresses.insert(address.clone()) {
                return Err(TransportError::Configuration("duplicate peer endpoint"));
            }
            normalized.insert(id, BasicNode { addr: address });
        }
        let token_digest = Sha256::digest(token.as_bytes()).into();
        Ok(Self {
            node_id,
            cluster_id,
            peers: normalized,
            token,
            token_digest,
            peer_verifier: tls.as_ref().map(peer_verifier).transpose()?,
            tls: tls.as_ref().map(tls_configs).transpose()?,
        })
    }
    pub fn node_id(&self) -> u64 {
        self.node_id
    }
    pub fn cluster_id(&self) -> &str {
        &self.cluster_id
    }
    pub fn peers(&self) -> &BTreeMap<u64, BasicNode> {
        &self.peers
    }
    pub fn tls_enabled(&self) -> bool {
        self.tls.is_some()
    }
    fn authenticate(
        &self,
        envelope: &Envelope<'_>,
        sender: Option<u64>,
    ) -> Result<(), TransportError> {
        if envelope.protocol != PROTOCOL
            || envelope.cluster_id != self.cluster_id
            || envelope.recipient != self.node_id
            || !self.peers.contains_key(&envelope.sender)
            || sender.is_some_and(|id| id != envelope.sender)
            || envelope.token.len() > 256
        {
            return Err(TransportError::Authentication);
        }
        let supplied: [u8; 32] = Sha256::digest(envelope.token.as_bytes()).into();
        if !bool::from(supplied.ct_eq(&self.token_digest)) {
            return Err(TransportError::Authentication);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcRequest {
    AppendEntries(AppendEntriesRequest<TypeConfig>),
    Vote(VoteRequest<u64>),
    Initialize { members: BTreeMap<u64, BasicNode> },
    Propose { command: Command },
    Status,
    Lookup { request_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub cluster_id: String,
    pub node_id: u64,
    pub state: String,
    pub current_term: u64,
    pub leader: Option<u64>,
    pub last_log_index: Option<u64>,
    pub last_applied: Option<u64>,
    pub committed_index: Option<u64>,
    pub membership: Vec<u64>,
    pub log_bytes: u64,
    pub max_log_bytes: u64,
    pub log_remaining_bytes: u64,
    pub running: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum RpcResponse {
    Appended(AppendEntriesResponse<u64>),
    Voted(VoteResponse<u64>),
    Initialized,
    Proposed { reply: Reply, log_index: u64 },
    Status(ClusterStatus),
    Lookup(Option<Reply>),
    Rejected { code: String, leader: Option<u64> },
}

#[derive(Serialize)]
struct Outgoing<'a, T> {
    protocol: u16,
    cluster_id: &'a str,
    sender: u64,
    recipient: u64,
    token: &'a str,
    payload: &'a T,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    protocol: u16,
    #[serde(borrow)]
    cluster_id: &'a str,
    sender: u64,
    recipient: u64,
    #[serde(borrow)]
    token: &'a str,
    #[serde(borrow)]
    payload: &'a RawValue,
}
struct BoundedWriter {
    bytes: Vec<u8>,
    exceeded: bool,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BODY_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("bounded encoding"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode<T: Serialize>(
    settings: &TransportSettings,
    recipient: u64,
    payload: &T,
) -> Result<Vec<u8>, TransportError> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        exceeded: false,
    };
    let envelope = Outgoing {
        protocol: PROTOCOL,
        cluster_id: &settings.cluster_id,
        sender: settings.node_id,
        recipient,
        token: &settings.token,
        payload,
    };
    if serde_json::to_writer(&mut writer, &envelope).is_err() {
        return Err(if writer.exceeded {
            TransportError::TooLarge
        } else {
            TransportError::Protocol
        });
    }
    Ok(writer.bytes)
}
trait PeerStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> PeerStream for T {}
type Stream = Box<dyn PeerStream>;
async fn read_frame(stream: &mut Stream) -> Result<Vec<u8>, TransportError> {
    let count = stream
        .read_u32()
        .await
        .map_err(|_| TransportError::Unreachable)? as usize;
    if count == 0 {
        return Err(TransportError::Protocol);
    }
    if count > MAX_BODY_BYTES {
        return Err(TransportError::TooLarge);
    }
    let mut body = vec![0; count];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|_| TransportError::Unreachable)?;
    Ok(body)
}
async fn write_frame(stream: &mut Stream, body: &[u8]) -> Result<(), TransportError> {
    stream
        .write_u32(body.len() as u32)
        .await
        .map_err(|_| TransportError::Unreachable)?;
    stream
        .write_all(body)
        .await
        .map_err(|_| TransportError::Unreachable)?;
    stream
        .flush()
        .await
        .map_err(|_| TransportError::Unreachable)
}
async fn outgoing(settings: &TransportSettings, target: u64) -> Result<Stream, TransportError> {
    let peer = settings
        .peers
        .get(&target)
        .ok_or(TransportError::Authentication)?;
    let (_, host, loopback) = endpoint(&peer.addr)?;
    let addresses = tokio::net::lookup_host(&peer.addr)
        .await
        .map_err(|_| TransportError::Unreachable)?
        .collect::<Vec<_>>();
    if addresses.is_empty()
        || (settings.tls.is_none()
            && (!loopback || addresses.iter().any(|address| !address.ip().is_loopback())))
    {
        return Err(TransportError::Authentication);
    }
    let mut connected = None;
    for address in addresses {
        if let Ok(stream) = TcpStream::connect(address).await {
            connected = Some(stream);
            break;
        }
    }
    let stream = connected.ok_or(TransportError::Unreachable)?;
    stream
        .set_nodelay(true)
        .map_err(|_| TransportError::Unreachable)?;
    if let Some((client, _)) = &settings.tls {
        let name = ServerName::try_from(host).map_err(|_| TransportError::Tls)?;
        let tls = TlsConnector::from(client.clone())
            .connect(name, stream)
            .await
            .map_err(|_| TransportError::Tls)?;
        Ok(Box::new(tls))
    } else {
        Ok(Box::new(stream))
    }
}
async fn request_with_timeout(
    settings: &TransportSettings,
    target: u64,
    rpc: &RpcRequest,
    deadline: Duration,
) -> Result<RpcResponse, TransportError> {
    let expires = tokio::time::Instant::now() + deadline.min(RPC_DEADLINE);
    let body = encode(settings, target, rpc)?;
    tokio::time::timeout_at(expires, async {
        let mut stream = outgoing(settings, target).await?;
        write_frame(&mut stream, &body).await?;
        let body = read_frame(&mut stream).await?;
        let envelope: Envelope<'_> =
            serde_json::from_slice(&body).map_err(|_| TransportError::Protocol)?;
        settings.authenticate(&envelope, Some(target))?;
        serde_json::from_str(envelope.payload.get()).map_err(|_| TransportError::Protocol)
    })
    .await
    .map_err(|_| TransportError::Timeout)?
}
pub async fn request(
    settings: &TransportSettings,
    target: u64,
    rpc: RpcRequest,
) -> Result<RpcResponse, TransportError> {
    request_with_timeout(settings, target, &rpc, RPC_DEADLINE).await
}

#[derive(Clone)]
pub struct NetworkFactory {
    settings: Arc<TransportSettings>,
}
impl NetworkFactory {
    pub fn new(settings: Arc<TransportSettings>) -> Self {
        Self { settings }
    }
}
pub struct PeerClient {
    settings: Arc<TransportSettings>,
    target: u64,
    address_matches: bool,
}
impl RaftNetworkFactory<TypeConfig> for NetworkFactory {
    type Network = PeerClient;
    async fn new_client(&mut self, target: u64, node: &BasicNode) -> Self::Network {
        PeerClient {
            settings: self.settings.clone(),
            target,
            address_matches: self
                .settings
                .peers
                .get(&target)
                .is_some_and(|configured| configured == node),
        }
    }
}
impl PeerClient {
    async fn call(
        &self,
        rpc: RpcRequest,
        option: RPCOption,
    ) -> Result<RpcResponse, TransportError> {
        if !self.address_matches {
            return Err(TransportError::Authentication);
        }
        request_with_timeout(&self.settings, self.target, &rpc, option.hard_ttl()).await
    }
}
impl RaftNetwork<TypeConfig> for PeerClient {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        let hint = (rpc.entries.len() / 2).max(1) as u64;
        match self.call(RpcRequest::AppendEntries(rpc), option).await {
            Ok(RpcResponse::Appended(reply)) => Ok(reply),
            Err(TransportError::TooLarge) => Err(PayloadTooLarge::new_entries_hint(hint).into()),
            Ok(_) => Err(Unreachable::new(&TransportError::Protocol).into()),
            Err(error) => Err(Unreachable::new(&error).into()),
        }
    }
    async fn vote(
        &mut self,
        rpc: VoteRequest<u64>,
        option: RPCOption,
    ) -> Result<VoteResponse<u64>, RPCError<u64, BasicNode, RaftError<u64>>> {
        match self.call(RpcRequest::Vote(rpc), option).await {
            Ok(RpcResponse::Voted(reply)) => Ok(reply),
            Ok(_) => Err(Unreachable::new(&TransportError::Protocol).into()),
            Err(error) => Err(Unreachable::new(&error).into()),
        }
    }
    async fn install_snapshot(
        &mut self,
        _: InstallSnapshotRequest<TypeConfig>,
        _: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<u64>,
        RPCError<u64, BasicNode, RaftError<u64, InstallSnapshotError>>,
    > {
        Err(Unreachable::new(&TransportError::Configuration(
            "snapshot transport is not implemented",
        ))
        .into())
    }
}

fn rejected(code: &str, leader: Option<u64>) -> RpcResponse {
    RpcResponse::Rejected {
        code: code.into(),
        leader,
    }
}
async fn dispatch(
    settings: &TransportSettings,
    raft: &Raft,
    storage: &Storage,
    sender: u64,
    rpc: RpcRequest,
) -> RpcResponse {
    if match &rpc {
        RpcRequest::AppendEntries(rpc) => rpc.vote.leader_id.voted_for() != Some(sender),
        RpcRequest::Vote(rpc) => rpc.vote.leader_id.voted_for() != Some(sender),
        _ => false,
    } {
        return rejected("invalid_sender", None);
    }
    if let RpcRequest::AppendEntries(request) = &rpc {
        for entry in &request.entries {
            match &entry.payload {
                openraft::EntryPayload::Normal(command) if command.validate().is_err() => {
                    return rejected("invalid_command", None);
                }
                openraft::EntryPayload::Membership(membership) => {
                    let nodes: BTreeMap<_, _> = membership
                        .nodes()
                        .map(|(id, node)| (*id, node.clone()))
                        .collect();
                    let voters: BTreeSet<_> = settings.peers.keys().copied().collect();
                    if nodes != settings.peers
                        || membership.get_joint_config().as_slice() != [voters]
                    {
                        return rejected("invalid_members", None);
                    }
                }
                _ => {}
            }
        }
    }
    match rpc {
        RpcRequest::AppendEntries(rpc) => raft
            .append_entries(rpc)
            .await
            .map(RpcResponse::Appended)
            .unwrap_or_else(|_| rejected("raft_unavailable", None)),
        RpcRequest::Vote(rpc) => raft
            .vote(rpc)
            .await
            .map(RpcResponse::Voted)
            .unwrap_or_else(|_| rejected("raft_unavailable", None)),
        RpcRequest::Initialize { members } => {
            if members != settings.peers {
                return rejected("invalid_members", None);
            }
            match raft.is_initialized().await {
                Ok(true) => RpcResponse::Initialized,
                Ok(false) => raft
                    .initialize(members)
                    .await
                    .map(|_| RpcResponse::Initialized)
                    .unwrap_or_else(|_| rejected("initialize_failed", None)),
                Err(_) => rejected("raft_unavailable", None),
            }
        }
        RpcRequest::Propose { command } => {
            if command.validate().is_err() {
                return rejected("invalid_command", None);
            }
            match raft.client_write(command).await {
                Ok(response) => RpcResponse::Proposed {
                    reply: response.data,
                    log_index: response.log_id.index,
                },
                Err(error) => rejected(
                    "proposal_failed",
                    error
                        .forward_to_leader()
                        .and_then(|forward| forward.leader_id),
                ),
            }
        }
        RpcRequest::Status => {
            let metrics = raft.metrics().borrow().clone();
            let store = storage.clone();
            match tokio::task::spawn_blocking(move || {
                Ok::<_, ()>((
                    store.log_bytes().map_err(|_| ())?,
                    store.committed().map_err(|_| ())?,
                    store.max_log_bytes(),
                ))
            })
            .await
            {
                Ok(Ok((log_bytes, committed, max_log_bytes))) => {
                    RpcResponse::Status(ClusterStatus {
                        cluster_id: settings.cluster_id.clone(),
                        node_id: settings.node_id,
                        state: format!("{:?}", metrics.state),
                        current_term: metrics.current_term,
                        leader: metrics.current_leader,
                        last_log_index: metrics.last_log_index,
                        last_applied: metrics.last_applied.map(|id| id.index),
                        committed_index: committed.map(|id| id.index),
                        membership: metrics.membership_config.voter_ids().collect(),
                        log_bytes,
                        max_log_bytes,
                        log_remaining_bytes: max_log_bytes.saturating_sub(log_bytes),
                        running: metrics.running_state.is_ok(),
                    })
                }
                _ => rejected("storage_unavailable", None),
            }
        }
        RpcRequest::Lookup { request_id } => {
            if request_id.is_empty()
                || request_id.len() > 128
                || !request_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            {
                return rejected("invalid_request_id", None);
            }
            // Absence is authoritative only after a quorum read barrier and local apply.
            // A follower's local ledger may lag a committed write whose ACK was lost.
            if let Err(error) = raft.ensure_linearizable().await {
                return rejected(
                    "lookup_unavailable",
                    error
                        .forward_to_leader()
                        .and_then(|forward| forward.leader_id),
                );
            }
            let store = storage.clone();
            match tokio::task::spawn_blocking(move || store.lookup(&request_id).map_err(|_| ()))
                .await
            {
                Ok(Ok(reply)) => RpcResponse::Lookup(reply),
                _ => rejected("storage_unavailable", None),
            }
        }
    }
}

pub async fn serve(
    listener: TcpListener,
    settings: Arc<TransportSettings>,
    raft: Raft,
    storage: Storage,
) -> Result<(), TransportError> {
    if settings.tls.is_none()
        && !listener
            .local_addr()
            .map_err(|_| TransportError::Unreachable)?
            .ip()
            .is_loopback()
    {
        return Err(TransportError::Configuration(
            "plaintext listener must bind loopback",
        ));
    }
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (stream, _) = incoming.map_err(|_| TransportError::Unreachable)?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(stream); continue; };
                let settings = settings.clone(); let raft = raft.clone(); let storage = storage.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(RPC_DEADLINE, async {
                        let (mut stream, certificates): (Stream, _) = if let Some((_, server)) = &settings.tls { let tls = TlsAcceptor::from(server.clone()).accept(stream).await.map_err(|_| TransportError::Tls)?; let certificates = tls.get_ref().1.peer_certificates().map(|c| c.to_vec()); (Box::new(tls), certificates) } else { (Box::new(stream), None) };
                        let body = read_frame(&mut stream).await?;
                        let envelope: Envelope<'_> = serde_json::from_slice(&body).map_err(|_| TransportError::Protocol)?;
                        settings.authenticate(&envelope, None)?;
                        if let Some(verifier) = &settings.peer_verifier {
                            use rustls::client::danger::ServerCertVerifier;
                            let certs = certificates.as_ref().ok_or(TransportError::Tls)?;
                            let (leaf, chain) = certs.split_first().ok_or(TransportError::Tls)?;
                            let (_, host, _) = endpoint(&settings.peers[&envelope.sender].addr)?;
                            let name = ServerName::try_from(host).map_err(|_| TransportError::Tls)?;
                            verifier.verify_server_cert(leaf, chain, &name, &[], rustls::pki_types::UnixTime::now()).map_err(|_| TransportError::Tls)?;
                        }
                        let rpc: RpcRequest = serde_json::from_str(envelope.payload.get()).map_err(|_| TransportError::Protocol)?;
                        let response = dispatch(&settings, &raft, &storage, envelope.sender, rpc).await;
                        let encoded = encode(&settings, envelope.sender, &response)?;
                        write_frame(&mut stream, &encoded).await
                    }).await;
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}
