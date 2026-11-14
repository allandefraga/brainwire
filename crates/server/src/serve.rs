//! Durable Bolt entry point for the named database registry.

use brainwire_bolt::auth::{AuthMode, AuthPolicy};
use std::collections::BTreeMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config;
use crate::databases::Databases;
use crate::engine::DbEngine;

pub fn run(
    explicit_config: Option<&Path>,
    cluster_config: Option<&Path>,
    allow_unauthenticated_remote: bool,
) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("config: {e}"))?;
    let implicit = config::find_file(&cwd);
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let loaded = config::load(explicit_config.or(implicit.as_deref()), &env)
        .map_err(|e| format!("config: {e}"))?;
    if let Some(warning) = config::memory_overcommit_warning(&loaded) {
        eprintln!("warning: {warning}; review explicit budgets before serving");
    }
    let memory_pressure = crate::memory_pressure::PressureGuard::shared();
    eprintln!(
        "memory pressure admission: {:?} {:?} (sampled cgroup usage; not an RSS ceiling)",
        memory_pressure.status(),
        memory_pressure.usage(),
    );
    let cluster_config = cluster_config
        .map(crate::cluster::ClusterConfig::load)
        .transpose()?;
    if cluster_config.is_some() && loaded.config.server.request_ledger {
        return Err("experimental request ledger is standalone only".into());
    }
    let auth = match &loaded.config.server.auth_file {
        None => {
            eprintln!("warning: development mode, Bolt authentication is disabled; use an isolated network");
            AuthMode::AllowAll
        }
        Some(path) => {
            let file =
                std::fs::File::open(path).map_err(|_| "cannot open authentication policy file")?;
            let mut bytes = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|_| "cannot read authentication policy file")?;
            let policy =
                AuthPolicy::from_json(&bytes).map_err(|_| "invalid authentication policy file")?;
            AuthMode::Required(Arc::new(policy))
        }
    };
    let protected = matches!(auth, AuthMode::Required(_));
    let max_message = usize::try_from(loaded.config.server.max_message.0)
        .map_err(|_| "server.max_message exceeds platform address size")?;
    let max_decoded_message = usize::try_from(loaded.config.server.max_decoded_message.0)
        .map_err(|_| "server.max_decoded_message exceeds platform address size")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start server runtime: {e}"))?;
    let grace = std::time::Duration::from_millis(loaded.config.server.shutdown_grace_ms);
    let result = runtime.block_on(async move {
        // Bind before opening the stores, so a port conflict does not leave a
        // partly initialized server that appears to be accepting queries.
        let listener = TcpListener::bind(&loaded.config.server.listen)
            .await
            .map_err(|e| format!("cannot bind {}: {e}", loaded.config.server.listen))?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        validate_listener_security(address, protected, allow_unauthenticated_remote)?;
        let (replica, databases) = if let Some(cluster) = &cluster_config {
            let peer_listener = TcpListener::bind(&cluster.listen).await.map_err(|_| "cannot bind private peer listener")?;
            let (replica, databases) = crate::cluster::Replica::start(&loaded.config, cluster, peer_listener).await?;
            (Some(replica), databases)
        } else {
            let databases = Arc::new(Databases::open_all(&loaded.config)?);
            databases.start_index_workers();
            (None, databases)
        };
        if !crate::process_memory::init_allocator() {
            eprintln!("warning: allocator background decay unavailable; idle memory returns only on activity");
        }
        let cgroup = crate::memory_pressure::CgroupQueryHeadroom::start()
            .map_err(|error| format!("cannot start cgroup memory sampler: {error}"))?;
        let shared_admission = if loaded.config.memory.limit.0 > 0 {
            let memory = crate::process_memory::ProcessMemory::new(loaded.config.memory.limit.0);
            crate::process_memory::install(Arc::clone(&memory));
            let ceiling = Arc::clone(&memory);
            let admission = brainwire_core::admission::MemoryAdmission::new(Arc::new(move || ceiling.headroom()));
            databases.query_pool().set_shared_admission(Arc::clone(&admission));
            let ceiling = Arc::clone(&memory);
            databases.query_pool().set_process_headroom(move || {
                cgroup.as_ref().map_or(usize::MAX, |sample| sample.bytes()).min(ceiling.headroom())
            });
            databases.query_pool().set_process_memory(
                usize::try_from(loaded.config.memory.limit.0).unwrap_or(usize::MAX),
                || crate::process_memory::resident_bytes().and_then(|rss| usize::try_from(rss).ok()),
            );
            // The sampler must not keep databases open during graceful shutdown.
            let observed = Arc::downgrade(&databases);
            memory.start(move || {
                let Some(databases) = observed.upgrade() else {
                    return "databases=closed".to_owned();
                };
                let (cache, memtables) = databases.memory_usage();
                let query = databases.query_pool().usage();
                format!(
                    "cache={cache} memtables={memtables} query_reserved={} query_rejections={}",
                    query.reserved_bytes, query.rejections
                )
            });
            Some(admission)
        } else {
            if let Some(cgroup) = cgroup {
                databases.query_pool().set_process_headroom(move || cgroup.bytes());
            }
            None
        };
        // Registered after the stores open, so a signal during recovery still
        // stops at once (crash-safe), and before the first accept.
        let stop = termination().map_err(|e| format!("cannot install signal handlers: {e}"))?;
        let stores = Arc::clone(&databases);
        let default_name = loaded.config.server.default_database.clone();
        println!(
            "brainwire serve {} databases={:?} cache={} memtables={} query={} ingress={} limit={} (query reservations refused near the process RSS limit)",
            listener.local_addr().map_err(|e| e.to_string())?,
            databases.names(),
            loaded.config.memory.cache,
            loaded.config.memory.memtables,
            loaded.config.memory.query,
            loaded.config.memory.ingress,
            loaded.config.memory.limit,
        );
        let open = brainwire_bolt::server::serve_until_with_shared_admission(
            listener,
            move || {
                let engine = if let Some(replica) = &replica {
                    DbEngine::new_replicated(Arc::clone(&databases), default_name.clone(), Arc::clone(replica))
                } else if protected {
                    DbEngine::new_protected(Arc::clone(&databases), default_name.clone())
                } else {
                    DbEngine::new(Arc::clone(&databases), default_name.clone())
                };
                engine.expect("cannot start query worker")
            },
            brainwire_bolt::server::ConnectionLimits {
                max_message_bytes: max_message,
                max_decoded_message_bytes: max_decoded_message,
                ingress_memory_bytes: usize::try_from(loaded.config.memory.ingress.0).map_err(|_| "memory.ingress exceeds platform address size")?,
                io_timeout: std::time::Duration::from_millis(loaded.config.server.io_timeout_ms),
            },
            auth,
            stop,
            grace,
            shared_admission,
        )
        .await
        .map_err(|e| format!("Bolt server stopped: {e}"))?;
        Ok::<_, String>((open, stores))
    });
    // Abandoned sessions roll back when their tasks drop; a running engine
    // call is not awaited past this bound.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    let (open, stores) = result?;
    close_stores(stores);
    if open == 0 {
        eprintln!("brainwire: drained; all sessions closed");
        Ok(())
    } else {
        Err(format!(
            "shutdown grace of {} ms elapsed with {open} session(s) open; their unacknowledged work was abandoned",
            grace.as_millis()
        ))
    }
}

