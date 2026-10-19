//! Named databases under one configured data directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use brainwire_store::db::{Database, Resources, StoreConfig};

use crate::config::Config;

pub struct Databases {
    data_dir: PathBuf,
    databases: Mutex<BTreeMap<String, Arc<Database>>>,
    // One cache and write-buffer budget is shared by every opened database.
    resources: Resources,
    query_limits: brainwire_cypher::ExecutionLimits,
    query_pool: Arc<brainwire_cypher::QueryMemoryPool>,
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

impl Databases {
    pub fn open_all(config: &Config) -> Result<Self, String> {
        if config
            .server
            .data_dir
            .join(".brainwire-cluster.json")
            .exists()
        {
            return Err(
                "cluster stores require serve --cluster-config; standalone access is refused"
                    .into(),
            );
        }
        Self::open(config, true)
    }

    pub(crate) fn open_replica(config: &Config) -> Result<Self, String> {
        Self::open(config, false)
    }

    fn open(config: &Config, discover: bool) -> Result<Self, String> {
        let data_dir = config.server.data_dir.clone();
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let resources = Resources::new(StoreConfig {
            cache_bytes: usize::try_from(config.memory.cache.0)
                .map_err(|_| "memory.cache exceeds platform address size")?,
            memtable_bytes: usize::try_from(config.memory.memtables.0)
                .map_err(|_| "memory.memtables exceeds platform address size")?,
            id_block: 1024,
            conflict_history_keys: 1_000_000,
        });
        let registry = Self {
            data_dir,
            databases: Mutex::new(BTreeMap::new()),
            resources,
            query_limits: brainwire_cypher::ExecutionLimits {
                timeout: Some(std::time::Duration::from_millis(
                    config.server.query_timeout_ms,
                )),
                memory_bytes: usize::try_from(config.memory.query.0)
                    .map_err(|_| "memory.query exceeds platform address size")?,
            },
            query_pool: Arc::new(brainwire_cypher::QueryMemoryPool::new(
                usize::try_from(config.memory.query.0)
                    .map_err(|_| "memory.query exceeds platform address size")?,
            )),
        };
        if !discover {
            return Ok(registry);
        }
        for entry in fs::read_dir(&registry.data_dir)
            .map_err(|e| format!("cannot list {}: {e}", registry.data_dir.display()))?
        {
            let entry = entry.map_err(|e| format!("cannot list database entry: {e}"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if valid_name(&name)
                && entry.file_type().map_err(|e| e.to_string())?.is_dir()
                && entry.path().join("CURRENT").is_file()
            {
                registry.create(&name)?;
            }
        }
        registry.create(&config.server.default_database)?;
        Ok(registry)
    }

    pub fn query_limits(&self) -> brainwire_cypher::ExecutionLimits {
        self.query_limits
    }
    pub fn query_pool(&self) -> Arc<brainwire_cypher::QueryMemoryPool> {
        Arc::clone(&self.query_pool)
    }

    pub fn memory_usage(&self) -> (usize, usize) {
        self.resources.memory_usage()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Database>> {
        self.databases.lock().ok()?.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.databases
            .lock()
            .expect("database registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    pub fn create(&self, name: &str) -> Result<Arc<Database>, String> {
        if !valid_name(name) {
            return Err(format!("invalid database name {name:?}"));
        }
        let mut databases = self
            .databases
            .lock()
            .map_err(|_| "database registry lock poisoned")?;
        if let Some(db) = databases.get(name) {
            return Ok(Arc::clone(db));
        }
        let path = self.data_dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_dir() || !path.join("CURRENT").is_file() => {
                return Err(format!("{} exists but is not a database", path.display()));
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("cannot inspect {}: {error}", path.display()));
            }
            _ => {}
        }
        let db = Arc::new(
            Database::open(&path, &self.resources)
                .map_err(|e| format!("cannot open database {name}: {e}"))?,
        );
        databases.insert(name.to_owned(), Arc::clone(&db));
        Ok(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(path: &std::path::Path) -> crate::config::Config {
        let mut config = crate::config::Config::default();
        config.server.data_dir = path.to_path_buf();
        config.memory.cache = crate::size::ByteSize(64 << 20);
        config.memory.memtables = crate::size::ByteSize(16 << 20);
        config
    }

    #[test]
    fn creates_default_and_reopens_existing_named_databases() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&dir.path().join("data"));
        let dbs = Databases::open_all(&config).unwrap();
        assert_eq!(dbs.names(), vec!["default"]);
        assert!(dbs.get("default").is_some());
        assert!(dbs.create("research").is_ok());
        assert_eq!(dbs.names(), vec!["default", "research"]);
        drop(dbs);
        let reopened = Databases::open_all(&config).unwrap();
        assert_eq!(reopened.names(), vec!["default", "research"]);
        assert!(reopened.get("research").is_some());
    }

    #[test]
    fn rejects_path_names_and_ignores_non_database_directories() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&dir.path().join("data"));
        std::fs::create_dir_all(config.server.data_dir.join("notes")).unwrap();
        let dbs = Databases::open_all(&config).unwrap();
        assert_eq!(dbs.names(), vec!["default"]);
        assert!(dbs.create("notes").is_err());
        for name in ["", ".", "..", "../escape", "a/b", "a\\b", " a", "a b"] {
            assert!(dbs.create(name).is_err(), "{name:?}");
        }
        assert!(!dir.path().join("escape").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_database_directory() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let config = config(&dir.path().join("data"));
        let outside = dir.path().join("outside");
        let resources = Resources::new(StoreConfig::for_tests());
        drop(Database::open(&outside, &resources).unwrap());
        std::fs::create_dir_all(&config.server.data_dir).unwrap();
        symlink(&outside, config.server.data_dir.join("linked")).unwrap();
        let registry = Databases::open_all(&config).unwrap();
        assert_eq!(registry.names(), vec!["default"]);
        assert!(registry.create("linked").is_err());
    }

    #[test]
    fn refuses_to_start_if_a_named_database_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&dir.path().join("data"));
        let broken = config.server.data_dir.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("CURRENT"), "invalid manifest\n").unwrap();
        let result = Databases::open_all(&config);
        assert!(matches!(result, Err(message) if message.contains("broken")));
    }
}
