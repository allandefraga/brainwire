//! Experimental request ledger used by the authenticated opt-in server mode.
//!
//! The extra column family makes legacy opens fail. Creation is restricted to a
//! new directory; activation of existing stores and restore/fork are unsupported.
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{Database, Resources, StoreError};
use crate::txn::{Cf, TxnError};

pub(crate) const FAMILY: &str = "request_ledger_v1";
const HEADER: &[u8] = b"header";
const ENTRY_PREFIX: &[u8] = b"r/";
const HEADER_BYTES: usize = 41;
const RECORD_BYTES: usize = 57;
const ENTRY_BYTES: u64 = (2 + 32 + RECORD_BYTES) as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerLimits {
    pub max_entries: u64,
    /// Canonical request-entry key/value bytes, excluding the fixed header,
    /// physical SST overhead and process memory.
    pub max_bytes: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct LedgerState {
    pub(crate) incarnation: [u8; 16],
    limits: LedgerLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReceipt {
    pub incarnation: [u8; 16],
    pub sequence: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error(transparent)]
    Transaction(#[from] TxnError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("identified storage is not enabled")]
    Disabled,
    #[error("invalid request ledger input: {0}")]
    Invalid(&'static str),
    #[error("request already committed at sequence {0}")]
    AlreadyCommitted(u64),
    #[error("request was reused with different mutations")]
    PayloadMismatch,
    #[error("request ledger is full")]
    Full,
}

pub(crate) struct PreparedRequest {
    pub(crate) key: Vec<u8>,
    pub(crate) fingerprint: [u8; 32],
}

fn corrupt(message: &str) -> StoreError {
    StoreError::Corrupt(format!("request ledger: {message}"))
}

fn encode_header(state: LedgerState, count: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_BYTES);
    bytes.push(1);
    bytes.extend_from_slice(&state.incarnation);
    bytes.extend_from_slice(&state.limits.max_entries.to_be_bytes());
    bytes.extend_from_slice(&state.limits.max_bytes.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes
}

fn header(bytes: &[u8]) -> Result<(LedgerState, u64), StoreError> {
    if bytes.len() != HEADER_BYTES || bytes[0] != 1 {
        return Err(corrupt("invalid header"));
    }
    let number = |start| u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap());
    let state = LedgerState {
        incarnation: bytes[1..17].try_into().unwrap(),
        limits: LedgerLimits {
            max_entries: number(17),
            max_bytes: number(25),
        },
    };
    let count = number(33);
    if state.incarnation == [0; 16]
        || state.limits.max_entries == 0
        || state.limits.max_bytes < ENTRY_BYTES
        || count > state.limits.max_entries
        || count
            .checked_mul(ENTRY_BYTES)
            .is_none_or(|n| n > state.limits.max_bytes)
    {
        return Err(corrupt("invalid identity or quota"));
    }
    Ok((state, count))
}

fn record(bytes: &[u8], state: LedgerState) -> Result<([u8; 32], u64), StoreError> {
    if bytes.len() != RECORD_BYTES || bytes[0] != 1 || bytes[1..17] != state.incarnation {
        return Err(corrupt("invalid record"));
    }
    let sequence = u64::from_be_bytes(bytes[49..57].try_into().unwrap());
    if sequence == 0 {
        return Err(corrupt("zero committed sequence"));
    }
    Ok((bytes[17..49].try_into().unwrap(), sequence))
}

impl Database {
    /// Opt-in storage format. The path must not exist; legacy activation is refused.
    /// The experimental server mode uses this when creating new graphs.
    #[doc(hidden)]
    pub fn create_with_request_ledger(
        path: &Path,
        resources: &Resources,
        limits: LedgerLimits,
    ) -> Result<Self, LedgerError> {
        if limits.max_entries == 0 || limits.max_bytes < ENTRY_BYTES {
            return Err(LedgerError::Invalid(
                "positive entry quota and at least one record required",
            ));
        }
        std::fs::create_dir(path)?;
        let mut db = Self::open_config(path, resources, true, true)?;
        let state = LedgerState {
            incarnation: *uuid::Uuid::new_v4().as_bytes(),
            limits,
        };
        let mut batch = rocksdb::WriteBatch::default();
        batch.put_cf(db.cf(FAMILY)?, HEADER, encode_header(state, 0));
        let mut options = rocksdb::WriteOptions::default();
        options.set_sync(true);
        db.db.write_opt(batch, &options).map_err(StoreError::from)?;
        db.request_ledger = Some(state);
        Ok(db)
    }

    /// Opens only an already initialized opt-in store. Missing or corrupt
    /// metadata fails closed; the identity and quota are never regenerated.
    #[doc(hidden)]
    pub fn open_with_request_ledger(
        path: &Path,
        resources: &Resources,
    ) -> Result<Self, LedgerError> {
        let mut db = Self::open_config(path, resources, true, false)?;
        let cf = db.cf(FAMILY)?;
        let bytes = db
            .db
            .get_cf(cf, HEADER)
            .map_err(StoreError::from)?
            .ok_or_else(|| corrupt("missing header"))?;
        let (state, count) = header(&bytes)?;
        let mut actual = 0u64;
        let latest = db.commit_sequence().unwrap_or(0);
        for item in db.db.iterator_cf(cf, rocksdb::IteratorMode::Start) {
            let (key, value) = item.map_err(StoreError::from)?;
            if key.as_ref() == HEADER {
                continue;
            }
            if key.len() != 34 || !key.starts_with(ENTRY_PREFIX) {
                return Err(corrupt("invalid key").into());
            }
            let (_, sequence) = record(&value, state)?;
            if sequence > latest {
                return Err(corrupt("record exceeds committed sequence").into());
            }
            actual = actual
                .checked_add(1)
                .ok_or_else(|| corrupt("entry count overflow"))?;
            if actual > count {
                return Err(corrupt("entry count mismatch").into());
            }
        }
        if actual != count {
            return Err(corrupt("entry count mismatch").into());
        }
        db.request_ledger = Some(state);
        Ok(db)
    }

    /// Stable identity of this experimental store, not proof of restore/fork lineage.
    #[doc(hidden)]
    pub fn request_ledger_incarnation(&self) -> Option<[u8; 16]> {
        self.request_ledger.map(|state| state.incarnation)
    }

    /// Trusted storage lookup only: callers must establish authentication and
    /// graph ownership before providing the scope. Absence does not prove abort.
    #[doc(hidden)]
    pub fn request_receipt(
        &self,
        scope: &str,
        seed: &str,
    ) -> Result<Option<CommitReceipt>, LedgerError> {
        let request = self.prepare_request(scope, seed, [0; 32])?;
        let state = self.request_ledger.ok_or(LedgerError::Disabled)?;
        self.committer.known_request_read(|| {
            self.db
                .get_cf(self.cf(FAMILY)?, request.key)
                .map_err(StoreError::from)?
                .map(|bytes| {
                    record(&bytes, state).map(|(_, sequence)| CommitReceipt {
                        incarnation: state.incarnation,
                        sequence,
                    })
                })
                .transpose()
                .map_err(Into::into)
        })
    }

    pub(crate) fn prepare_request(
        &self,
        scope: &str,
        seed: &str,
        fingerprint: [u8; 32],
    ) -> Result<PreparedRequest, LedgerError> {
        let state = self.request_ledger.ok_or(LedgerError::Disabled)?;
        for component in [scope, seed] {
            if component.is_empty()
                || component.len() > 128
                || !component.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(LedgerError::Invalid(
                    "scope and seed must be 1..128 printable ASCII bytes",
                ));
            }
        }
        let mut digest = Sha256::new();
        digest.update(b"brainwire/request-ledger/v1\0");
        digest.update(state.incarnation);
        for component in [scope, seed] {
            digest.update((component.len() as u32).to_be_bytes());
            digest.update(component.as_bytes());
        }
        let mut key = ENTRY_PREFIX.to_vec();
        key.extend_from_slice(&digest.finalize());
        Ok(PreparedRequest { key, fingerprint })
    }

    /// Must be called with the transaction committer lock held.
    pub(crate) fn append_request(
        &self,
        request: &PreparedRequest,
        sequence: u64,
        batch: &mut rocksdb::WriteBatch,
    ) -> Result<CommitReceipt, LedgerError> {
        let state = self.request_ledger.ok_or(LedgerError::Disabled)?;
        let cf = self.cf(FAMILY)?;
        if let Some(bytes) = self.db.get_cf(cf, &request.key).map_err(StoreError::from)? {
            let (fingerprint, sequence) = record(&bytes, state)?;
            return Err(if fingerprint == request.fingerprint {
                LedgerError::AlreadyCommitted(sequence)
            } else {
                LedgerError::PayloadMismatch
            });
        }
        let raw = self
            .db
            .get_cf(cf, HEADER)
            .map_err(StoreError::from)?
            .ok_or_else(|| corrupt("missing header"))?;
        let (stored, count) = header(&raw)?;
        if stored.incarnation != state.incarnation || stored.limits != state.limits {
            return Err(corrupt("header changed").into());
        }
        let next = count.checked_add(1).ok_or(LedgerError::Full)?;
        if next > state.limits.max_entries
            || next
                .checked_mul(ENTRY_BYTES)
                .is_none_or(|n| n > state.limits.max_bytes)
        {
            return Err(LedgerError::Full);
        }
        let mut value = Vec::with_capacity(RECORD_BYTES);
        value.push(1);
        value.extend_from_slice(&state.incarnation);
        value.extend_from_slice(&request.fingerprint);
        value.extend_from_slice(&sequence.to_be_bytes());
        batch.put_cf(cf, &request.key, value);
        batch.put_cf(cf, HEADER, encode_header(state, next));
        Ok(CommitReceipt {
            incarnation: state.incarnation,
            sequence,
        })
    }
}

/// The exact staged mutation fingerprint, not a query-text or user payload hash.
pub(crate) fn fingerprint<'a>(
    writes: impl Iterator<Item = (Cf, &'a [u8], Option<&'a [u8]>)>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"brainwire/staged-mutations/v1\0");
    for (family, key, value) in writes {
        hash.update((family.name().len() as u64).to_be_bytes());
        hash.update(family.name().as_bytes());
        hash.update((key.len() as u64).to_be_bytes());
        hash.update(key);
        match value {
            Some(value) => {
                hash.update([1]);
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value);
            }
            None => hash.update([0]),
        }
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{StoreConfig, COLUMN_FAMILIES};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn resources() -> Resources {
        Resources::new(StoreConfig::for_tests())
    }
    fn limits(entries: u64) -> LedgerLimits {
        LedgerLimits {
            max_entries: entries,
            max_bytes: entries * ENTRY_BYTES,
        }
    }

    #[test]
    fn duplicate_and_mismatch_do_not_apply_or_advance_and_reopen_keeps_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("graph");
        let res = resources();
        let db = Database::create_with_request_ledger(&path, &res, limits(4)).unwrap();
        let mut tx = db.begin();
        tx.put(Cf::Node, b"key".to_vec(), b"one".to_vec());
        let receipt = tx.commit_identified("principal-1", "event-1").unwrap();
        assert_eq!(receipt.sequence, 1);
        let mut duplicate = db.begin();
        duplicate.put(Cf::Node, b"key".to_vec(), b"one".to_vec());
        assert!(matches!(
            duplicate.commit_identified("principal-1", "event-1"),
            Err(LedgerError::AlreadyCommitted(1))
        ));
        let mut mismatch = db.begin();
        mismatch.put(Cf::Node, b"key".to_vec(), b"two".to_vec());
        assert!(matches!(
            mismatch.commit_identified("principal-1", "event-1"),
            Err(LedgerError::PayloadMismatch)
        ));
        assert_eq!(db.commit_sequence(), Some(1));
        assert_eq!(
            db.begin().get(Cf::Node, b"key").unwrap(),
            Some(b"one".to_vec())
        );
        assert_eq!(db.request_receipt("principal-2", "event-1").unwrap(), None);
        drop(db);
        let db = Database::open_with_request_ledger(&path, &res).unwrap();
        assert_eq!(
            db.request_receipt("principal-1", "event-1").unwrap(),
            Some(receipt)
        );
    }

    #[test]
    fn concurrent_duplicate_serializes_lookup_with_the_graph_batch() {
        let root = tempfile::tempdir().unwrap();
        let res = resources();
        let db = Database::create_with_request_ledger(&root.path().join("graph"), &res, limits(4))
            .unwrap();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let worker = || {
                let mut tx = db.begin();
                tx.put(Cf::Node, b"key".to_vec(), b"one".to_vec());
                barrier.wait();
                tx.commit_identified("principal", "same")
            };
            let one = scope.spawn(worker);
            let two = scope.spawn(worker);
            [one.join().unwrap(), two.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|x| x.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|x| matches!(x, Err(LedgerError::AlreadyCommitted(1))))
                .count(),
            1
        );
        assert_eq!(db.commit_sequence(), Some(1));
    }

    #[test]
    fn quota_and_conflict_write_neither_ledger_nor_graph_and_empty_request_is_recorded() {
        let root = tempfile::tempdir().unwrap();
        let res = resources();
        let db = Database::create_with_request_ledger(&root.path().join("graph"), &res, limits(1))
            .unwrap();
        db.begin().commit_identified("principal", "empty").unwrap();
        let mut full = db.begin();
        full.put(Cf::Node, b"lost".to_vec(), b"no".to_vec());
        assert!(matches!(
            full.commit_identified("principal", "new"),
            Err(LedgerError::Full)
        ));
        assert_eq!(db.request_receipt("principal", "new").unwrap(), None);
        assert_eq!(db.begin().get(Cf::Node, b"lost").unwrap(), None);
        assert_eq!(db.commit_sequence(), Some(1));

        let db = Database::create_with_request_ledger(&root.path().join("other"), &res, limits(4))
            .unwrap();
        let mut old = db.begin();
        old.get_for_update(Cf::Node, b"key").unwrap();
        old.put(Cf::Node, b"old".to_vec(), b"no".to_vec());
        let mut fresh = db.begin();
        fresh.put(Cf::Node, b"key".to_vec(), b"new".to_vec());
        fresh.commit().unwrap();
        assert!(matches!(
            old.commit_identified("principal", "conflict"),
            Err(LedgerError::Transaction(TxnError::Conflict))
        ));
        assert_eq!(db.request_receipt("principal", "conflict").unwrap(), None);
        assert_eq!(db.begin().get(Cf::Node, b"old").unwrap(), None);
    }

    #[test]
    fn invalid_input_and_byte_quota_fail_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let res = resources();
        let path = root.path().join("invalid");
        assert!(Database::create_with_request_ledger(
            &path,
            &res,
            LedgerLimits {
                max_entries: 1,
                max_bytes: 1
            }
        )
        .is_err());
        assert!(!path.exists());
        let db = Database::create_with_request_ledger(
            &root.path().join("graph"),
            &res,
            LedgerLimits {
                max_entries: 100,
                max_bytes: ENTRY_BYTES,
            },
        )
        .unwrap();
        for bad in ["", "line\nbreak", &"x".repeat(129)] {
            assert!(matches!(
                db.begin().commit_identified("principal", bad),
                Err(LedgerError::Invalid(_))
            ));
        }
        assert_eq!(db.commit_sequence(), None);
        db.begin().commit_identified("principal", "first").unwrap();
        assert!(matches!(
            db.begin().commit_identified("principal", "second"),
            Err(LedgerError::Full)
        ));
    }

    #[test]
    fn legacy_open_refuses_extra_family_and_activation_does_not_touch_existing_store() {
        let root = tempfile::tempdir().unwrap();
        let res = resources();
        let path = root.path().join("graph");
        let db = Database::create_with_request_ledger(&path, &res, limits(1)).unwrap();
        db.begin().commit_identified("principal", "first").unwrap();
        drop(db);
        assert!(Database::open(&path, &res).is_err());
        let mut options = rocksdb::Options::default();
        options.create_if_missing(false);
        // This is the old binary's exact CF open policy; a real old-binary
        // qualification remains open for the experimental opt-in server format.
        assert!(rocksdb::DB::open_cf(&options, &path, COLUMN_FAMILIES).is_err());
        assert!(Database::create_with_request_ledger(&path, &res, limits(1)).is_err());
        let legacy = root.path().join("legacy");
        drop(Database::open(&legacy, &res).unwrap());
        assert!(Database::open_with_request_ledger(&legacy, &res).is_err());
        assert!(Database::create_with_request_ledger(&legacy, &res, limits(1)).is_err());
        assert!(Database::open(&legacy, &res).is_ok());
    }

    #[test]
    fn successful_write_with_lost_result_quarantines_until_recovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("graph");
        let res = resources();
        let db = Database::create_with_request_ledger(&path, &res, limits(4)).unwrap();
        let reserved_id = db.next_node_id(brainwire_core::ids::TypeId(1)).unwrap();
        let mut pending_replication = db.begin().with_replicated_ids();
        pending_replication.put(Cf::Node, b"not-prepared".to_vec(), b"value".to_vec());
        let invalid_path = root.path().join("not-a-directory");
        std::fs::write(&invalid_path, b"file").unwrap();
        let injected = Database::open(&invalid_path, &res).err().unwrap();
        db.inject_unknown_result_after_next_write(injected);
        let mut tx = db.begin();
        tx.put(Cf::Node, b"unknown".to_vec(), b"committed".to_vec());
        assert!(tx.commit_identified("principal", "uncertain").is_err());
        assert_eq!(
            db.commit_sequence(),
            None,
            "RAM was deliberately not advanced"
        );
        assert!(db.request_receipt("principal", "uncertain").is_err());
        assert!(db.begin().commit().is_err());
        assert!(matches!(
            pending_replication
                .prepare_replication(crate::replicated_apply::ApplyLimits::default()),
            Err(crate::replicated_apply::ApplyError::Invalid(
                "uncertain storage write requires reopen"
            ))
        ));
        assert!(matches!(
            db.begin()
                .with_replicated_ids()
                .prepare_replication(crate::replicated_apply::ApplyLimits::default()),
            Err(crate::replicated_apply::ApplyError::Invalid(
                "uncertain storage write requires reopen"
            ))
        ));
        // Quarantine blocks durable actions, not low-level read access or
        // consumption of an already reserved ID block.
        assert_eq!(
            db.begin().get(Cf::Node, b"unknown").unwrap(),
            Some(b"committed".to_vec())
        );
        assert_eq!(
            db.next_node_id(brainwire_core::ids::TypeId(1))
                .unwrap()
                .seq(),
            reserved_id.seq() + 1
        );

        assert!(db
            .checkpoint(&root.path().join("blocked-checkpoint"))
            .is_err());
        drop(db);
        let db = Database::open_with_request_ledger(&path, &res).unwrap();
        assert_eq!(db.commit_sequence(), Some(2));
        assert_eq!(
            db.request_receipt("principal", "uncertain")
                .unwrap()
                .unwrap()
                .sequence,
            2
        );
        assert_eq!(
            db.begin().get(Cf::Node, b"unknown").unwrap(),
            Some(b"committed".to_vec())
        );
        assert_eq!(
            db.begin()
                .commit_identified("principal", "next")
                .unwrap()
                .sequence,
            3
        );
    }

    #[test]
    fn storage_write_refusal_keeps_ledger_graph_and_quota_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("graph");
        let res = resources();
        let db = Database::create_with_request_ledger(&path, &res, limits(1)).unwrap();
        let state = db.request_ledger.unwrap();
        drop(db);
        let options = rocksdb::Options::default();
        let raw = rocksdb::DB::open_cf_for_read_only(
            &options,
            &path,
            COLUMN_FAMILIES.into_iter().chain([FAMILY]),
            false,
        )
        .unwrap();
        let mut db = Database::for_test(raw, 100);
        db.request_ledger = Some(state);
        let mut tx = db.begin();
        tx.put(Cf::Node, b"not-written".to_vec(), b"value".to_vec());
        assert!(matches!(
            tx.commit_identified("principal", "refused"),
            Err(LedgerError::Store(StoreError::Rocks(_)))
        ));
        assert_eq!(db.commit_sequence(), None);
        assert!(db.request_receipt("principal", "refused").is_err());
        assert!(db
            .begin()
            .commit_identified("principal", "blocked-until-reopen")
            .is_err());
        assert_eq!(db.begin().get(Cf::Node, b"not-written").unwrap(), None);
        drop(db);
        let db = Database::open_with_request_ledger(&path, &res).unwrap();
        assert!(db
            .begin()
            .commit_identified("principal", "after-refusal")
            .is_ok());
    }

    #[test]
    fn missing_corrupt_or_inconsistent_header_fails_closed_without_regeneration() {
        let root = tempfile::tempdir().unwrap();
        let res = resources();
        for scenario in ["missing", "bad", "count"] {
            let path = root.path().join(scenario);
            let db = Database::create_with_request_ledger(&path, &res, limits(4)).unwrap();
            let cf = db.cf(FAMILY).unwrap();
            match scenario {
                "missing" => db.db.delete_cf(cf, HEADER).unwrap(),
                "bad" => db.db.put_cf(cf, HEADER, b"bad").unwrap(),
                _ => db
                    .db
                    .put_cf(cf, HEADER, encode_header(db.request_ledger.unwrap(), 1))
                    .unwrap(),
            }
            drop(db);
            assert!(Database::open_with_request_ledger(&path, &res).is_err());
        }
    }

    #[test]
    #[ignore = "subprocess helper for retained-volume crash test"]
    fn crash_writer() {
        let path = std::path::PathBuf::from(
            std::env::var_os("BW_LEDGER_TEST_PATH").expect("owned test path"),
        );
        let res = resources();
        let db = Database::open_with_request_ledger(&path, &res).unwrap();
        let mut tx = db.begin();
        tx.put(Cf::Node, b"crash-key".to_vec(), b"value".to_vec());
        if std::env::var("BW_LEDGER_TEST_PHASE").unwrap() == "after" {
            tx.commit_identified("principal", "crash-request").unwrap();
        }
        std::fs::write(path.parent().unwrap().join("ready"), b"ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn process_kill_before_or_after_commit_recovers_both_or_neither() {
        let res = resources();
        for phase in ["before", "after"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("graph");
            drop(Database::create_with_request_ledger(&path, &res, limits(4)).unwrap());
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "db::request_ledger::tests::crash_writer",
                    "--ignored",
                ])
                .env("BW_LEDGER_TEST_PATH", &path)
                .env("BW_LEDGER_TEST_PHASE", phase)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            while !root.path().join("ready").exists() && Instant::now() < deadline {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("crash helper exited early: {status}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let ready = root.path().join("ready").exists();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(ready, "crash helper did not reach selected commit boundary");
            let db = Database::open_with_request_ledger(&path, &res).unwrap();
            let data = db.begin().get(Cf::Node, b"crash-key").unwrap();
            let outcome = db.request_receipt("principal", "crash-request").unwrap();
            assert_eq!(data.is_some(), phase == "after");
            assert_eq!(outcome.is_some(), phase == "after");
        }
    }
}
