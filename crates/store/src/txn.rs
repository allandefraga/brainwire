//! Snapshot-isolated transactions with an in-house write overlay and a
//! committer that lets the first committer win (spec 1.4). The crate does not
//! expose WriteBatchWithIndex, hence the overlay. The committer's conflict
//! history is bounded (`StoreConfig::conflict_history_keys`): once it holds
//! more keys than the cap, the oldest commits are evicted even if a
//! still-open transaction could have depended on them. `pruned_through`
//! tracks the highest commit sequence ever evicted — on every eviction, safe
//! or not, and on recovery from a poisoned lock — so a transaction old
//! enough to predate it fails closed (`Conflict`) at commit if it touched
//! anything, instead of silently skipping a check it can no longer make.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, MutexGuard};

use crate::db::{Database, StoreError, COMMIT_SEQUENCE_KEY};

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

pub(crate) struct Committer {
    state: Mutex<CommitState>,
}

impl Committer {
    pub(crate) fn new(conflict_history_keys: usize, sequence: Option<u64>) -> Committer {
        Committer {
            state: Mutex::new(CommitState {
                cap: conflict_history_keys,
                seq: sequence.unwrap_or(0),
                tracked: sequence.is_some(),
                ..CommitState::default()
            }),
        }
    }

    pub(crate) fn checkpoint(
        &self,
        db: &rocksdb::DB,
        destination: &std::path::Path,
    ) -> Result<Option<u64>, StoreError> {
        let st = self.lock();
        rocksdb::checkpoint::Checkpoint::new(db)?.create_checkpoint(destination)?;
        Ok(st.tracked.then_some(st.seq))
    }

    /// The one place that locks the committer. A poisoned lock means an
    /// earlier holder panicked partway through updating `last_write` /
    /// `history` for a commit, so that update can no longer be trusted for
    /// any key it might or might not have finished recording. Recovering
    /// raises `pruned_through` to the sequence at the time of poisoning —
    /// the same fail-closed-at-commit mechanism every eviction uses (see
    /// `CommitState::prune`) — so every transaction old enough to have
    /// possibly missed that update retries. It then clears the poison flag:
    /// `PoisonError::into_inner` recovers the guard but leaves the `Mutex`
    /// marked poisoned, and left set, this whole recovery — including the
    /// `pruned_through` bump, against whatever `seq` has become by then —
    /// would rerun on every later `begin`, `commit` and `drop` instead of
    /// once, eventually raising `pruned_through` past transactions that
    /// never had anything to do with the original panic.
    fn lock(&self) -> MutexGuard<'_, CommitState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            let mut st = poisoned.into_inner();
            st.pruned_through = st.pruned_through.max(st.seq);
            self.state.clear_poison();
            st
        })
    }
}

#[derive(Default)]
struct CommitState {
    seq: u64,
    tracked: bool,
    // BTreeMap, not HashMap: a prefix dependency (`Txn::scan_prefix_for_update`)
    // needs a range scan over "every key of this Cf starting with this
    // prefix", which only an ordered map gives cheaply.
    last_write: BTreeMap<Key, u64>,
    /// Committed keys, ordered by commit sequence, so pruning removes an
    /// exact prefix instead of rescanning `last_write` in full.
    history: VecDeque<(u64, Vec<Key>)>,
    /// Running total of keys held across `history`, kept in step with it so
    /// checking the cap never has to sum the deque.
    history_keys: usize,
    active: BTreeMap<u64, usize>,
    /// The highest commit sequence ever evicted from `history`: raised on
    /// every eviction in `prune` (whether the entry was safe to drop or
    /// not) and on recovery from a poisoned lock (`Committer::lock`). A
    /// transaction that began before this and touched anything can no
    /// longer be checked precisely, so `Txn::commit` fails it closed
    /// instead of trusting a check it can't fully make.
    pruned_through: u64,
    cap: usize,
}

impl CommitState {
    fn release(&mut self, start: u64) {
        if let Some(n) = self.active.get_mut(&start) {
            *n -= 1;
            if *n == 0 {
                self.active.remove(&start);
            }
        }
        self.prune();
    }

    /// Evicts history in commit-sequence order: first everything at or below
    /// the oldest active start (safe — conflict needs a commit sequence
    /// strictly after a transaction's start, so no active or future
    /// transaction can conflict on an entry this old), then, if the cap is
    /// still exceeded, the oldest remaining entries regardless (unsafe: some
    /// active transaction might have depended on one of them). Every
    /// eviction here, safe or not, raises `pruned_through` — harmless for
    /// the safe ones, since by construction no active transaction's start
    /// can be below them anyway — but it's the unsafe branch that actually
    /// matters: only it can push `pruned_through` past a still-active
    /// transaction's start, which is what makes that transaction fail
    /// closed at commit instead of silently passing a check that can no
    /// longer be made precisely.
    fn prune(&mut self) {
        let horizon = self.active.keys().next().copied().unwrap_or(self.seq);
        while let Some(&(front_seq, _)) = self.history.front() {
            if front_seq > horizon && self.history_keys <= self.cap {
                break;
            }
            let (commit_seq, keys) = self
                .history
                .pop_front()
                .expect("front() returned Some above");
            self.history_keys -= keys.len();
            for key in &keys {
                if self.last_write.get(key) == Some(&commit_seq) {
                    self.last_write.remove(key);
                }
            }
            self.pruned_through = self.pruned_through.max(commit_seq);
        }
    }
}

