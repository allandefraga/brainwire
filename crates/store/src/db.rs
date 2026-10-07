//! One RocksDB per database, with process-wide cache and memtable budget
//! (spec 1.1, 1.2, 1.5).

use std::collections::BTreeMap;
use std::path::Path;

use brainwire_core::codec::CodecError;
use brainwire_core::keys::KeyError;
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBCompressionType, Options,
    WriteBufferManager, DB,
};

/// Reserved metadata key; only the transaction committer may write it.
pub(crate) const COMMIT_SEQUENCE_KEY: &[u8] = b"commit/sequence";

pub(crate) fn load_commit_sequence(db: &DB) -> Result<Option<u64>, StoreError> {
    let cf = db
        .cf_handle("meta")
        .ok_or(StoreError::MissingColumnFamily("meta"))?;
    let Some(marker) = db.get_cf(cf, COMMIT_SEQUENCE_KEY)? else {
        return Ok(None);
    };
    if marker.len() != 9 || marker[0] != 1 {
        return Err(StoreError::Corrupt(
            "invalid commit sequence marker version or length".into(),
        ));
    }
    Ok(Some(u64::from_be_bytes(
        marker[1..].try_into().expect("validated marker length"),
    )))
}

pub const COLUMN_FAMILIES: [&str; 11] = [
    "meta", "node", "pk", "out", "in", "lbl", "idx", "ident", "event", "acl", "note",
];

#[derive(Debug, Clone, Copy)]
pub struct StoreConfig {
    pub cache_bytes: usize,
    pub memtable_bytes: usize,
    pub id_block: u64,
    /// Cap on the number of keys the transaction committer keeps in its
    /// conflict-detection history (`txn::Committer`). Once exceeded, the
    /// oldest commits are evicted even if a still-open transaction could
    /// have depended on them, and such a transaction fails closed at commit
    /// if it touched anything (spec 1.4). Production default is 1,000,000;
    /// `for_tests()` uses the same value, since a small cap can make a
    /// long-lived transaction conflict with its own id-block reservations
    /// (`alloc.rs`) — a test that needs to reach the eviction path sets its
    /// own small cap explicitly instead (see `txn.rs`'s
    /// `a_transaction_older_than_the_pruned_history_conflicts_on_any_touch`).
    pub conflict_history_keys: usize,
}

impl StoreConfig {
    pub fn for_tests() -> StoreConfig {
        StoreConfig {
            cache_bytes: 64 << 20,
            memtable_bytes: 16 << 20,
            id_block: 4,
            conflict_history_keys: 1_000_000,
        }
    }
}

/// Shared by every database of the process, so ten databases fit the budget of one.
pub struct Resources {
    pub(crate) config: StoreConfig,
    cache: Cache,
    write_buffers: WriteBufferManager,
}

impl Resources {
    /// Process-wide accounting; memtable reservations are included in cache usage.
    pub fn memory_usage(&self) -> (usize, usize) {
        (self.cache.get_usage(), self.write_buffers.get_usage())
    }

