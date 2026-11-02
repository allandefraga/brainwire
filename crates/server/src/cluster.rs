//! Opt-in static three-voter consensus. Standalone serving remains independent.

mod application;
pub(crate) use application::bounded_read;
pub(crate) use application::validate_graphs;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use brainwire_replication::network::{
    self, NetworkFactory, RpcRequest, RpcResponse, TlsFiles, TransportSettings,
};
use brainwire_replication::{Command, Raft, Reply, Storage};
use openraft::{BasicNode, SnapshotPolicy};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::Config;
use crate::databases::Databases;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClusterConfig {
    pub node_id: u64,
    pub cluster_id: String,
    pub listen: String,
    pub peers: Vec<Peer>,
    pub token_file: PathBuf,
    pub tls: Option<TlsConfig>,
    #[serde(default = "log_capacity")]
    pub max_log_bytes: u64,
}

fn log_capacity() -> u64 {
    1 << 30
}

fn running_diagnostic(status: &network::ClusterStatus) -> &'static str {
    match (status.running, status.running_issue.as_deref()) {
        (true, None) => "running",
        (false, Some("storage_error")) => "fatal_storage_error",
        (false, Some("panic")) => "raft_panicked",
        (false, Some("stopped")) => "raft_stopped",
        _ => "not_running",
    }
}

fn valid_running_state(status: &network::ClusterStatus) -> bool {
    (!status.running || status.running_issue.is_none())
        && status
            .running_issue
            .as_deref()
            .is_none_or(|issue| matches!(issue, "storage_error" | "panic" | "stopped"))
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Peer {
    pub id: u64,
    pub address: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsConfig {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub ca: PathBuf,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u8,
    node_id: u64,
    cluster_id: String,
    peers: BTreeMap<u64, String>,
}

impl ClusterConfig {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes = bounded_read(path, 64 << 10)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| "cluster configuration must be UTF-8")?;
        if text.len() > 64 << 10 {
            return Err("cluster configuration exceeds 64 KiB".into());
        }
        let config: Self = toml::from_str(text).map_err(|_| "invalid cluster configuration")?;
        config.members()?;
        if config.cluster_id.is_empty()
            || config.cluster_id.len() > 128
            || !config
                .cluster_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err("invalid cluster identity".into());
        }
        let listener: std::net::SocketAddr = config
            .listen
            .parse()
            .map_err(|_| "cluster listen must be IP:PORT")?;
        if listener.port() == 0 {
            return Err("cluster listen port must be nonzero".into());
        }
        if !listener.ip().is_loopback() && config.tls.is_none() {
            return Err("non-loopback peer listener requires mutual TLS".into());
        }
        if !((1 << 20)..=(64_u64 << 30)).contains(&config.max_log_bytes) {
            return Err("max_log_bytes must be between 1 MiB and 64 GiB".into());
        }
        Ok(config)
    }

    pub(crate) fn members(&self) -> Result<BTreeMap<u64, BasicNode>, String> {
        if self.peers.len() != 3 || self.node_id == 0 {
            return Err("cluster requires exactly three distinct voters".into());
        }
        let mut members = BTreeMap::new();
        let mut addresses = std::collections::BTreeSet::new();
        for peer in &self.peers {
            if peer.id == 0
                || members
                    .insert(peer.id, BasicNode::new(peer.address.clone()))
                    .is_some()
                || !addresses.insert(peer.address.clone())
            {
                return Err("duplicate voter ID or address".into());
            }
        }
        if !members.contains_key(&self.node_id) {
            return Err("local node is not a configured voter".into());
        }
        Ok(members)
    }

    pub(crate) fn transport(&self) -> Result<Arc<TransportSettings>, String> {
        let tls = self.tls.as_ref().map(|tls| TlsFiles {
            cert: tls.cert.clone(),
            key: tls.key.clone(),
            ca: tls.ca.clone(),
        });
        TransportSettings::load(
            self.node_id,
            self.cluster_id.clone(),
            self.members()?,
            &self.token_file,
            tls,
        )
        .map(Arc::new)
        .map_err(|_| "invalid private peer transport configuration".into())
    }

    fn bind_identity(&self, root: &Path) -> Result<(), String> {
        let identity = Identity {
            version: 1,
            node_id: self.node_id,
            cluster_id: self.cluster_id.clone(),
            peers: self
                .peers
                .iter()
                .map(|peer| (peer.id, peer.address.clone()))
                .collect(),
        };
        std::fs::create_dir_all(root).map_err(|_| "cannot create cluster data directory")?;
        let path = root.join(".brainwire-cluster.json");
        if path.exists() {
            let existing: Identity = serde_json::from_slice(&bounded_read(&path, 64 << 10)?)
                .map_err(|_| "invalid persisted cluster identity")?;
            if existing != identity {
                return Err(
                    "persisted cluster/node/membership identity differs; refusing adoption".into(),
                );
            }
        } else {
            if std::fs::read_dir(root)
                .map_err(|_| "cannot inspect cluster data directory")?
                .next()
                .is_some()
            {
                return Err("new cluster requires an empty independent data directory; existing stores are not adopted".into());
            }
            application::durable_json(&path, &identity)?;
        }
        Ok(())
    }
}

