//! One RocksDB per database, with process-wide cache and memtable budget
//! (spec 1.1, 1.2, 1.5).

use std::path::Path;

use brainwire_core::codec::CodecError;
use brainwire_core::keys::KeyError;
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBCompressionType, Options,
    WriteBufferManager, DB,
};

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
    /// have depended on them, and such a transaction fails closed on its
    /// next write or dependency (spec 1.4). Production default undecided;
    /// tests use a small cap so the eviction path itself is reachable.
    pub conflict_history_keys: usize,
}

impl StoreConfig {
    pub fn for_tests() -> StoreConfig {
        StoreConfig {
            cache_bytes: 64 << 20,
            memtable_bytes: 16 << 20,
            id_block: 4,
            conflict_history_keys: 4,
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
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

pub struct Database {
    pub(crate) db: DB,
    pub(crate) committer: crate::txn::Committer,
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
        Ok(Database {
            db,
            committer: crate::txn::Committer::new(res.config.conflict_history_keys),
        })
    }

    pub(crate) fn cf(&self, name: &'static str) -> Result<&rocksdb::ColumnFamily, StoreError> {
        self.db
            .cf_handle(name)
            .ok_or(StoreError::MissingColumnFamily(name))
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
        Database {
            db,
            committer: crate::txn::Committer::new(conflict_history_keys),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