    pub fn new(config: StoreConfig) -> Resources {
        let cache = Cache::new_hyper_clock_cache(config.cache_bytes, 0);
        // allow_stall = false: one full family must not stall the other databases (spec 1.5).
        let write_buffers = WriteBufferManager::new_write_buffer_manager_with_cache(
            config.memtable_bytes,
            false,
            cache.clone(),
        );
        Resources {
            config,
            cache,
            write_buffers,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Rocks(#[from] rocksdb::Error),
    #[error("column family {0} is missing")]
    MissingColumnFamily(&'static str),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error("catalog: {0}")]
    Catalog(String),
    #[error("checkpoint: {0}")]
    Checkpoint(String),
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

pub struct Database {
    pub(crate) db: DB,
    pub(crate) committer: crate::txn::Committer,
    pub(crate) alloc: crate::alloc::IdAllocator,
    pub(crate) id_block: u64,
}

fn family_options(res: &Resources) -> Options {
    let mut block = BlockBasedOptions::default();
    block.set_block_cache(&res.cache);
    block.set_block_size(16 * 1024);
    let mut opts = Options::default();
    opts.set_block_based_table_factory(&block);
    opts.set_compression_type(DBCompressionType::Lz4);
    opts.set_bottommost_compression_type(DBCompressionType::Zstd);
    opts
}

impl Database {
    pub fn open(path: &Path, res: &Resources) -> Result<Database, StoreError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_write_buffer_manager(&res.write_buffers);
        let families = COLUMN_FAMILIES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, family_options(res)));
        let db = DB::open_cf_descriptors(&opts, path, families)?;
        let sequence = load_commit_sequence(&db)?;
        Ok(Database {
            db,
            committer: crate::txn::Committer::new(res.config.conflict_history_keys, sequence),
            alloc: crate::alloc::IdAllocator::default(),
            id_block: res.config.id_block,
        })
    }

    /// Native consistent checkpoint of committed data. Never overwrites a path.
    /// Parent must exist; destination cannot be nested inside the live database.
    /// SST hardlinks are immutable; this is not an off-machine backup.
    pub fn checkpoint(&self, destination: &Path) -> Result<(), StoreError> {
        self.checkpoint_with_sequence(destination).map(|_| ())
    }

    /// Creates a checkpoint and returns the durable sequence captured with its
    /// data. Commits are blocked during native checkpoint creation so the
    /// receipt cannot describe a newer or older store state.
    pub fn checkpoint_with_sequence(&self, destination: &Path) -> Result<Option<u64>, StoreError> {
        let error = |e: std::io::Error| StoreError::Checkpoint(e.to_string());
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = std::fs::canonicalize(parent).map_err(error)?;
        let source = std::fs::canonicalize(self.directory()).map_err(error)?;
        if parent.starts_with(&source) {
            return Err(StoreError::Checkpoint(
                "destination must be outside the source database".into(),
            ));
        }
        let name = destination.file_name().ok_or_else(|| {
            StoreError::Checkpoint("destination needs a new directory name".into())
        })?;
        let destination = parent.join(name);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(StoreError::Checkpoint(
                    "destination already exists; refusing overwrite".into(),
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(error(e)),
        }
        self.committer.checkpoint(&self.db, &destination)
    }

    pub fn directory(&self) -> &Path {
        self.db.path()
    }

    /// Current SST metadata from RocksDB; no graph records are scanned.
    pub fn live_sst_files(&self) -> Result<Vec<rocksdb::LiveFile>, StoreError> {
        Ok(self.db.live_files()?)
    }

    pub(crate) fn cf(&self, name: &'static str) -> Result<&rocksdb::ColumnFamily, StoreError> {
        self.db
            .cf_handle(name)
            .ok_or(StoreError::MissingColumnFamily(name))
    }

    /// Point-in-time RocksDB estimates per column family. These are engine
    /// properties, not exact graph counts or a complete process-RSS budget.
    pub fn storage_metrics(
        &self,
    ) -> Result<BTreeMap<&'static str, BTreeMap<&'static str, Option<u64>>>, StoreError> {
        const PROPERTIES: [(&str, &str); 4] = [
            ("estimated_keys", "rocksdb.estimate-num-keys"),
            ("live_data_bytes", "rocksdb.estimate-live-data-size"),
            ("memtable_bytes", "rocksdb.cur-size-all-mem-tables"),
            ("level0_files", "rocksdb.num-files-at-level0"),
        ];
        let mut families = BTreeMap::new();
        for name in COLUMN_FAMILIES {
            let cf = self.cf(name)?;
            let mut metrics = BTreeMap::new();
            for (label, property) in PROPERTIES {
                metrics.insert(label, self.db.property_int_value_cf(cf, property)?);
            }
            families.insert(name, metrics);
        }
        Ok(families)
    }
}

#[cfg(test)]
impl Database {
    /// Wraps an already-open RocksDB handle (for example opened read-only,
    /// to reach a write failure `Database::open` cannot produce) as a
    /// `Database`, without depending on its exact field set from outside
    /// this module — `txn.rs`'s tests used to build `Database { .. }`
    /// directly, which would need updating every time this struct gains a
    /// field (Task 8+).
    pub(crate) fn for_test(db: DB, conflict_history_keys: usize) -> Database {
        let sequence = load_commit_sequence(&db).expect("valid test database commit marker");
        Database {
            db,
            committer: crate::txn::Committer::new(conflict_history_keys, sequence),
            alloc: crate::alloc::IdAllocator::default(),
            id_block: StoreConfig::for_tests().id_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_reopens_committed_schema_indexes_and_counter_without_pending_writes() {
        use crate::graph::{check_invariants, find_by_indexed_property, merge_node};
        use brainwire_core::{schema::GraphMapping, value::Value};
        let dir = tempfile::tempdir().unwrap();
        let resources = Resources::new(StoreConfig::for_tests());
        let db = Database::open(&dir.path().join("source"), &resources).unwrap();
        let mapping:GraphMapping=serde_json::from_str(r#"{"version":"1","nodes":[{"label":"Item","properties":["n"],"indexed_fields":["n"]}],"edges":[]}"#).unwrap();
        let catalog = db.apply_ontology(&mapping).unwrap();
        let mut tx = db.begin();
        merge_node(
            &mut tx,
            &catalog,
            "Item",
            "committed",
            &BTreeMap::from([("n".into(), Value::Int(7))]),
        )
        .unwrap();
        tx.commit().unwrap();
        let mut pending = db.begin();
        merge_node(&mut pending, &catalog, "Item", "pending", &BTreeMap::new()).unwrap();
        let destination = dir.path().join("checkpoint");
        db.checkpoint(&destination).unwrap();
        assert!(db.checkpoint(&destination).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("broken-link");
            std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
            assert!(db.checkpoint(&link).is_err());
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
        }
        assert!(db.checkpoint(&db.directory().join("nested")).is_err());
        let mut tx = db.begin();
        merge_node(
            &mut tx,
            &catalog,
            "Item",
            "committed",
            &BTreeMap::from([("n".into(), Value::Int(8))]),
        )
        .unwrap();
        tx.commit().unwrap();
        drop(pending);
        drop(db);
        let restored = Database::open(&destination, &resources).unwrap();
        assert_eq!(restored.stats().unwrap().nodes, 1);
        let catalog = restored.catalog().unwrap().unwrap();
        assert_eq!(
            find_by_indexed_property(
                &restored.begin(),
                &catalog.node_types[0],
                "n",
                &Value::Int(7)
            )
            .unwrap()
            .unwrap()
            .len(),
            1
        );
        check_invariants(&restored).unwrap();
        let mut tx = restored.begin();
        merge_node(&mut tx, &catalog, "Item", "restored-new", &BTreeMap::new()).unwrap();
        tx.commit().unwrap();
        check_invariants(&restored).unwrap();
        assert_eq!(restored.stats().unwrap().nodes, 2);
    }

    #[test]
    fn creates_every_column_family() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        drop(Database::open(dir.path(), &res).unwrap());
        let names = rocksdb::DB::list_cf(&rocksdb::Options::default(), dir.path()).unwrap();
        for cf in COLUMN_FAMILIES {
            assert!(names.iter().any(|n| n == cf), "missing {cf}");
        }
    }

    #[test]
    fn data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        {
            let db = Database::open(dir.path(), &res).unwrap();
            db.db.put_cf(db.cf("node").unwrap(), b"k", b"v").unwrap();
        }
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(
            db.db.get_cf(db.cf("node").unwrap(), b"k").unwrap(),
            Some(b"v".to_vec())
        );
    }

    #[test]
    fn two_databases_share_one_resources() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let res = Resources::new(StoreConfig::for_tests());
        let da = Database::open(a.path(), &res).unwrap();
        let db = Database::open(b.path(), &res).unwrap();
        da.db.put_cf(da.cf("meta").unwrap(), b"x", b"1").unwrap();
        assert_eq!(db.db.get_cf(db.cf("meta").unwrap(), b"x").unwrap(), None);
    }

    #[test]
    fn storage_metrics_report_every_column_family_without_claiming_exact_counts() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let metrics = db.storage_metrics().unwrap();
        assert_eq!(metrics.len(), COLUMN_FAMILIES.len());
        for name in COLUMN_FAMILIES {
            assert!(metrics[name].contains_key("estimated_keys"));
            assert!(metrics[name].contains_key("live_data_bytes"));
            assert!(metrics[name].contains_key("memtable_bytes"));
            assert!(metrics[name].contains_key("level0_files"));
        }
    }

    // Additional tests beyond the brief: the plan's carried-over review rule
    // requires every public function and every error variant to be exercised
    // by at least one test. The three tests above never hit
    // `MissingColumnFamily`, `Codec`, `Key`, `Catalog` or `Corrupt`, and never
    // prove that a path which cannot be opened returns an error instead of
    // panicking, so they are added here.

    #[test]
    fn missing_column_family_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        match db.cf("nonexistent") {
            Err(StoreError::MissingColumnFamily(name)) => assert_eq!(name, "nonexistent"),
            Err(other) => panic!("expected MissingColumnFamily, got {other}"),
            Ok(_) => panic!("expected an error for a nonexistent column family"),
        }
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_a_store_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the database directory should be: RocksDB
        // cannot create or open a DB there.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        match Database::open(&blocked, &res) {
            Err(StoreError::Rocks(_)) => {}
            Err(other) => panic!("expected StoreError::Rocks, got {other}"),
            Ok(_) => panic!("expected opening a blocked path to fail"),
        }
    }

