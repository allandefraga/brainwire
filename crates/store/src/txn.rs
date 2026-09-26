//! Snapshot-isolated transactions with an in-house write overlay and a
//! committer that lets the first committer win (spec 1.4). The crate does not
//! expose WriteBatchWithIndex, hence the overlay.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use crate::db::{Database, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cf {
    Meta,
    Node,
    Pk,
    Out,
    In,
    Lbl,
    Idx,
    Ident,
    Event,
    Acl,
    Note,
}

impl Cf {
    pub fn name(self) -> &'static str {
        match self {
            Cf::Meta => "meta",
            Cf::Node => "node",
            Cf::Pk => "pk",
            Cf::Out => "out",
            Cf::In => "in",
            Cf::Lbl => "lbl",
            Cf::Idx => "idx",
            Cf::Ident => "ident",
            Cf::Event => "event",
            Cf::Acl => "acl",
            Cf::Note => "note",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TxnError {
    #[error("conflict with a transaction that committed first")]
    Conflict,
    #[error(transparent)]
    Store(#[from] StoreError),
}

type Key = (Cf, Vec<u8>);

#[derive(Default)]
pub(crate) struct Committer {
    state: Mutex<CommitState>,
}

#[derive(Default)]
struct CommitState {
    seq: u64,
    last_write: HashMap<Key, u64>,
    active: BTreeMap<u64, usize>,
}

impl CommitState {
    fn release(&mut self, start: u64) {
        if let Some(n) = self.active.get_mut(&start) {
            *n -= 1;
            if *n == 0 {
                self.active.remove(&start);
            }
        }
        // A key last written at or before the oldest active start cannot conflict any more.
        if self.last_write.len() > 100_000 {
            let horizon = self.active.keys().next().copied().unwrap_or(self.seq);
            self.last_write.retain(|_, s| *s > horizon);
        }
    }
}

pub struct Txn<'db> {
    db: &'db Database,
    snapshot: rocksdb::Snapshot<'db>,
    start_seq: u64,
    writes: BTreeMap<Key, Option<Vec<u8>>>,
    depends: BTreeSet<Key>,
    done: bool,
}

impl Database {
    pub fn begin(&self) -> Txn<'_> {
        let mut st = self.committer.state.lock().expect("committer lock");
        let start_seq = st.seq;
        *st.active.entry(start_seq).or_insert(0) += 1;
        // Taken under the lock, so no commit lands between the sequence and the snapshot.
        let snapshot = self.db.snapshot();
        drop(st);
        Txn {
            db: self,
            snapshot,
            start_seq,
            writes: BTreeMap::new(),
            depends: BTreeSet::new(),
            done: false,
        }
    }
}

impl<'db> Txn<'db> {
    pub fn database(&self) -> &'db Database {
        self.db
    }

