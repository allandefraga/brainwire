//! Sync WAL-backed consensus metadata. Snapshot transfer and log purging are unsupported.
// OpenRaft storage traits require their unboxed upstream StorageError (224 bytes).
#![allow(clippy::result_large_err)]
use crate::{Application, Command, Reply, TypeConfig};
use openraft::storage::{LogFlushed, RaftLogStorage, RaftStateMachine};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, LogState, OptionalSend, RaftLogReader,
    RaftSnapshotBuilder, Snapshot, SnapshotMeta, StorageError, StorageIOError, StoredMembership,
    Vote,
};
use rocksdb::{IteratorMode, Options, WriteBatch, WriteOptions, DB};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fmt::Debug,
    ops::{Bound, RangeBounds},
    path::Path,
    sync::{Arc, Mutex},
};

type Error = StorageError<u64>;
const VOTE: &[u8] = b"Mvote";
const COMMITTED: &[u8] = b"Mcommitted";
const APPLIED: &[u8] = b"Mapplied";
const MEMBERSHIP: &[u8] = b"Mmembership";
const BYTES: &[u8] = b"Mbytes";
const REGISTRY_INITIALIZED: &[u8] = b"Mregistry_initialized";
fn error(message: impl ToString) -> Error {
    StorageIOError::write(&std::io::Error::other(message.to_string())).into()
}
fn key(index: u64) -> [u8; 9] {
    let mut k = [b'L'; 9];
    k[1..].copy_from_slice(&index.to_be_bytes());
    k
}
fn dedupe_key(id: &str) -> Vec<u8> {
    let mut k = vec![b'D'];
    k.extend_from_slice(id.as_bytes());
    k
}
#[derive(Serialize, Deserialize)]
struct Cached {
    digest: [u8; 32],
    reply: Reply,
}
struct Inner {
    db: DB,
    io: Mutex<()>,
    sm: Mutex<Option<String>>,
    application: Arc<dyn Application>,
    max_log_bytes: u64,
}
#[derive(Clone)]
pub struct Storage(Arc<Inner>);
#[derive(Clone)]
pub struct LogStore(Storage);
#[derive(Clone)]
pub struct StateMachine(Storage);
impl Storage {
    pub fn open(
        path: impl AsRef<Path>,
        max_log_bytes: u64,
        application: Arc<dyn Application>,
    ) -> Result<Self, Error> {
        if max_log_bytes == 0 {
            return Err(error("retained log cap must be positive"));
        }
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_paranoid_checks(true);
        let db = DB::open(&opts, path).map_err(error)?;
        let result = Self(Arc::new(Inner {
            db,
            io: Mutex::new(()),
            sm: Mutex::new(None),
            application,
            max_log_bytes,
        }));
        let mut bytes = 0u64;
        let mut previous: Option<u64> = None;
        for item in result.0.db.iterator(IteratorMode::Start) {
            let (k, v) = item.map_err(error)?;
            if k.first() != Some(&b'L') {
                continue;
            }
            if k.len() != 9 {
                return Err(error("invalid log key"));
            }
            if v.len() > crate::types::MAX_ENTRY_BYTES {
                return Err(error("retained entry exceeds transport bound"));
            }
            let e: Entry<TypeConfig> = serde_json::from_slice(&v).map_err(error)?;
            if k.as_ref() != key(e.log_id.index)
                || previous.is_some_and(|p| p.checked_add(1) != Some(e.log_id.index))
            {
                return Err(error("nonconsecutive or corrupt retained log"));
            }
            previous = Some(e.log_id.index);
            bytes = bytes
                .checked_add(v.len() as u64)
                .ok_or_else(|| error("log bytes overflow"))?;
        }
        if bytes > max_log_bytes || result.read::<u64>(BYTES)?.unwrap_or(0) != bytes {
            return Err(error("retained log cap or accounting mismatch"));
        }
        if let Some(applied) = result.last_applied()? {
            let retained: Option<Entry<TypeConfig>> = result.read(&key(applied.index))?;
            if retained.as_ref().map(|e| e.log_id) != Some(applied) {
                return Err(error("applied marker has no matching retained log"));
            }
        }
        if let Some(committed) = result.committed()? {
            let retained: Option<Entry<TypeConfig>> = result.read(&key(committed.index))?;
            if retained.as_ref().map(|e| e.log_id) != Some(committed) {
                return Err(error("committed marker has no matching retained log"));
            }
        }
        Ok(result)
    }
    pub fn log_store(&self) -> LogStore {
        LogStore(self.clone())
    }
    pub fn state_machine(&self) -> StateMachine {
        StateMachine(self.clone())
    }
    fn read<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, Error> {
        self.0
            .db
            .get(key)
            .map_err(error)?
            .map(|v| serde_json::from_slice(&v).map_err(error))
            .transpose()
    }
    fn write(&self, batch: WriteBatch) -> Result<(), Error> {
        let mut opts = WriteOptions::default();
        opts.set_sync(true);
        self.0.db.write_opt(batch, &opts).map_err(error)
    }
    fn put<T: Serialize>(batch: &mut WriteBatch, key: &[u8], value: &T) -> Result<(), Error> {
        batch.put(key, serde_json::to_vec(value).map_err(error)?);
        Ok(())
    }
    pub fn lookup(&self, request_id: &str) -> Result<Option<Reply>, Error> {
        Ok(self
            .read::<Cached>(&dedupe_key(request_id))?
            .map(|c| c.reply))
    }
    pub fn last_applied(&self) -> Result<Option<LogId<u64>>, Error> {
        self.read(APPLIED)
    }
    pub fn committed(&self) -> Result<Option<LogId<u64>>, Error> {
        Ok(self.read::<Option<LogId<u64>>>(COMMITTED)?.flatten())
    }
    pub fn membership(&self) -> Result<StoredMembership<u64, BasicNode>, Error> {
        Ok(self.read(MEMBERSHIP)?.unwrap_or_default())
    }
    /// Durable proof that a newly applied registry command initialized graph metadata.
    pub fn registry_initialized(&self) -> Result<bool, Error> {
        Ok(self.read(REGISTRY_INITIALIZED)?.unwrap_or(false))
    }
    /// Configured cap for retained encoded log entries, excluding RocksDB overhead.
    pub fn max_log_bytes(&self) -> u64 {
        self.0.max_log_bytes
    }
    pub fn log_bytes(&self) -> Result<u64, Error> {
        Ok(self.read(BYTES)?.unwrap_or(0))
    }
    fn logs(
        &self,
        start: Bound<u64>,
        end: Bound<u64>,
        limited: bool,
    ) -> Result<Vec<Entry<TypeConfig>>, Error> {
        let _guard = self.0.io.lock().map_err(error)?;
        if matches!(start, Bound::Excluded(u64::MAX)) {
            return Ok(Vec::new());
        }
        let begin = match start {
            Bound::Included(n) => n,
            Bound::Excluded(n) => n.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        for item in self
            .0
            .db
            .iterator(IteratorMode::From(&key(begin), rocksdb::Direction::Forward))
        {
            let (k, v) = item.map_err(error)?;
            if k.first() != Some(&b'L') {
                break;
            }
            if v.len() > crate::types::MAX_ENTRY_BYTES {
                return Err(error("retained entry exceeds transport bound"));
            }
            let index = u64::from_be_bytes(
                k.get(1..)
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| error("invalid log key"))?,
            );
            if match end {
                Bound::Included(n) => index > n,
                Bound::Excluded(n) => index >= n,
                Bound::Unbounded => false,
            } {
                break;
            }
            if limited && !entries.is_empty() && bytes + v.len() > crate::types::MAX_ENTRY_BYTES {
                break;
            }
            let e: Entry<TypeConfig> = serde_json::from_slice(&v).map_err(error)?;
            if e.log_id.index != index {
                return Err(error("log key/index mismatch"));
            }
            bytes += v.len();
            entries.push(e);
        }
        Ok(entries)
    }
    fn last_log(&self) -> Result<Option<Entry<TypeConfig>>, Error> {
        self.0
            .db
            .iterator(IteratorMode::From(
                b"L\xff\xff\xff\xff\xff\xff\xff\xff",
                rocksdb::Direction::Reverse,
            ))
            .next()
            .transpose()
            .map_err(error)?
            .filter(|(k, _)| k.first() == Some(&b'L'))
            .map(|(_, v)| serde_json::from_slice(&v).map_err(error))
            .transpose()
    }
    fn append_entries(&self, entries: Vec<Entry<TypeConfig>>) -> Result<(), Error> {
        let _guard = self.0.io.lock().map_err(error)?;
        let mut bytes = self.log_bytes()?;
        let mut batch = WriteBatch::default();
        let last = self.last_log()?;
        if let (Some(first), Some(last)) = (entries.first(), last.as_ref()) {
            if first.log_id.index > last.log_id.index.saturating_add(1) {
                return Err(error("append would create log hole"));
            }
        }
        for pair in entries.windows(2) {
            if pair[0].log_id.index.checked_add(1) != Some(pair[1].log_id.index) {
                return Err(error("append entries must be consecutive"));
            }
        }
        for entry in entries {
            let k = key(entry.log_id.index);
            let v = serde_json::to_vec(&entry).map_err(error)?;
            if v.len() > crate::types::MAX_ENTRY_BYTES {
                return Err(error("entry exceeds bounded transport size"));
            }
            if let Some(old) = self.0.db.get(k).map_err(error)? {
                if old.as_slice() != v.as_slice() {
                    return Err(error("conflicting entry requires truncate"));
                }
                bytes = bytes
                    .checked_sub(old.len() as u64)
                    .ok_or_else(|| error("log accounting underflow"))?;
            }
            bytes = bytes
                .checked_add(v.len() as u64)
                .ok_or_else(|| error("log accounting overflow"))?;
            if bytes > self.0.max_log_bytes {
                return Err(error("retained log cap exhausted; purge unsupported"));
            }
            batch.put(k, v);
        }
        Self::put(&mut batch, BYTES, &bytes)?;
        self.write(batch)
    }
    fn apply_entries(&self, entries: Vec<Entry<TypeConfig>>) -> Result<Vec<Reply>, Error> {
        let mut fatal = self.0.sm.lock().map_err(error)?;
        if let Some(reason) = fatal.as_ref() {
            return Err(error(reason));
        }
        let result = (|| {
            let mut replies = Vec::with_capacity(entries.len());
            for entry in entries {
                if self
                    .last_applied()?
                    .is_some_and(|last| last.index.checked_add(1) != Some(entry.log_id.index))
                {
                    return Err(error("state machine apply must advance"));
                }
                let mut batch = WriteBatch::default();
                let reply = match entry.payload {
                    EntryPayload::Blank => Reply::Initialized,
                    EntryPayload::Membership(m) => {
                        Self::put(
                            &mut batch,
                            MEMBERSHIP,
                            &StoredMembership::new(Some(entry.log_id), m),
                        )?;
                        Reply::Initialized
                    }
                    EntryPayload::Normal(command) => {
                        command.validate().map_err(error)?;
                        let digest = command.digest().map_err(error)?;
                        let k = dedupe_key(command.request_id());
                        match self.read::<Cached>(&k)? {
                            Some(cached) if cached.digest == digest => cached.reply,
                            Some(_) => Reply::Conflict,
                            None => {
                                let reply = self.0.application.apply(&command).map_err(error)?;
                                if matches!(
                                    command,
                                    Command::Bootstrap { .. } | Command::CreateGraph { .. }
                                ) && reply == Reply::Initialized
                                {
                                    Self::put(&mut batch, REGISTRY_INITIALIZED, &true)?;
                                }
                                Self::put(
                                    &mut batch,
                                    &k,
                                    &Cached {
                                        digest,
                                        reply: reply.clone(),
                                    },
                                )?;
                                reply
                            }
                        }
                    }
                };
                Self::put(&mut batch, APPLIED, &entry.log_id)?;
                {
                    let _guard = self.0.io.lock().map_err(error)?;
                    self.write(batch)?;
                }
                replies.push(reply);
            }
            Ok(replies)
        })();
        if let Err(e) = &result {
            *fatal = Some(e.to_string());
        }
        result
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f).await.map_err(error)?
}
impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<R: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: R,
    ) -> Result<Vec<Entry<TypeConfig>>, Error> {
        let s = self.0.clone();
        let start = range.start_bound().cloned();
        let end = range.end_bound().cloned();
        blocking(move || s.logs(start, end, false)).await
    }
    async fn limited_get_log_entries(
        &mut self,
        start: u64,
        end: u64,
    ) -> Result<Vec<Entry<TypeConfig>>, Error> {
        let s = self.0.clone();
        blocking(move || s.logs(Bound::Included(start), Bound::Excluded(end), true)).await
    }
}
impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;
    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, Error> {
        let s = self.0.clone();
        blocking(move || {
            Ok(LogState {
                last_purged_log_id: None,
                last_log_id: s.last_log()?.map(|e| e.log_id),
            })
        })
        .await
    }
    async fn get_log_reader(&mut self) -> Self {
        self.clone()
    }
    async fn save_vote(&mut self, vote: &Vote<u64>) -> Result<(), Error> {
        let s = self.0.clone();
        let vote = *vote;
        blocking(move || {
            let _guard = s.0.io.lock().map_err(error)?;
            let mut b = WriteBatch::default();
            Storage::put(&mut b, VOTE, &vote)?;
            s.write(b)
        })
        .await
    }
    async fn read_vote(&mut self) -> Result<Option<Vote<u64>>, Error> {
        let s = self.0.clone();
        blocking(move || s.read(VOTE)).await
    }
    async fn save_committed(&mut self, committed: Option<LogId<u64>>) -> Result<(), Error> {
        let s = self.0.clone();
        blocking(move || {
            let _guard = s.0.io.lock().map_err(error)?;
            let mut b = WriteBatch::default();
            Storage::put(&mut b, COMMITTED, &committed)?;
            s.write(b)
        })
        .await
    }
    async fn read_committed(&mut self) -> Result<Option<LogId<u64>>, Error> {
        let s = self.0.clone();
        blocking(move || s.committed()).await
    }
    async fn append<I>(&mut self, entries: I, callback: LogFlushed<TypeConfig>) -> Result<(), Error>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries = entries.into_iter().collect();
        let s = self.0.clone();
        let result = blocking(move || s.append_entries(entries)).await;
        callback.log_io_completed(
            result
                .as_ref()
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string())),
        );
        result
    }
    async fn truncate(&mut self, log_id: LogId<u64>) -> Result<(), Error> {
        let s = self.0.clone();
        blocking(move || {
            let _guard = s.0.io.lock().map_err(error)?;
            if s.last_applied()?.is_some_and(|a| a.index >= log_id.index)
                || s.committed()?.is_some_and(|a| a.index >= log_id.index)
            {
                return Err(error("cannot truncate applied or committed log"));
            }
            let mut b = WriteBatch::default();
            let mut bytes = s.log_bytes()?;
            for item in s.0.db.iterator(IteratorMode::From(
                &key(log_id.index),
                rocksdb::Direction::Forward,
            )) {
                let (k, v) = item.map_err(error)?;
                if k.first() != Some(&b'L') {
                    break;
                }
                bytes = bytes
                    .checked_sub(v.len() as u64)
                    .ok_or_else(|| error("log accounting underflow"))?;
                b.delete(k);
            }
            Storage::put(&mut b, BYTES, &bytes)?;
            s.write(b)
        })
        .await
    }
    async fn purge(&mut self, _: LogId<u64>) -> Result<(), Error> {
        Err(error(
            "log purging unsupported; SnapshotPolicy::Never required",
        ))
    }
}
impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;
    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, BasicNode>), Error> {
        let s = self.0.clone();
        blocking(move || {
            let _guard = s.0.io.lock().map_err(error)?;
            Ok((s.last_applied()?, s.membership()?))
        })
        .await
    }
    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Reply>, Error>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let s = self.0.clone();
        let entries = entries.into_iter().collect();
        blocking(move || s.apply_entries(entries)).await
    }
    async fn get_snapshot_builder(&mut self) -> Self {
        self.clone()
    }
    async fn begin_receiving_snapshot(&mut self) -> Result<Box<std::io::Cursor<Vec<u8>>>, Error> {
        Err(error("snapshot transfer unsupported"))
    }
    async fn install_snapshot(
        &mut self,
        _: &SnapshotMeta<u64, BasicNode>,
        _: Box<std::io::Cursor<Vec<u8>>>,
    ) -> Result<(), Error> {
        Err(error("snapshot installation unsupported"))
    }
    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, Error> {
        Ok(None)
    }
}
impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, Error> {
        Err(error("snapshot building unsupported"))
    }
}
