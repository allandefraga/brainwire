//! Durable Bolt entry point for the named database registry.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::config;
use crate::databases::Databases;
use crate::engine::DbEngine;

pub fn run(explicit_config: Option<&Path>) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("config: {e}"))?;
    let implicit = config::find_file(&cwd);
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let loaded = config::load(explicit_config.or(implicit.as_deref()), &env)
        .map_err(|e| format!("config: {e}"))?;
    let max_message = usize::try_from(loaded.config.server.max_message.0)
        .map_err(|_| "server.max_message exceeds platform address size")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start server runtime: {e}"))?;
    runtime.block_on(async move {
        // Bind before opening the stores, so a port conflict does not leave a
        // partly initialized server that appears to be accepting queries.
        let listener = TcpListener::bind(&loaded.config.server.listen)
            .await
            .map_err(|e| format!("cannot bind {}: {e}", loaded.config.server.listen))?;
        let databases = Arc::new(Databases::open_all(&loaded.config)?);
        let default_name = loaded.config.server.default_database.clone();
        println!(
            "brainwire serve {} databases={:?} cache={} memtables={} query={} (per-RUN allocation guard; not RSS)",
            listener.local_addr().map_err(|e| e.to_string())?,
            databases.names(),
            loaded.config.memory.cache,
            loaded.config.memory.memtables,
            loaded.config.memory.query,
        );
        brainwire_bolt::server::serve_with_limit(
            listener,
            move || DbEngine::new(Arc::clone(&databases), default_name.clone())
                .expect("cannot start query worker"),
            max_message,
        )
        .await
        .map_err(|e| format!("Bolt server stopped: {e}"))
    })
}