pub(crate) struct Replica {
    pub raft: Raft,
    pub storage: Storage,
    runtime: tokio::runtime::Handle,
    writers: Arc<Semaphore>,
    cluster_id: String,
}

impl Replica {
    pub(crate) async fn start(
        config: &Config,
        cluster: &ClusterConfig,
        listener: tokio::net::TcpListener,
    ) -> Result<(Arc<Self>, Arc<Databases>), String> {
        let settings = cluster.transport()?;
        if !config
            .server
            .data_dir
            .join(".brainwire-cluster.json")
            .is_file()
            || !config
                .server
                .data_dir
                .join(".brainwire-raft/CURRENT")
                .is_file()
        {
            return Err(
                "persisted voter identity/consensus store missing; serve never provisions a voter"
                    .into(),
            );
        }
        config
            .server
            .auth_file
            .as_ref()
            .ok_or("replicated Bolt serving requires server.auth_file")?;
        cluster.bind_identity(&config.server.data_dir)?;
        let databases = Arc::new(Databases::open_replica(config)?);
        let app = Arc::new(application::GraphApplication::open(
            Arc::clone(&databases),
            &config.server.data_dir,
        )?);
        let storage = Storage::open(
            config.server.data_dir.join(".brainwire-raft"),
            cluster.max_log_bytes,
            app,
        )
        .map_err(|_| "cannot open durable consensus storage")?;
        if storage
            .registry_initialized()
            .map_err(|_| "cannot inspect committed registry proof")?
            && !config
                .server
                .data_dir
                .join(".brainwire-graphs.json")
                .is_file()
        {
            return Err("committed graph registry missing; refusing empty recovery".into());
        }
        let raft_config = openraft::Config {
            cluster_name: cluster.cluster_id.clone(),
            heartbeat_interval: 100,
            election_timeout_min: 500,
            election_timeout_max: 1000,
            max_payload_entries: 1,
            snapshot_policy: SnapshotPolicy::Never,
            ..openraft::Config::default()
        }
        .validate()
        .map_err(|_| "invalid consensus settings")?;
        let raft = Raft::new(
            cluster.node_id,
            Arc::new(raft_config),
            NetworkFactory::new(settings.clone()),
            storage.log_store(),
            storage.state_machine(),
        )
        .await
        .map_err(|_| "cannot start consensus")?;
        let replica = Arc::new(Self {
            raft: raft.clone(),
            storage: storage.clone(),
            runtime: tokio::runtime::Handle::current(),
            writers: Arc::new(Semaphore::new(1)),
            cluster_id: cluster.cluster_id.clone(),
        });
        tokio::spawn(async move {
            if let Err(error) = network::serve(listener, settings, raft, storage).await {
                eprintln!("private consensus listener stopped: {error}");
            }
        });
        Ok((replica, databases))
    }