pub struct Txn<'db> {
    db: &'db Database,
    snapshot: rocksdb::Snapshot<'db>,
    start_seq: u64,
    snapshot_commit_sequence: Option<u64>,
    writes: BTreeMap<Key, Option<Vec<u8>>>,
    depends: BTreeSet<Key>,
    /// Range dependencies recorded by `scan_prefix_for_update`: (cf, prefix)
    /// pairs such that a write landing anywhere under that prefix, by a
    /// transaction that commits first, conflicts this one — including a key
    /// that did not exist yet when this transaction scanned (a phantom).
    depends_prefix: BTreeSet<Key>,
    done: bool,
    estimated_bytes: usize,
    pub(crate) replicated_ids: bool,
    pub(crate) used_standalone_ids: bool,
    replication_valid: bool,
}

impl Database {
    /// Latest durable per-store commit sequence. Missing legacy metadata is
    /// untracked until the first successful write; reads never create a marker.
    pub fn commit_sequence(&self) -> Option<u64> {
        let st = self.committer.lock();
        st.tracked.then_some(st.seq)
    }

    pub fn begin(&self) -> Txn<'_> {
        let mut st = self.committer.lock();
        let start_seq = st.seq;
        let snapshot_commit_sequence = st.tracked.then_some(start_seq);
        *st.active.entry(start_seq).or_insert(0) += 1;
        // Taken under the lock, so no commit lands between the sequence and the snapshot.
        let snapshot = self.db.snapshot();
        drop(st);
        Txn {
            db: self,
            snapshot,
            start_seq,
            snapshot_commit_sequence,
            writes: BTreeMap::new(),
            depends: BTreeSet::new(),
            depends_prefix: BTreeSet::new(),
            done: false,
            estimated_bytes: 0,
            replicated_ids: false,
            used_standalone_ids: false,
            replication_valid: true,
        }
    }
}

impl<'db> Txn<'db> {
    /// Opt into staged ID counters before any mutation or standalone allocation.
    /// Replicated writes must be prepared, never committed directly.
    pub fn with_replicated_ids(mut self) -> Self {
        self.replication_valid &= self.writes.is_empty() && !self.used_standalone_ids;
        self.replicated_ids = true;
        self
    }