    pub fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(v) = self.writes.get(&(cf, key.to_vec())) {
            return Ok(v.clone());
        }
        Ok(self.snapshot.get_cf(self.db.cf(cf.name())?, key)?)
    }

    pub fn get_for_update(&mut self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.depends.insert((cf, key.to_vec()));
        self.get(cf, key)
    }

    pub fn put(&mut self, cf: Cf, key: Vec<u8>, value: Vec<u8>) {
        self.writes.insert((cf, key), Some(value));
    }

    pub fn delete(&mut self, cf: Cf, key: Vec<u8>) {
        self.writes.insert((cf, key), None);
    }

    // The pair-of-byte-vectors return type is the brief's required interface
    // (spec 1.4); factoring it into a named alias is not worth doing for one
    // call site, so the complexity lint is silenced locally instead.
    #[allow(clippy::type_complexity)]
    pub fn scan_prefix(
        &self,
        cf: Cf,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let mut merged: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
        let handle = self.db.cf(cf.name())?;
        let mode = rocksdb::IteratorMode::From(prefix, rocksdb::Direction::Forward);
        for item in self.snapshot.iterator_cf(handle, mode) {
            let (k, v) = item?;
            if !k.starts_with(prefix) {
                break;
            }
            merged.insert(k.to_vec(), Some(v.to_vec()));
        }
        for ((c, k), v) in self.writes.range((cf, prefix.to_vec())..) {
            if *c != cf || !k.starts_with(prefix) {
                break;
            }
            merged.insert(k.clone(), v.clone());
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect())
    }

    pub fn commit(mut self) -> Result<u64, TxnError> {
        let db = self.db;
        let mut st = db.committer.state.lock().expect("committer lock");
        let start = self.start_seq;
        let conflict = self
            .writes
            .keys()
            .chain(self.depends.iter())
            .any(|k| st.last_write.get(k).is_some_and(|&s| s > start));
        if conflict {
            st.release(start);
            self.done = true;
            return Err(TxnError::Conflict);
        }
        if !self.writes.is_empty() {
            let mut batch = rocksdb::WriteBatch::default();
            for ((cf, key), value) in &self.writes {
                let handle = db.cf(cf.name())?;
                match value {
                    Some(v) => batch.put_cf(handle, key, v),
                    None => batch.delete_cf(handle, key),
                }
            }
            let mut opts = rocksdb::WriteOptions::default();
            opts.set_sync(true);
            db.db.write_opt(batch, &opts).map_err(StoreError::from)?;
            st.seq += 1;
            let seq = st.seq;
            for key in self.writes.keys() {
                st.last_write.insert(key.clone(), seq);
            }
        }
        let seq = st.seq;
        st.release(start);
        self.done = true;
        Ok(seq)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        if !self.done {
            if let Ok(mut st) = self.db.committer.state.lock() {
                st.release(self.start_seq);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, Resources, StoreConfig};

    fn open() -> (tempfile::TempDir, Resources, Database) {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        (dir, res, db)
    }

    #[test]
    fn reads_its_own_writes() {
        let (_d, _r, db) = open();
        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        assert_eq!(t.get(Cf::Node, b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn reads_come_from_the_snapshot() {
        let (_d, _r, db) = open();
        let reader = db.begin();
        let mut writer = db.begin();
        writer.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        writer.commit().unwrap();
        assert_eq!(reader.get(Cf::Node, b"a").unwrap(), None);
        assert_eq!(db.begin().get(Cf::Node, b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn first_committer_wins_on_a_dependency() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        assert_eq!(t1.get_for_update(Cf::Pk, b"user:7").unwrap(), None);
        assert_eq!(t2.get_for_update(Cf::Pk, b"user:7").unwrap(), None);
        t1.put(Cf::Pk, b"user:7".to_vec(), b"id1".to_vec());
        t2.put(Cf::Pk, b"user:7".to_vec(), b"id2".to_vec());
        t1.commit().unwrap();
        assert!(matches!(t2.commit(), Err(TxnError::Conflict)));
        assert_eq!(
            db.begin().get(Cf::Pk, b"user:7").unwrap(),
            Some(b"id1".to_vec())
        );
    }

    #[test]
    fn disjoint_writes_both_commit() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        t1.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        t2.put(Cf::Node, b"b".to_vec(), b"2".to_vec());
        t1.commit().unwrap();
        t2.commit().unwrap();
    }

    #[test]
    fn scan_merges_the_overlay_and_applies_deletes() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        seed.put(Cf::Out, b"p:1".to_vec(), b"x".to_vec());
        seed.put(Cf::Out, b"p:2".to_vec(), b"y".to_vec());
        seed.put(Cf::Out, b"q:1".to_vec(), b"z".to_vec());
        seed.commit().unwrap();
        let mut t = db.begin();
        t.delete(Cf::Out, b"p:1".to_vec());
        t.put(Cf::Out, b"p:3".to_vec(), b"w".to_vec());
        let got = t.scan_prefix(Cf::Out, b"p:").unwrap();
        assert_eq!(
            got,
            vec![
                (b"p:2".to_vec(), b"y".to_vec()),
                (b"p:3".to_vec(), b"w".to_vec())
            ]
        );
    }

    #[test]
    fn a_dropped_transaction_leaves_no_trace() {
        let (_d, _r, db) = open();
        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        drop(t);
        assert_eq!(db.begin().get(Cf::Node, b"a").unwrap(), None);
        let mut again = db.begin();
        again.put(Cf::Node, b"a".to_vec(), b"2".to_vec());
        again.commit().unwrap();
    }

    // Additional tests beyond the brief: the plan's carried-over review rule
    // requires every public function and every error variant to be exercised
    // by at least one test. None of the six tests above call `Txn::database`,
    // inspect what `commit`'s returned sequence actually means, or ever reach
    // `TxnError::Store` (as opposed to `TxnError::Conflict`), so they are
    // added here.

    #[test]
    fn database_returns_the_originating_database() {
        let (_d, _r, db) = open();
        let t = db.begin();
        assert!(std::ptr::eq(t.database(), &db));
    }

    #[test]
    fn commit_sequence_increases_on_writes_and_a_read_only_commit_is_a_no_op() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        t1.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        let seq1 = t1.commit().unwrap();

        // A transaction that writes nothing has nothing to order against
        // later commits, so it must not advance the shared sequence.
        let read_only = db.begin();
        let seq_noop = read_only.commit().unwrap();
        assert_eq!(seq_noop, seq1);

        let mut t2 = db.begin();
        t2.put(Cf::Node, b"b".to_vec(), b"2".to_vec());
        let seq2 = t2.commit().unwrap();
        assert!(seq2 > seq1);
    }

    #[test]
    fn a_failed_write_is_a_store_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        // Create every column family, then close the read-write handle so a
        // second, read-only handle can attach to the same path.
        drop(Database::open(dir.path(), &res).unwrap());

        let opts = rocksdb::Options::default();
        let cfs = crate::db::COLUMN_FAMILIES
            .iter()
            .map(|name| rocksdb::ColumnFamilyDescriptor::new(*name, rocksdb::Options::default()));
        let ro = rocksdb::DB::open_cf_descriptors_read_only(&opts, dir.path(), cfs, false).unwrap();
        let db = Database {
            db: ro,
            committer: Committer::default(),
        };

        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        match t.commit() {
            Err(TxnError::Store(_)) => {}
            other => {
                panic!("expected TxnError::Store for a write against a read-only db, got {other:?}")
            }
        }
    }
}