    pub(crate) fn request_key(&self, graph: &str, caller: &str, seed: &str) -> String {
        use sha2::{Digest, Sha256};
        let namespace =
            serde_json::to_vec(&(&self.cluster_id, graph, caller, seed)).expect("string namespace");
        Sha256::digest(namespace)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub(crate) fn begin(
        &self,
        writable: bool,
        request_id: &str,
    ) -> Result<Option<OwnedSemaphorePermit>, String> {
        self.runtime.block_on(async {
            let permit = if writable { Some(tokio::time::timeout(Duration::from_secs(10), self.writers.clone().acquire_owned())
                .await.map_err(|_| "writer admission timed out")?.map_err(|_| "writer admission closed")?) } else { None };
            tokio::time::timeout(Duration::from_secs(10), self.raft.ensure_linearizable()).await
                .map_err(|_| "quorum read barrier timed out")?.map_err(|_| "leader/quorum unavailable")?;
            if writable {
                match self.storage.lookup(request_id).map_err(|_| "cannot inspect durable request outcome")? {
                    Some(Reply::Applied { .. }) => return Err(format!("request {request_id} already committed; inspect durable outcome with cluster request")),
                    Some(_) => return Err(format!("request {request_id} previously rejected; use a new request ID")),
                    None => {}
                }
            }
            Ok(permit)
        })
    }

    pub(crate) fn commit(
        &self,
        request_id: String,
        graph: String,
        expected_revision: u64,
        batch: Vec<u8>,
    ) -> Result<String, String> {
        if batch.len() > (1 << 20) - 256 {
            return Err("replicated batch exceeds 1 MiB; reduce the Python batch size".into());
        }
        self.runtime.block_on(async {
            let command = Command::Write { request_id: request_id.clone(), graph, expected_revision, batch };
            command.validate().map_err(|_| "replicated command exceeds validation limits")?;
            let response = tokio::time::timeout(Duration::from_secs(10), self.raft.client_write(command)).await
                .map_err(|_| format!("commit outcome unknown for request {request_id}; inspect durable outcome"))?
                .map_err(|_| format!("leader/quorum unavailable; commit outcome may be unknown for request {request_id}"))?;
            match response.data {
                Reply::Applied { .. } => Ok(format!("brainwire:{}:{}", self.cluster_id, response.log_id.index)),
                _ => Err("replicated transaction revision conflict".into()),
            }
        })
    }
}

pub(crate) fn control(config: &ClusterConfig, request: RpcRequest) -> Result<RpcResponse, String> {
    let settings = config.transport()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "cannot start control runtime")?
        .block_on(async {
            let follow = matches!(&request, RpcRequest::Lookup { .. });
            let mut target = config.node_id;
            for _ in 0..4 {
                let response = network::request(&settings, target, request.clone())
                    .await
                    .map_err(|_| "private control outcome unknown".to_owned())?;
                match response {
                    RpcResponse::Rejected {
                        leader: Some(leader),
                        ..
                    } if follow && leader != target && settings.peers().contains_key(&leader) => {
                        target = leader
                    }
                    _ => return Ok(response),
                }
            }
            Err("no quorum-authoritative request outcome available".into())
        })
}

pub(crate) fn probe(config: &ClusterConfig, kind: &str) -> Result<(), String> {
    let outcome = (|| {
        let settings = config.transport().map_err(|_| "transport_unavailable")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime_unavailable")?;
        runtime.block_on(async {
            let status = match network::request(&settings, config.node_id, RpcRequest::Status).await
            {
                Ok(RpcResponse::Status(status)) => status,
                _ => return Err("status_unavailable"),
            };
            if status.cluster_id != config.cluster_id || status.node_id != config.node_id {
                return Err("invalid_status");
            }
            if !valid_running_state(&status) {
                return Err("invalid_status");
            }
            if !status.running {
                return Err(match running_diagnostic(&status) {
                    "fatal_storage_error" => "fatal_storage_error",
                    "raft_panicked" => "raft_panicked",
                    "raft_stopped" => "raft_stopped",
                    _ => "not_running",
                });
            }
            if kind == "liveness" {
                return Ok(());
            }
            if status.membership != settings.peers().keys().copied().collect::<Vec<_>>() {
                return Err("uninitialized_or_membership_mismatch");
            }
            if status.state != "Leader" || status.leader != Some(config.node_id) {
                return Err("not_local_leader");
            }
            match network::request(
                &settings,
                config.node_id,
                RpcRequest::Lookup {
                    request_id: "brainwire-readiness-v1".into(),
                },
            )
            .await
            {
                Ok(RpcResponse::Lookup(_)) => Ok(()),
                _ => Err("quorum_unavailable"),
            }
        })
    })();
    println!(
        "{}",
        serde_json::json!({"kind":"raft_probe","node_id":config.node_id,
        "probe_kind":kind,"success":outcome.is_ok(),"reason":outcome.err().unwrap_or(if kind == "liveness" { "running" } else { "quorum_read_barrier_passed" }),
        "scope":"Raft only; does not verify graphs, Bolt authentication, or future writes"})
    );
    if outcome.is_err() {
        return Err("local Raft probe failed".into());
    }
    Ok(())
}