/// Engine worker threads release their store handles after their sessions
/// end. Close RocksDB on this thread before `exit()` runs C++ static
/// destructors: a store closing concurrently with them aborts the process.
fn close_stores(stores: Arc<Databases>) {
    // ponytail: fixed bound; a running engine call honours query_timeout_ms cooperatively.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while Arc::strong_count(&stores) > 1 {
        if std::time::Instant::now() >= deadline {
            eprintln!("brainwire: a running engine call still holds the stores; exiting without closing them (acknowledged commits are already synced)");
            // SAFETY: `_exit` skips destructors, as the previous abrupt
            // termination did; no Rust or RocksDB state is touched afterwards.
            unsafe { libc::_exit(1) }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(stores);
}

/// The first SIGTERM or SIGINT starts the drain. A further signal keeps the
/// handler installed, so forced stop is SIGKILL (the orchestrator's default).
fn termination() -> std::io::Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        eprintln!("brainwire: shutdown requested; draining sessions");
    })
}

fn validate_listener_security(
    address: SocketAddr,
    protected: bool,
    allow_unauthenticated_remote: bool,
) -> Result<(), String> {
    if address.ip().is_loopback() || protected || allow_unauthenticated_remote {
        return Ok(());
    }
    Err("refusing unauthenticated non-loopback Bolt listener; configure server.auth_file or explicitly opt in with --allow-unauthenticated-remote on an isolated development network".into())
}

#[cfg(test)]
#[path = "query_pressure_bolt_test.rs"]
mod query_pressure_bolt_test;

#[cfg(test)]
mod tests {
    use super::validate_listener_security;
    use std::net::SocketAddr;

    fn address(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    #[test]
    fn unauthenticated_listeners_are_loopback_only_by_default() {
        assert!(validate_listener_security(address("127.0.0.1:7687"), false, false).is_ok());
        assert!(validate_listener_security(address("[::1]:7687"), false, false).is_ok());
        assert!(validate_listener_security(address("0.0.0.0:7687"), false, false).is_err());
        assert!(validate_listener_security(address("[::]:7687"), false, false).is_err());
        assert!(validate_listener_security(address("192.0.2.10:7687"), false, false).is_err());
    }

    #[test]
    fn remote_listener_requires_auth_or_clear_development_opt_in() {
        assert!(validate_listener_security(address("0.0.0.0:7687"), true, false).is_ok());
        assert!(validate_listener_security(address("0.0.0.0:7687"), false, true).is_ok());
    }
}