    #[test]
    fn resources_stores_the_config_it_was_given() {
        let cfg = StoreConfig::for_tests();
        let res = Resources::new(cfg);
        assert_eq!(res.config.cache_bytes, cfg.cache_bytes);
        assert_eq!(res.config.memtable_bytes, cfg.memtable_bytes);
        assert_eq!(res.config.id_block, cfg.id_block);
        assert_eq!(res.config.conflict_history_keys, cfg.conflict_history_keys);
    }

    #[test]
    fn codec_error_converts_into_store_error() {
        let err: StoreError = CodecError::Truncated.into();
        assert!(matches!(err, StoreError::Codec(CodecError::Truncated)));
        assert_eq!(err.to_string(), "row is truncated");
    }

    #[test]
    fn key_error_converts_into_store_error() {
        let err: StoreError = KeyError::Truncated.into();
        assert!(matches!(err, StoreError::Key(KeyError::Truncated)));
        assert_eq!(err.to_string(), "key is truncated");
    }

    #[test]
    fn catalog_error_carries_its_message() {
        let err = StoreError::Catalog("bad graph_mapping".to_string());
        assert_eq!(err.to_string(), "catalog: bad graph_mapping");
    }

    #[test]
    fn corrupt_error_carries_its_message() {
        let err = StoreError::Corrupt("bad row".to_string());
        assert_eq!(err.to_string(), "corrupt data: bad row");
    }
}