    pub fn database(&self) -> &'db Database {
        self.db
    }

    /// Durable sequence captured with this transaction's snapshot at begin.
    /// Pending writes and later commits do not change it.
    pub fn snapshot_commit_sequence(&self) -> Option<u64> {
        self.snapshot_commit_sequence
    }

    /// Conservative O(1) overlay/dependency estimate, excluding RocksDB internals.
    pub fn estimated_memory_bytes(&self) -> usize {
        self.estimated_bytes
    }

    pub fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if let Some(v) = self.writes.get(&(cf, key.to_vec())) {
            return Ok(v.clone());
        }
        Ok(self.snapshot.get_cf(self.db.cf(cf.name())?, key)?)
    }

    /// Visit one value without copying it before the caller can account for
    /// decoding. Pending writes (including deletes) take precedence over the
    /// transaction's pinned RocksDB snapshot.
    pub fn with_get<E: From<StoreError>, R>(
        &self,
        cf: Cf,
        key: &[u8],
        visit: impl FnOnce(Option<&[u8]>) -> Result<R, E>,
    ) -> Result<R, E> {
        if let Some(value) = self.writes.get(&(cf, key.to_vec())) {
            return visit(value.as_deref());
        }
        let value = self
            .snapshot
            .get_pinned_cf(self.db.cf(cf.name())?, key)
            .map_err(StoreError::from)?;
        visit(value.as_deref())
    }

    pub fn get_for_update(&mut self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if self.depends.insert((cf, key.to_vec())) {
            self.estimated_bytes = self.estimated_bytes.saturating_add(128 + key.len());
        }
        self.get(cf, key)
    }

    pub fn put(&mut self, cf: Cf, key: Vec<u8>, value: Vec<u8>) {
        self.stage(cf, key, Some(value));
    }

    pub fn delete(&mut self, cf: Cf, key: Vec<u8>) {
        self.stage(cf, key, None);
    }

    fn stage(&mut self, cf: Cf, key: Vec<u8>, value: Option<Vec<u8>>) {
        let bytes = value.as_ref().map_or(0, Vec::len);
        let key_bytes = key.len();
        match self.writes.insert((cf, key), value) {
            Some(old) => {
                self.estimated_bytes = self
                    .estimated_bytes
                    .saturating_sub(old.as_ref().map_or(0, Vec::len))
                    .saturating_add(bytes);
            }
            None => {
                self.estimated_bytes = self
                    .estimated_bytes
                    .saturating_add(192 + key_bytes)
                    .saturating_add(bytes);
            }
        }
    }
    fn depend_prefix(&mut self, cf: Cf, prefix: &[u8]) {
        if self.depends_prefix.insert((cf, prefix.to_vec())) {
            self.estimated_bytes = self.estimated_bytes.saturating_add(128 + prefix.len());
        }
    }

    /// Visit one prefix in key order, merging the snapshot with this
    /// transaction's overlay without copying/materializing all stored values.
    /// Callback errors stop the scan immediately. This is a snapshot read,
    /// not a serializable range dependency.
    pub fn visit_prefix<E: From<StoreError>>(
        &self,
        cf: Cf,
        prefix: &[u8],
        visit: impl FnMut(&[u8], &[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.visit_matching(cf, prefix, |key| key.starts_with(prefix), visit)
    }

    /// Stream a half-open key range, including this snapshot's pending writes.
    pub fn visit_range<E: From<StoreError>>(
        &self,
        cf: Cf,
        lo: &[u8],
        hi: &[u8],
        visit: impl FnMut(&[u8], &[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        if lo >= hi {
            return Ok(());
        }
        self.visit_matching(cf, lo, |key| key < hi, visit)
    }

    fn visit_matching<E: From<StoreError>>(
        &self,
        cf: Cf,
        lo: &[u8],
        in_range: impl Fn(&[u8]) -> bool,
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut disk = self.snapshot.raw_iterator_cf(self.db.cf(cf.name())?);
        disk.seek(lo);
        let mut overlay = self
            .writes
            .range((cf, lo.to_vec())..)
            .take_while(|((family, key), _)| *family == cf && in_range(key))
            .peekable();
        loop {
            disk.status().map_err(StoreError::from)?;
            let disk_key = disk.key().filter(|key| in_range(key));
            let staged = overlay.peek();
            match (disk_key, staged) {
                (None, None) => break,
                (Some(key), Some(((_, staged_key), _))) if key < staged_key.as_slice() => {
                    visit(key, disk.value().expect("valid iterator"))?;
                    disk.next();
                }
                (Some(key), None) => {
                    visit(key, disk.value().expect("valid iterator"))?;
                    disk.next();
                }
                (key, Some(((_, staged_key), value))) => {
                    let replace = key == Some(staged_key.as_slice());
                    if let Some(value) = value {
                        visit(staged_key, value)?;
                    }
                    overlay.next();
                    if replace {
                        disk.next();
                    }
                }
            }
        }
        Ok(())
    }

    /// Stream a prefix while tracking concurrent inserts, updates and deletes.
    pub fn visit_prefix_for_update<E: From<StoreError>>(
        &mut self,
        cf: Cf,
        prefix: &[u8],
        visit: impl FnMut(&[u8], &[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        self.depend_prefix(cf, prefix);
        self.visit_prefix(cf, prefix, visit)
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

    /// Reads the half-open key range from this transaction's snapshot and
    /// applies its pending writes and deletes in key order.
    #[allow(clippy::type_complexity)]
    pub fn scan_range(
        &self,
        cf: Cf,
        lo: &[u8],
        hi: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        if lo >= hi {
            return Ok(Vec::new());
        }

        let mut merged: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
        let handle = self.db.cf(cf.name())?;
        let mut options = rocksdb::ReadOptions::default();
        options.set_iterate_upper_bound(hi);
        let mode = rocksdb::IteratorMode::From(lo, rocksdb::Direction::Forward);
        for item in self.snapshot.iterator_cf_opt(handle, options, mode) {
            let (key, value) = item?;
            merged.insert(key.to_vec(), Some(value.to_vec()));
        }
        for ((_, key), value) in self.writes.range((cf, lo.to_vec())..(cf, hi.to_vec())) {
            merged.insert(key.clone(), value.clone());
        }
        Ok(merged
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect())
    }

    /// A range read that is also a dependency: unlike `scan_prefix`, a key
    /// under `prefix` written by a transaction that commits first makes this
    /// one conflict — even a key this snapshot never saw, because it did not
    /// exist yet (a phantom). Callers that scan a range to decide what to
    /// delete or fold over, and need that decision to still hold at commit
    /// time, should use this or `visit_prefix_for_update` instead of a plain
    /// `scan_prefix`.
    #[allow(clippy::type_complexity)]
    pub fn scan_prefix_for_update(
        &mut self,
        cf: Cf,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        self.depend_prefix(cf, prefix);
        self.scan_prefix(cf, prefix)
    }

    fn has_conflict(&self, st: &CommitState) -> bool {
        let start = self.start_seq;
        let touched_anything =
            !self.writes.is_empty() || !self.depends.is_empty() || !self.depends_prefix.is_empty();
        // A transaction old enough to predate `pruned_through` can no longer
        // be checked precisely — some eviction (safe or not) or a poison
        // recovery might have removed the one entry that mattered — so fail
        // it closed rather than silently skip the check. Cheap and broad,
        // so it runs before the precise per-key scan.
        (touched_anything && start < st.pruned_through)
            || self
                .writes
                .keys()
                .chain(self.depends.iter())
                .any(|k| st.last_write.get(k).is_some_and(|&s| s > start))
            || self.depends_prefix.iter().any(|(cf, prefix)| {
                st.last_write
                    .range((*cf, prefix.clone())..)
                    .take_while(|((c, k), _)| c == cf && k.starts_with(prefix))
                    .any(|(_, &s)| s > start)
            })
    }

    /// Move trusted overlay mutations into a canonical entry after MVCC validation.
    /// No local write or network operation occurs. The leader must serialize the
    /// interval between preparation and quorum/apply; revision CAS rejects stale entries.
    pub fn prepare_replication(
        mut self,
        limits: crate::replicated_apply::ApplyLimits,
    ) -> Result<Option<crate::replicated_apply::Batch>, crate::replicated_apply::ApplyError> {
        use crate::replicated_apply::{ApplyError, Batch, Mutation};
        if !self.replicated_ids || !self.replication_valid {
            return Err(ApplyError::Invalid(
                "replicated IDs must be enabled before writes",
            ));
        }
        if self.writes.is_empty() {
            return Ok(None);
        }
        let current = crate::replicated_apply::applied_state_in(&mut self)?;
        let index = current.map_or(Ok(1), |state| {
            state
                .index
                .checked_add(1)
                .ok_or(ApplyError::Invalid("apply revision exhausted"))
        })?;
        let mut st = self.db.committer.lock();
        if self.has_conflict(&st) {
            return Err(TxnError::Conflict.into());
        }
        let writes = std::mem::take(&mut self.writes);
        let batch = Batch {
            version: 1,
            index,
            mutations: writes
                .into_iter()
                .map(|((cf, key), value)| Mutation {
                    cf: cf.name().into(),
                    key,
                    value,
                })
                .collect(),
        };
        // Validate bounds/reserved metadata without persisting anything.
        batch.validate(limits)?;
        st.release(self.start_seq);
        self.done = true;
        Ok(Some(batch))
    }

    pub fn commit(mut self) -> Result<u64, TxnError> {
        if self.replicated_ids && !self.writes.is_empty() {
            return Err(
                StoreError::Corrupt("replicated write requires quorum preparation".into()).into(),
            );
        }
        let db = self.db;
        let mut st = db.committer.lock();
        let start = self.start_seq;
        if self
            .writes
            .contains_key(&(Cf::Meta, COMMIT_SEQUENCE_KEY.to_vec()))
        {
            return Err(StoreError::Corrupt("commit sequence metadata is reserved".into()).into());
        }
        let conflict = self.has_conflict(&st);
        if conflict {
            st.release(start);
            self.done = true;
            return Err(TxnError::Conflict);
        }
        if !self.writes.is_empty() {
            let seq = st
                .seq
                .checked_add(1)
                .ok_or_else(|| StoreError::Corrupt("commit sequence exhausted".into()))?;
            let mut batch = rocksdb::WriteBatch::default();
            for ((cf, key), value) in &self.writes {
                let handle = db.cf(cf.name())?;
                match value {
                    Some(v) => batch.put_cf(handle, key, v),
                    None => batch.delete_cf(handle, key),
                }
            }
            // Keep this internal write outside self.writes: ordinary disjoint
            // writers must not implicitly depend on the shared marker.
            let mut marker = [0; 9];
            marker[0] = 1;
            marker[1..].copy_from_slice(&seq.to_be_bytes());
            batch.put_cf(db.cf("meta")?, COMMIT_SEQUENCE_KEY, marker);
            let mut opts = rocksdb::WriteOptions::default();
            opts.set_sync(true);
            db.db.write_opt(batch, &opts).map_err(StoreError::from)?;
            st.seq = seq;
            st.tracked = true;
            let mut keys: Vec<Key> = self.writes.keys().cloned().collect();
            // Explicit point/prefix dependencies on metadata still observe it.
            keys.push((Cf::Meta, COMMIT_SEQUENCE_KEY.to_vec()));
            for key in &keys {
                st.last_write.insert(key.clone(), seq);
            }
            st.history_keys += keys.len();
            st.history.push_back((seq, keys));
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
            self.db.committer.lock().release(self.start_seq);
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
    fn memory_estimate_tracks_replacement_tombstones_and_unique_dependencies() {
        let (_dir, _res, db) = open();
        let mut tx = db.begin();
        assert_eq!(tx.estimated_memory_bytes(), 0);
        tx.put(Cf::Node, b"key".to_vec(), vec![0; 100]);
        let initial = tx.estimated_memory_bytes();
        tx.put(Cf::Node, b"key".to_vec(), vec![0; 20]);
        assert_eq!(tx.estimated_memory_bytes(), initial - 80);
        tx.delete(Cf::Node, b"key".to_vec());
        assert_eq!(tx.estimated_memory_bytes(), initial - 100);
        tx.get_for_update(Cf::Node, b"key").unwrap();
        let dependent = tx.estimated_memory_bytes();
        tx.get_for_update(Cf::Node, b"key").unwrap();
        assert_eq!(tx.estimated_memory_bytes(), dependent);
        tx.visit_prefix_for_update::<StoreError>(Cf::Out, b"x", |_, _| Ok(()))
            .unwrap();
        let range = tx.estimated_memory_bytes();
        tx.scan_prefix_for_update(Cf::Out, b"x").unwrap();
        assert_eq!(tx.estimated_memory_bytes(), range);
        assert!(range > dependent);
    }

    #[test]
    fn range_visitor_matches_eager_scan_with_overlay_and_stops_on_error() {
        let (_dir, _res, db) = open();
        let mut seed = db.begin();
        for key in [b"a", b"b", b"d", b"z"] {
            seed.put(Cf::Node, key.to_vec(), vec![1; 1024]);
        }
        seed.commit().unwrap();
        let mut tx = db.begin();
        tx.delete(Cf::Node, b"b".to_vec());
        tx.put(Cf::Node, b"c".to_vec(), b"new".to_vec());
        tx.put(Cf::Node, b"d".to_vec(), b"replacement".to_vec());
        tx.put(Cf::Pk, b"c".to_vec(), b"other-cf".to_vec());
        let mut external = db.begin();
        external.put(Cf::Node, b"e".to_vec(), b"invisible".to_vec());
        external.commit().unwrap();
        for (lo, hi) in [
            (b"b".as_slice(), b"z".as_slice()),
            (b"b", b"b"),
            (b"z", b"a"),
        ] {
            let mut found = Vec::new();
            tx.visit_range::<StoreError>(Cf::Node, lo, hi, |k, v| {
                found.push((k.to_vec(), v.to_vec()));
                Ok(())
            })
            .unwrap();
            assert_eq!(found, tx.scan_range(Cf::Node, lo, hi).unwrap());
        }
        let mut visits = 0;
        assert!(tx
            .visit_range::<StoreError>(Cf::Node, b"a", b"z", |_, _| {
                visits += 1;
                Err(StoreError::Corrupt("stop".into()))
            })
            .is_err());
        assert_eq!(visits, 1);
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
    fn borrowed_point_read_merges_snapshot_overlay_and_tombstones() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        seed.put(Cf::Node, b"stored".to_vec(), b"snapshot".to_vec());
        seed.commit().unwrap();

        let mut reader = db.begin();
        let mut writer = db.begin();
        writer.put(Cf::Node, b"stored".to_vec(), b"later".to_vec());
        writer.put(Cf::Node, b"new".to_vec(), b"overlay".to_vec());
        writer.commit().unwrap();

        let stored =
            reader
                .with_get::<StoreError, _>(Cf::Node, b"stored", |value| {
                    Ok(value.map(ToOwned::to_owned))
                })
                .unwrap();
        assert_eq!(stored, Some(b"snapshot".to_vec()));
        reader.put(Cf::Node, b"stored".to_vec(), b"replacement".to_vec());
        reader.delete(Cf::Node, b"new".to_vec());
        assert_eq!(
            reader
                .with_get::<StoreError, _>(Cf::Node, b"stored", |value| {
                    Ok(value.map(ToOwned::to_owned))
                })
                .unwrap(),
            Some(b"replacement".to_vec())
        );
        assert_eq!(
            reader
                .with_get::<StoreError, _>(Cf::Node, b"new", |value| {
                    Ok(value.map(ToOwned::to_owned))
                })
                .unwrap(),
            None
        );
        reader.delete(Cf::Node, b"stored".to_vec());
        assert_eq!(
            reader
                .with_get::<StoreError, _>(Cf::Node, b"stored", |value| {
                    Ok(value.map(ToOwned::to_owned))
                })
                .unwrap(),
            None
        );
        assert_eq!(
            reader
                .with_get::<StoreError, _>(Cf::Node, b"missing", |value| {
                    Ok(value.map(ToOwned::to_owned))
                })
                .unwrap(),
            None
        );
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
    fn visitor_merges_updates_deletes_insertions_and_respects_snapshot_and_prefix() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        for key in [b"p:1", b"p:3", b"p:5", b"q:1"] {
            seed.put(Cf::Out, key.to_vec(), b"old".to_vec());
        }
        seed.commit().unwrap();
        let mut tx = db.begin();
        let mut external = db.begin();
        external.put(Cf::Out, b"p:4".to_vec(), b"invisible".to_vec());
        external.commit().unwrap();
        tx.delete(Cf::Out, b"p:1".to_vec());
        tx.put(Cf::Out, b"p:0".to_vec(), b"first".to_vec());
        tx.put(Cf::Out, b"p:3".to_vec(), b"replacement".to_vec());
        tx.put(Cf::Out, b"p:9".to_vec(), b"last".to_vec());
        tx.put(Cf::In, b"p:2".to_vec(), b"other-cf".to_vec());
        for prefix in [b"p:".as_slice(), b"", b"missing"] {
            let mut visited = vec![];
            tx.visit_prefix::<StoreError>(Cf::Out, prefix, |k, v| {
                visited.push((k.to_vec(), v.to_vec()));
                Ok(())
            })
            .unwrap();
            assert_eq!(visited, tx.scan_prefix(Cf::Out, prefix).unwrap());
        }
        let mut visits = 0;
        assert!(tx
            .visit_prefix::<StoreError>(Cf::Out, b"", |_, _| {
                visits += 1;
                Err(StoreError::Corrupt("stop".into()))
            })
            .is_err());
        assert_eq!(visits, 1);
    }

    #[test]
    fn scan_range_merges_snapshot_and_overlay_with_exclusive_upper_bound() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        for (key, value) in [
            (b"a".as_slice(), b"before".as_slice()),
            (b"b", b"old"),
            (b"c", b"removed"),
            (b"e", b"upper"),
            (b"f", b"after"),
        ] {
            seed.put(Cf::Node, key.to_vec(), value.to_vec());
        }
        seed.put(Cf::Pk, b"b".to_vec(), b"other-cf".to_vec());
        seed.commit().unwrap();

        let mut txn = db.begin();
        txn.put(Cf::Node, b"b".to_vec(), b"new".to_vec());
        txn.delete(Cf::Node, b"c".to_vec());
        txn.put(Cf::Node, b"d".to_vec(), b"inserted".to_vec());
        txn.put(Cf::Node, b"e".to_vec(), b"outside".to_vec());
        txn.put(Cf::Pk, b"d".to_vec(), b"other-cf".to_vec());

        assert_eq!(
            txn.scan_range(Cf::Node, b"b", b"e").unwrap(),
            vec![
                (b"b".to_vec(), b"new".to_vec()),
                (b"d".to_vec(), b"inserted".to_vec()),
            ]
        );
        assert!(txn.scan_range(Cf::Node, b"e", b"e").unwrap().is_empty());
        assert!(txn.scan_range(Cf::Node, b"f", b"b").unwrap().is_empty());
    }

    #[test]
    fn scan_range_uses_the_transaction_snapshot() {
        let (_d, _r, db) = open();
        let mut seed = db.begin();
        seed.put(Cf::Node, b"b".to_vec(), b"initial".to_vec());
        seed.commit().unwrap();

        let mut reader = db.begin();
        let mut writer = db.begin();
        writer.put(Cf::Node, b"b".to_vec(), b"later".to_vec());
        writer.put(Cf::Node, b"c".to_vec(), b"later".to_vec());
        writer.commit().unwrap();

        assert_eq!(
            reader.scan_range(Cf::Node, b"b", b"d").unwrap(),
            vec![(b"b".to_vec(), b"initial".to_vec())]
        );
        reader.put(Cf::Node, b"c".to_vec(), b"own".to_vec());
        assert_eq!(
            reader.scan_range(Cf::Node, b"b", b"d").unwrap(),
            vec![
                (b"b".to_vec(), b"initial".to_vec()),
                (b"c".to_vec(), b"own".to_vec()),
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
        let writable = Database::open(dir.path(), &res).unwrap();
        let mut seed = writable.begin();
        seed.put(Cf::Node, b"seed".to_vec(), b"committed".to_vec());
        assert_eq!(seed.commit().unwrap(), 1);
        drop(writable);

        let opts = rocksdb::Options::default();
        let cfs = crate::db::COLUMN_FAMILIES
            .iter()
            .map(|name| rocksdb::ColumnFamilyDescriptor::new(*name, rocksdb::Options::default()));
        let ro = rocksdb::DB::open_cf_descriptors_read_only(&opts, dir.path(), cfs, false).unwrap();
        let db = Database::for_test(ro, 1_000_000);
        assert_eq!(db.commit_sequence(), Some(1));
        assert_eq!(db.begin().snapshot_commit_sequence(), Some(1));

        let mut t = db.begin();
        t.put(Cf::Node, b"a".to_vec(), b"1".to_vec());
        match t.commit() {
            Err(TxnError::Store(_)) => {}
            other => {
                panic!("expected TxnError::Store for a write against a read-only db, got {other:?}")
            }
        }
        assert_eq!(db.commit_sequence(), Some(1));
        assert_eq!(db.begin().get(Cf::Node, b"a").unwrap(), None);
        drop(db);
        let reopened = Database::open(dir.path(), &res).unwrap();
        assert_eq!(reopened.commit_sequence(), Some(1));
    }

    // Fix round 1 (review of the tests above found the dependency check
    // unproven): `first_committer_wins_on_a_dependency` has both
    // transactions `put` the same key, so the write-write overlap alone
    // explains the conflict — deleting `get_for_update`'s dependency
    // tracking entirely leaves it (and every other test above) green. These
    // three isolate `get_for_update` from any write-write overlap.

    #[test]
    fn get_for_update_conflicts_even_when_the_writes_are_disjoint() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        assert_eq!(t1.get_for_update(Cf::Pk, b"pk:1").unwrap(), None);
        // t1 writes a different key than the one it depends on.
        t1.put(Cf::Node, b"other".to_vec(), b"1".to_vec());
        t2.put(Cf::Pk, b"pk:1".to_vec(), b"id".to_vec());
        t2.commit().unwrap();
        assert!(matches!(t1.commit(), Err(TxnError::Conflict)));
    }

    #[test]
    fn a_read_only_get_for_update_dependency_also_conflicts() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        assert_eq!(t1.get_for_update(Cf::Pk, b"pk:1").unwrap(), None);
        // t1 writes nothing at all; the dependency alone must still conflict.
        t2.put(Cf::Pk, b"pk:1".to_vec(), b"id".to_vec());
        t2.commit().unwrap();
        assert!(matches!(t1.commit(), Err(TxnError::Conflict)));
    }

    #[test]
    fn plain_get_creates_no_dependency_and_the_commit_succeeds() {
        let (_d, _r, db) = open();
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        assert_eq!(t1.get(Cf::Pk, b"pk:1").unwrap(), None);
        t2.put(Cf::Pk, b"pk:1".to_vec(), b"id".to_vec());
        t2.commit().unwrap();
        t1.put(Cf::Node, b"other".to_vec(), b"1".to_vec());
        t1.commit().unwrap();
    }

    // Fix round 1 (review found the committer's history unbounded: it grows
    // for as long as any transaction stays open, and rescanning it under the
    // global lock cost 8.3 ms at 5M entries). `StoreConfig::conflict_history_keys`
    // caps it; this proves the cap is actually enforced and that eviction
    // beyond the safe horizon is reported as a conflict instead of silently
    // skipped.
    #[test]
    fn a_transaction_older_than_the_pruned_history_conflicts_on_any_touch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = StoreConfig {
            conflict_history_keys: 2,
            ..StoreConfig::for_tests()
        };
        let res = Resources::new(cfg);
        let db = Database::open(dir.path(), &res).unwrap();

        // Stays open across every filler commit below, so it predates all of them.
        let mut older = db.begin();

        // Three single-key commits exceed the cap of 2, forcing an eviction
        // that `older` (still active, started before all of them) could have
        // depended on.
        let mut filler1 = db.begin();
        filler1.put(Cf::Node, b"f1".to_vec(), b"1".to_vec());
        filler1.commit().unwrap();
        let mut filler2 = db.begin();
        filler2.put(Cf::Node, b"f2".to_vec(), b"2".to_vec());
        filler2.commit().unwrap();
        let mut filler3 = db.begin();
        filler3.put(Cf::Node, b"f3".to_vec(), b"3".to_vec());
        filler3.commit().unwrap();

        older.put(Cf::Node, b"own-key".to_vec(), b"x".to_vec());
        assert!(matches!(older.commit(), Err(TxnError::Conflict)));

        // A transaction that began after the pruning is unaffected and commits normally.
        let mut fresh = db.begin();
        fresh.put(Cf::Node, b"fresh".to_vec(), b"y".to_vec());
        fresh.commit().unwrap();
    }

    // Fix round 1, finding 3 (not one of the ruling's verbatim test asks, but
    // the carried-over rule requires every guarantee this fix adds to be
    // exercised): `begin`/`commit` used to `.expect()` the committer lock, so
    // a panic on any thread holding it would poison it and then panic every
    // later transaction too. Poisons the raw `Mutex` directly (bypassing the
    // `Committer::lock` helper under test) from a scoped thread, then proves
    // neither `begin` nor `commit` panics afterward, and that recovery is
    // conservative: `old` began before the poisoning and touched nothing that
    // actually overlaps with what was committed in the meantime, yet still
    // has to retry, because the recovery cannot tell "poisoned cleanly" apart
    // from "poisoned mid-update".
    #[test]
    fn a_poisoned_committer_recovers_and_forces_older_transactions_to_retry() {
        let (_d, _r, db) = open();

        // Begins before anything commits, so it predates the commit below.
        let mut old = db.begin();

        let mut seed = db.begin();
        seed.put(Cf::Node, b"seed".to_vec(), b"1".to_vec());
        seed.commit().unwrap();

        // Poison the committer's lock from another thread. `old` is still
        // active, so this commit's history entry survives to be poisoned
        // instead of being pruned away.
        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = db.committer.state.lock().unwrap();
                    panic!("simulated panic while holding the committer lock");
                })
                .join()
                .is_err()
        });
        assert!(panicked, "expected the spawned thread to panic");

        // Neither begin() nor commit() panics on the now-poisoned lock, and
        // `old` — old enough to have missed the poisoned update — retries
        // even though its own write ("anything") never touched "seed".
        old.put(Cf::Node, b"anything".to_vec(), b"x".to_vec());
        assert!(matches!(old.commit(), Err(TxnError::Conflict)));

        // A transaction that began after the poisoning is unaffected.
        let mut fresh = db.begin();
        fresh.put(Cf::Node, b"fresh-after-poison".to_vec(), b"y".to_vec());
        fresh.commit().unwrap();
    }

    // Follow-up (re-review of fix round 1, finding 1): `PoisonError::into_inner`
    // recovers the guard but leaves the `Mutex` marked poisoned, so without
    // `clear_poison()` the recovery above — including its `pruned_through`
    // bump against whatever `seq` is by then — reran on every later `begin`,
    // `commit` and `drop`. After one real poisoning, any transaction that
    // touched anything would get `Conflict` if another commit landed while it
    // was open, even with fully disjoint keys.
    #[test]
    fn poison_recovers_only_once_so_later_disjoint_transactions_still_both_commit() {
        let (_d, _r, db) = open();

        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = db.committer.state.lock().unwrap();
                    panic!("simulated panic while holding the committer lock");
                })
                .join()
                .is_err()
        });
        assert!(panicked, "expected the spawned thread to panic");

        // t1's begin() is the call that recovers from the poisoning above.
        let mut t1 = db.begin();
        let mut t2 = db.begin();
        t1.put(Cf::Node, b"disjoint-a".to_vec(), b"1".to_vec());
        t2.put(Cf::Node, b"disjoint-b".to_vec(), b"2".to_vec());
        // t1's commit is a second lock() call, and it writes (advancing the
        // shared sequence) while t2 is still open. If the poison flag were
        // still set, this commit would re-run the recovery and re-raise
        // pruned_through past t2's start, conflicting t2 below even though
        // t2 never touched "disjoint-a".
        t1.commit().unwrap();
        t2.commit().unwrap();
    }

    // Task 9 fix round 1 (reviewer Critical: a concurrent merge_edge that
    // commits before a detach_delete which never saw the new edge leaves a
    // dangling edge, because `scan_prefix` records no dependency on the
    // range it read). `scan_prefix_for_update` is the new public function
    // that closes this; it needs its own direct tests, independent of
    // graph.rs's higher-level detach_delete/merge_edge behavior.

    #[test]
    fn scan_prefix_for_update_conflicts_on_a_write_under_the_prefix_that_lands_after_start() {
        let (_d, _r, db) = open();
        let mut reader = db.begin();
        let mut writer = db.begin();
        // The prefix is empty of writes at this point; the scan alone must
        // still record the dependency (a phantom-read guard).
        assert!(reader
            .scan_prefix_for_update(Cf::Out, b"p:")
            .unwrap()
            .is_empty());
        writer.put(Cf::Out, b"p:1".to_vec(), b"x".to_vec());
        writer.commit().unwrap();
        // reader itself writes nothing; the range dependency alone must conflict.
        assert!(matches!(reader.commit(), Err(TxnError::Conflict)));
    }

    #[test]
    fn scan_prefix_for_update_does_not_conflict_on_a_write_outside_the_prefix_or_cf() {
        let (_d, _r, db) = open();
        let mut reader = db.begin();
        let mut writer = db.begin();
        assert!(reader
            .scan_prefix_for_update(Cf::Out, b"p:")
            .unwrap()
            .is_empty());
        // Same cf, a key that does not start with "p:".
        writer.put(Cf::Out, b"q:1".to_vec(), b"x".to_vec());
        // Same key bytes, a different cf.
        writer.put(Cf::In, b"p:1".to_vec(), b"y".to_vec());
        writer.commit().unwrap();
        reader.put(Cf::Node, b"unrelated".to_vec(), b"1".to_vec());
        reader.commit().unwrap();
    }
}