/// Independent samples do not establish a quorum or a write/read readiness barrier.
pub(crate) fn inspect(config: &ClusterConfig) -> Result<(), String> {
    let settings = config.transport()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "cannot start inspection runtime")?;
    let expected = settings.peers().keys().copied().collect::<Vec<_>>();
    let mut samples = runtime.block_on(async {
        // Transport validation guarantees exactly three voters; poll all before waiting.
        let settings_ref = &settings;
        let request = |node_id| async move {
            let started = Instant::now();
            let response = network::request(settings_ref, node_id, RpcRequest::Status).await;
            (started.elapsed().as_secs_f64() * 1_000.0, response)
        };
        let responses = tokio::join!(
            request(expected[0]),
            request(expected[1]),
            request(expected[2]),
        );
        let mut samples = Vec::new();
        for (&node_id, (response_time_ms, response)) in expected
            .iter()
            .zip([responses.0, responses.1, responses.2])
        {
            let sample = match response {
                Ok(RpcResponse::Status(status)) => {
                    let initialized = status.membership == expected;
                    let uninitialized = status.membership.is_empty()
                        && status.last_applied.is_none()
                        && status.committed_index.is_none();
                    let valid = status.cluster_id == config.cluster_id
                        && status.node_id == node_id
                        && (initialized || uninitialized)
                        && status.leader.is_none_or(|id| expected.contains(&id))
                        && matches!(status.state.as_str(), "Learner" | "Follower" | "Candidate" | "Leader" | "Shutdown")
                        && valid_running_state(&status)
                        && ((1 << 20)..=(64_u64 << 30)).contains(&status.max_log_bytes)
                        && status.log_bytes <= status.max_log_bytes
                        && status.log_remaining_bytes == status.max_log_bytes - status.log_bytes;
                    if valid {
                        serde_json::json!({"node_id":node_id,"response_time_ms":response_time_ms,
                            "initialized":initialized,
                            "diagnostic":if status.running { "sampled" } else { running_diagnostic(&status) },"status":status})
                    } else {
                        serde_json::json!({"node_id":node_id,"response_time_ms":response_time_ms,
                            "diagnostic":"invalid_status","status":null})
                    }
                }
                _ => serde_json::json!({"node_id":node_id,"response_time_ms":response_time_ms,
                    "diagnostic":"unavailable","status":null}),
            };
            samples.push(sample);
        }
        samples
    });
    let complete = samples
        .iter()
        .all(|sample| sample["diagnostic"] == "sampled");
    let all_initialized = complete && samples.iter().all(|sample| sample["initialized"] == true);
    let replication_progress = if all_initialized {
        let committed = samples
            .iter()
            .map(|sample| sample["status"]["committed_index"].as_u64())
            .collect::<Option<Vec<_>>>();
        let applied = samples
            .iter()
            .map(|sample| sample["status"]["last_applied"].as_u64())
            .collect::<Option<Vec<_>>>();
        match (committed, applied) {
            (Some(committed), Some(applied)) => {
                let committed_min = *committed.iter().min().expect("three configured voters");
                let committed_max = *committed.iter().max().expect("three configured voters");
                let applied_min = *applied.iter().min().expect("three configured voters");
                let applied_max = *applied.iter().max().expect("three configured voters");
                for (sample, (committed, applied)) in
                    samples.iter_mut().zip(committed.into_iter().zip(applied))
                {
                    sample["committed_index_lag"] =
                        serde_json::json!(committed_max.saturating_sub(committed));
                    sample["applied_index_lag"] =
                        serde_json::json!(applied_max.saturating_sub(applied));
                }
                Some(serde_json::json!({
                    "basis":"concurrent independent samples; not an atomic snapshot or readiness proof",
                    "committed_index":{"min":committed_min,"max":committed_max,"spread":committed_max - committed_min},
                    "applied_index":{"min":applied_min,"max":applied_max,"spread":applied_max - applied_min}
                }))
            }
            _ => None,
        }
    } else {
        None
    };
    let caps = samples
        .iter()
        .filter_map(|sample| sample["status"]["max_log_bytes"].as_u64())
        .collect::<Vec<_>>();
    let cap_agreement = complete.then(|| caps.iter().all(|cap| *cap == caps[0]));
    let configured_cap_agreement =
        complete.then(|| caps.iter().all(|cap| *cap == config.max_log_bytes));
    let report = serde_json::json!({"kind":"diagnostic_sample",
        "scope":"independent status samples; not a quorum or write/read readiness proof",
        "cluster_id":config.cluster_id,"configured_max_log_bytes":config.max_log_bytes,
        "complete":complete,"all_initialized":all_initialized,
        "replication_progress":replication_progress,
        "retained_log_cap_agreement":cap_agreement,
        "configured_log_cap_agreement":configured_cap_agreement,"members":samples});
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "cannot encode cluster inspection")?
    );
    if !complete || cap_agreement != Some(true) || configured_cap_agreement != Some(true) {
        return Err("cluster inspection incomplete or retained log capacities differ".into());
    }
    Ok(())
}

