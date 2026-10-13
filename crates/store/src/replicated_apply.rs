//! Experimental deterministic per-store apply, deliberately disconnected from serving.
//!
//! Trusted concrete CF mutations include all graph/index/identity/ACL/cursor bytes;
//! this module does not capture normal commits, decode graph semantics, run Raft,
//! acknowledge a quorum, synchronize ID allocator caches or atomically update a
//! server's graph registry. Apply-only stores must not perform independent writes.
//! Bounds constrain canonical entry bytes, not aggregate process memory.

use sha2::{Digest, Sha256};

use crate::db::{Database, StoreError, COMMIT_SEQUENCE_KEY};
use crate::txn::{Cf, TxnError};

const MAGIC: &[u8] = b"BWAP";
const LEDGER_PREFIX: &[u8] = b"replicated-apply/";
const LEDGER_KEY: &[u8] = b"replicated-apply/v1";
const VERSION: u8 = 1;
const FAMILIES: [Cf; 11] = [
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    pub cf: String,
    pub key: Vec<u8>,
    /// None deletes; Some(empty) stores an empty value.
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub version: u8,
    pub index: u64,
    pub mutations: Vec<Mutation>,
}

#[derive(Debug, Clone, Copy)]
pub struct ApplyLimits {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
}

impl Default for ApplyLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_bytes: 64 << 20,
            max_key_bytes: 64 << 10,
            max_value_bytes: 16 << 20,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("unsupported deterministic batch version {0}")]
    UnsupportedVersion(u8),
    #[error("invalid deterministic batch: {0}")]
    Invalid(&'static str),
    #[error("deterministic batch exceeds {0}")]
    Bounds(&'static str),
    #[error("unknown column family {0}")]
    UnknownColumnFamily(String),
    #[error("duplicate column-family/key mutation")]
    DuplicateMutation,
    #[error("mutation targets reserved apply/commit metadata")]
    ReservedMetadata,
    #[error("invalid durable applied ledger")]
    InvalidLedger,
    #[error("log index gap: expected {expected}, received {received}")]
    Gap { expected: u64, received: u64 },
    #[error("older entry rejected: applied {applied}, received {received}")]
    Older { applied: u64, received: u64 },
    #[error("divergent duplicate at index {0}")]
    Divergent(u64),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transaction(#[from] TxnError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedState {
    pub index: u64,
    pub digest: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReceipt {
    pub state: AppliedState,
    pub already_applied: bool,
    /// Local durable store sequence; independent of the entry index.
    pub commit_sequence: Option<u64>,
}

fn family(name: &str) -> Result<Cf, ApplyError> {
    if name.len() > 5 {
        return Err(ApplyError::Invalid("column family name exceeds 5 bytes"));
    }
    FAMILIES
        .into_iter()
        .find(|cf| cf.name() == name)
        .ok_or_else(|| ApplyError::UnknownColumnFamily(name.into()))
}

type PreparedMutations<'a> = Vec<(&'a Mutation, Cf)>;

impl Batch {
    fn prepare(&self, limits: ApplyLimits) -> Result<(PreparedMutations<'_>, usize), ApplyError> {
        if self.version != VERSION {
            return Err(ApplyError::UnsupportedVersion(self.version));
        }
        if self.index == 0 || self.index == u64::MAX {
            return Err(ApplyError::Invalid("index must be 1..u64::MAX-1"));
        }
        if [
            limits.max_entries,
            limits.max_bytes,
            limits.max_key_bytes,
            limits.max_value_bytes,
        ]
        .contains(&0)
        {
            return Err(ApplyError::Invalid("limits must be positive"));
        }
        if self.mutations.len() > limits.max_entries || self.mutations.len() > u32::MAX as usize {
            return Err(ApplyError::Bounds("max_entries"));
        }
        let mut size = 17usize;
        let mut entries = Vec::with_capacity(self.mutations.len());
        for mutation in &self.mutations {
            let cf = family(&mutation.cf)?;
            if mutation.key.is_empty() {
                return Err(ApplyError::Invalid("empty key"));
            }
            if mutation.key.len() > limits.max_key_bytes || mutation.key.len() > u32::MAX as usize {
                return Err(ApplyError::Bounds("max_key_bytes"));
            }
            if mutation
                .value
                .as_ref()
                .is_some_and(|v| v.len() > limits.max_value_bytes || v.len() > u32::MAX as usize)
            {
                return Err(ApplyError::Bounds("max_value_bytes"));
            }
            if cf == Cf::Meta
                && (mutation.key.starts_with(LEDGER_PREFIX) || mutation.key == COMMIT_SEQUENCE_KEY)
            {
                return Err(ApplyError::ReservedMetadata);
            }
            let overhead =
                1 + mutation.cf.len() + 4 + 1 + mutation.value.as_ref().map_or(0, |v| 4 + v.len());
            size = size
                .checked_add(overhead)
                .and_then(|n| n.checked_add(mutation.key.len()))
                .ok_or(ApplyError::Bounds("max_bytes"))?;
            if size > limits.max_bytes {
                return Err(ApplyError::Bounds("max_bytes"));
            }
            entries.push((mutation, cf));
        }
        if size > limits.max_bytes {
            return Err(ApplyError::Bounds("max_bytes"));
        }
        entries.sort_unstable_by(|a, b| (&a.0.cf, &a.0.key).cmp(&(&b.0.cf, &b.0.key)));
        if entries
            .windows(2)
            .any(|pair| pair[0].0.cf == pair[1].0.cf && pair[0].0.key == pair[1].0.key)
        {
            return Err(ApplyError::DuplicateMutation);
        }
        Ok((entries, size))
    }

    fn encode(&self, entries: &[(&Mutation, Cf)], mut write: impl FnMut(&[u8])) {
        write(MAGIC);
        write(&[self.version]);
        write(&self.index.to_be_bytes());
        write(&(entries.len() as u32).to_be_bytes());
        for (mutation, _) in entries {
            write(&[mutation.cf.len() as u8]);
            write(mutation.cf.as_bytes());
            write(&(mutation.key.len() as u32).to_be_bytes());
            write(&mutation.key);
            match &mutation.value {
                None => write(&[0]),
                Some(value) => {
                    write(&[1]);
                    write(&(value.len() as u32).to_be_bytes());
                    write(value);
                }
            }
        }
    }

    pub fn canonical_bytes(&self, limits: ApplyLimits) -> Result<Vec<u8>, ApplyError> {
        let (entries, size) = self.prepare(limits)?;
        let mut result = Vec::with_capacity(size);
        self.encode(&entries, |bytes| result.extend_from_slice(bytes));
        Ok(result)
    }

    pub fn digest(&self, limits: ApplyLimits) -> Result<[u8; 32], ApplyError> {
        let (entries, _) = self.prepare(limits)?;
        let mut hash = Sha256::new();
        self.encode(&entries, |bytes| hash.update(bytes));
        Ok(hash.finalize().into())
    }

    /// Decode only the canonical ordering; announcements are checked before copying.
    pub fn from_canonical_bytes(bytes: &[u8], limits: ApplyLimits) -> Result<Self, ApplyError> {
        if [
            limits.max_entries,
            limits.max_bytes,
            limits.max_key_bytes,
            limits.max_value_bytes,
        ]
        .contains(&0)
        {
            return Err(ApplyError::Invalid("limits must be positive"));
        }
        if bytes.len() > limits.max_bytes {
            return Err(ApplyError::Bounds("max_bytes"));
        }
        let mut input = bytes;
        fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], ApplyError> {
            if input.len() < count {
                return Err(ApplyError::Invalid("truncated encoding"));
            }
            let (head, tail) = input.split_at(count);
            *input = tail;
            Ok(head)
        }
        fn length(input: &mut &[u8]) -> Result<usize, ApplyError> {
            Ok(u32::from_be_bytes(take(input, 4)?.try_into().unwrap()) as usize)
        }
        if take(&mut input, 4)? != MAGIC {
            return Err(ApplyError::Invalid("encoding magic"));
        }
        let version = take(&mut input, 1)?[0];
        if version != VERSION {
            return Err(ApplyError::UnsupportedVersion(version));
        }
        let index = u64::from_be_bytes(take(&mut input, 8)?.try_into().unwrap());
        if index == 0 || index == u64::MAX {
            return Err(ApplyError::Invalid("index must be 1..u64::MAX-1"));
        }
        let count = length(&mut input)?;
        if count > limits.max_entries {
            return Err(ApplyError::Bounds("max_entries"));
        }
        if count > input.len() / 9 {
            return Err(ApplyError::Invalid("truncated entry announcements"));
        }
        let mut mutations: Vec<Mutation> = Vec::with_capacity(count);
        for _ in 0..count {
            let cf_len = take(&mut input, 1)?[0] as usize;
            let cf = std::str::from_utf8(take(&mut input, cf_len)?)
                .map_err(|_| ApplyError::Invalid("column family encoding"))?;
            family(cf)?;
            let key_len = length(&mut input)?;
            if key_len == 0 || key_len > limits.max_key_bytes {
                return Err(ApplyError::Bounds("max_key_bytes"));
            }
            let key = take(&mut input, key_len)?.to_vec();
            let value = match take(&mut input, 1)?[0] {
                0 => None,
                1 => {
                    let n = length(&mut input)?;
                    if n > limits.max_value_bytes {
                        return Err(ApplyError::Bounds("max_value_bytes"));
                    }
                    Some(take(&mut input, n)?.to_vec())
                }
                _ => return Err(ApplyError::Invalid("mutation operation")),
            };
            if mutations.last().is_some_and(|prior| {
                (prior.cf.as_str(), prior.key.as_slice()) >= (cf, key.as_slice())
            }) {
                return Err(ApplyError::Invalid("noncanonical entry order"));
            }
            mutations.push(Mutation {
                cf: cf.into(),
                key,
                value,
            });
        }
        if !input.is_empty() {
            return Err(ApplyError::Invalid("trailing encoding bytes"));
        }
        let batch = Self {
            version,
            index,
            mutations,
        };
        batch.prepare(limits)?;
        Ok(batch)
    }
}

fn ledger(bytes: &[u8]) -> Result<AppliedState, ApplyError> {
    if bytes.len() != 45 || &bytes[..4] != b"BWAL" || bytes[4] != VERSION {
        return Err(ApplyError::InvalidLedger);
    }
    let index = u64::from_be_bytes(bytes[5..13].try_into().unwrap());
    if index == 0 || index == u64::MAX {
        return Err(ApplyError::InvalidLedger);
    }
    Ok(AppliedState {
        index,
        digest: bytes[13..].try_into().unwrap(),
    })
}

pub fn applied_state(database: &Database) -> Result<Option<AppliedState>, ApplyError> {
    database
        .begin()
        .get(Cf::Meta, LEDGER_KEY)?
        .as_deref()
        .map(ledger)
        .transpose()
}

/// Applies trusted materialized bytes and progress in one existing synchronous commit.
/// A conflict is returned to the caller; no hidden retry or quorum acknowledgement.
pub fn apply(
    database: &Database,
    batch: &Batch,
    limits: ApplyLimits,
) -> Result<ApplyReceipt, ApplyError> {
    let (entries, _) = batch.prepare(limits)?;
    let mut hash = Sha256::new();
    batch.encode(&entries, |bytes| hash.update(bytes));
    let state = AppliedState {
        index: batch.index,
        digest: hash.finalize().into(),
    };
    let mut transaction = database.begin();
    let current = transaction
        .get_for_update(Cf::Meta, LEDGER_KEY)?
        .as_deref()
        .map(ledger)
        .transpose()?;
    if let Some(current) = current {
        if batch.index < current.index {
            return Err(ApplyError::Older {
                applied: current.index,
                received: batch.index,
            });
        }
        if batch.index == current.index {
            if state.digest != current.digest {
                return Err(ApplyError::Divergent(batch.index));
            }
            return Ok(ApplyReceipt {
                state,
                already_applied: true,
                commit_sequence: transaction.snapshot_commit_sequence(),
            });
        }
    }
    let expected = current.map_or(1, |state| state.index + 1);
    if batch.index != expected {
        return Err(ApplyError::Gap {
            expected,
            received: batch.index,
        });
    }
    for (mutation, cf) in entries {
        match &mutation.value {
            Some(value) => transaction.put(cf, mutation.key.clone(), value.clone()),
            None => transaction.delete(cf, mutation.key.clone()),
        }
    }
    let mut marker = Vec::with_capacity(45);
    marker.extend_from_slice(b"BWAL");
    marker.push(VERSION);
    marker.extend_from_slice(&state.index.to_be_bytes());
    marker.extend_from_slice(&state.digest);
    transaction.put(Cf::Meta, LEDGER_KEY.to_vec(), marker);
    let sequence = transaction.commit()?;
    Ok(ApplyReceipt {
        state,
        already_applied: false,
        commit_sequence: Some(sequence),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Resources, StoreConfig, COLUMN_FAMILIES};
    #[test]
    fn actual_failed_commit_does_not_persist_data_or_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let resources = Resources::new(StoreConfig::for_tests());
        drop(Database::open(directory.path(), &resources).unwrap());
        let families = COLUMN_FAMILIES
            .iter()
            .map(|name| rocksdb::ColumnFamilyDescriptor::new(*name, rocksdb::Options::default()));
        let raw = rocksdb::DB::open_cf_descriptors_read_only(
            &rocksdb::Options::default(),
            directory.path(),
            families,
            false,
        )
        .unwrap();
        let database = Database::for_test(raw, 1_000_000);
        let batch = Batch {
            version: 1,
            index: 1,
            mutations: vec![Mutation {
                cf: "note".into(),
                key: b"pending".to_vec(),
                value: Some(b"v".to_vec()),
            }],
        };
        assert!(matches!(
            apply(&database, &batch, ApplyLimits::default()),
            Err(ApplyError::Transaction(TxnError::Store(StoreError::Rocks(
                _
            ))))
        ));
        assert_eq!(applied_state(&database).unwrap(), None);
        assert_eq!(database.begin().get(Cf::Note, b"pending").unwrap(), None);
        assert_eq!(database.commit_sequence(), None);
    }
}