pub(crate) fn propose_control(
    config: &ClusterConfig,
    command: Command,
) -> Result<RpcResponse, String> {
    command
        .validate()
        .map_err(|_| "invalid or oversized cluster command")?;
    let settings = config.transport()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "cannot start control runtime")?;
    runtime.block_on(async {
        let mut target = config.node_id;
        for _ in 0..30 {
            let response = network::request(
                &settings,
                target,
                RpcRequest::Propose {
                    command: command.clone(),
                },
            )
            .await
            .map_err(|_| {
                format!(
                    "control outcome may be unknown for request {}",
                    command.request_id()
                )
            })?;
            match response {
                RpcResponse::Rejected {
                    leader: Some(leader),
                    ..
                } if settings.peers().contains_key(&leader) => target = leader,
                RpcResponse::Rejected { leader: None, .. } => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                _ => return Ok(response),
            }
        }
        Err("no leader became available".into())
    })
}

pub(crate) fn audit(cluster: &ClusterConfig, config_path: &Path) -> Result<(), String> {
    use brainwire_store::db::{Database, Resources, StoreConfig};
    use brainwire_store::txn::Cf;
    use sha2::{Digest, Sha256};
    let loaded = crate::config::load(Some(config_path), &std::env::vars().collect())
        .map_err(|_| "invalid server configuration")?;
    let root = &loaded.config.server.data_dir;
    if !root.join(".brainwire-cluster.json").is_file() {
        return Err("not a cluster data directory".into());
    }
    cluster.bind_identity(root)?;
    let bytes = bounded_read(&root.join(".brainwire-graphs.json"), 8 << 20)?;
    if bytes.len() > 8 << 20 {
        return Err("committed registry exceeds bounds".into());
    }
    let graphs: BTreeMap<String, Vec<u8>> =
        serde_json::from_slice(&bytes).map_err(|_| "invalid committed registry")?;
    validate_graphs(&graphs)?;
    let resources = Resources::new(StoreConfig {
        cache_bytes: usize::try_from(loaded.config.memory.cache.0)
            .map_err(|_| "cache exceeds address size")?,
        memtable_bytes: usize::try_from(loaded.config.memory.memtables.0)
            .map_err(|_| "memtables exceed address size")?,
        id_block: 1024,
        conflict_history_keys: 1_000_000,
    });
    let families = [
        Cf::Meta,
        Cf::Node,
        Cf::Pk,
        Cf::Out,
        Cf::In,
        Cf::Lbl,
        Cf::Idx,
        Cf::Ident,
        Cf::Event,
        Cf::Acl,
        Cf::Note,
    ];
    let mut results = BTreeMap::new();
    for name in graphs.keys() {
        let path = root.join(name);
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| "committed graph directory missing")?;
        if !metadata.is_dir() || !path.join("CURRENT").is_file() {
            return Err("committed graph is not a local store".into());
        }
        let database = Database::open(&path, &resources)
            .map_err(|_| "cannot audit open graph; stop all owned replicas first")?;
        brainwire_store::graph::check_invariants(&database)
            .map_err(|_| "graph/index audit failed")?;
        if !database
            .scalar_indexes_ready()
            .map_err(|_| "cannot inspect indexes")?
        {
            return Err("graph indexes not ready".into());
        }
        let mut hash = Sha256::new();
        let txn = database.begin();
        for family in families {
            hash.update(family.name().as_bytes());
            txn.visit_prefix::<brainwire_store::db::StoreError>(family, &[], |key, value| {
                if family == Cf::Meta && key == b"commit/sequence" {
                    return Ok(());
                }
                hash.update((key.len() as u64).to_be_bytes());
                hash.update(key);
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value);
                Ok(())
            })
            .map_err(|_| "cannot hash graph state")?;
        }
        let digest = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        results.insert(name,serde_json::json!({"status":"ok","logical_sha256":digest,
            "revision":brainwire_store::replicated_apply::applied_state(&database).map_err(|_| "invalid graph apply ledger")?.map_or(0,|state|state.index)}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"result":"PASS","graphs":results}))
            .map_err(|_| "cannot encode audit")?
    );
    Ok(())
}

pub(crate) fn provision(cluster: &ClusterConfig, config_path: &Path) -> Result<(), String> {
    let loaded = crate::config::load(Some(config_path), &std::env::vars().collect())
        .map_err(|_| "invalid server configuration")?;
    loaded
        .config
        .server
        .auth_file
        .as_ref()
        .ok_or("replicated serving requires server.auth_file")?;
    cluster.transport()?;
    let root = &loaded.config.server.data_dir;
    if root.exists()
        && std::fs::read_dir(root)
            .map_err(|_| "cannot inspect new member directory")?
            .next()
            .is_some()
    {
        return Err("one-time provisioning requires an empty directory; never re-provision a lost voter under its previous ID".into());
    }
    cluster.bind_identity(root)?;
    let databases = Arc::new(Databases::open_replica(&loaded.config)?);
    let app = Arc::new(application::GraphApplication::open(databases, root)?);
    let storage = Storage::open(root.join(".brainwire-raft"), cluster.max_log_bytes, app)
        .map_err(|_| "cannot provision durable consensus storage")?;
    if storage
        .last_applied()
        .map_err(|_| "cannot inspect consensus state")?
        .is_some()
    {
        return Err("provisioning refuses existing consensus history".into());
    }
    drop(storage);
    println!(
        "{}",
        serde_json::json!({"status":"provisioned","cluster_id":cluster.cluster_id,"node_id":cluster.node_id})
    );
    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::ClusterConfig;

    #[test]
    fn static_peer_listener_requires_a_fixed_nonzero_port() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cluster.toml");
        for (listen, valid) in [
            ("127.0.0.1:0", false),
            ("[::1]:0", false),
            ("127.0.0.1:19001", true),
            ("[::1]:19001", true),
        ] {
            std::fs::write(
                &path,
                format!(
                    r#"
node_id = 1
cluster_id = "fixed-ports"
listen = "{listen}"
token_file = "not-opened-by-config-load"
[[peers]]
id = 1
address = "127.0.0.1:19001"
[[peers]]
id = 2
address = "127.0.0.1:19002"
[[peers]]
id = 3
address = "127.0.0.1:19003"
"#
                ),
            )
            .unwrap();
            let loaded = ClusterConfig::load(&path);
            if valid {
                assert!(loaded.is_ok(), "valid listener {listen} rejected");
            } else {
                assert_eq!(
                    loaded.err().as_deref(),
                    Some("cluster listen port must be nonzero")
                );
            }
        }
    }
}
