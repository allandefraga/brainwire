//! Id allocation in blocks. The end of a block is committed before any id in
//! it is used, so a crash never reuses an id (spec 1.3).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use brainwire_core::ids::{EdgeId, NodeId, TypeId};

use crate::db::{Database, StoreError};
use crate::txn::{Cf, Txn, TxnError};

#[derive(Default)]
pub(crate) struct IdAllocator {
    blocks: Mutex<HashMap<Vec<u8>, Block>>,
}

impl IdAllocator {
    /// The one place that locks the in-memory blocks. A poisoned lock means
    /// an earlier holder panicked while a block's in-memory state might not
    /// match what has actually been handed out, so every in-memory block is
    /// dropped rather than trusted: the next allocation for any key reserves
    /// a fresh block starting from the persisted counter (spec 1.3 — a
    /// block's end is committed to RocksDB before any id in it is used), so
    /// ids already handed out from a dropped block are simply skipped,
    /// wasted but never reused or repeated. Clears the poison flag for the
    /// same reason as the committer's equivalent (`txn::Committer::lock`):
    /// left set, this recovery — dropping every block again — would rerun
    /// on every later allocation instead of once.
    fn lock(&self) -> MutexGuard<'_, HashMap<Vec<u8>, Block>> {
        self.blocks.lock().unwrap_or_else(|poisoned| {
            let mut blocks = poisoned.into_inner();
            blocks.clear();
            self.blocks.clear_poison();
            blocks
        })
    }
}

#[derive(Clone, Copy)]
struct Block {
    next: u64,
    end: u64,
}

const EDGE_COUNTER_KEY: &[u8] = b"seq/edge";

pub(crate) fn node_counter_key(ty: TypeId) -> Vec<u8> {
    let mut key = b"seq/node/".to_vec();
    key.extend_from_slice(&ty.0.to_be_bytes());
    key
}

impl Database {
    pub fn next_node_id(&self, ty: TypeId) -> Result<NodeId, StoreError> {
        let seq = self.next_seq(node_counter_key(ty), 0)?;
        NodeId::new(0, ty, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    pub fn next_edge_id(&self) -> Result<EdgeId, StoreError> {
        let seq = self.next_seq(EDGE_COUNTER_KEY.to_vec(), 1)?;
        EdgeId::new(0, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    fn next_seq(&self, key: Vec<u8>, first: u64) -> Result<u64, StoreError> {
        let mut blocks = self.alloc.lock();
        let exhausted = blocks.get(&key).is_none_or(|b| b.next >= b.end);
        if exhausted {
            let block = self.reserve(&key, first)?;
            blocks.insert(key.clone(), block);
        }
        let block = blocks.get_mut(&key).expect("block present");
        let seq = block.next;
        block.next += 1;
        Ok(seq)
    }

    fn reserve(&self, key: &[u8], first: u64) -> Result<Block, StoreError> {
        loop {
            let mut txn = self.begin();
            let current = match txn.get_for_update(Cf::Meta, key)? {
                Some(v) => u64::from_be_bytes(
                    v.as_slice()
                        .try_into()
                        .map_err(|_| StoreError::Corrupt("id counter".into()))?,
                ),
                None => first,
            };
            let end = current + self.id_block;
            txn.put(Cf::Meta, key.to_vec(), end.to_be_bytes().to_vec());
            match txn.commit() {
                Ok(_) => return Ok(Block { next: current, end }),
                Err(TxnError::Conflict) => continue,
                Err(TxnError::Store(e)) => return Err(e),
            }
        }
    }
}

impl Txn<'_> {
    pub fn next_node_id(&mut self, ty: TypeId) -> Result<NodeId, StoreError> {
        if !self.replicated_ids {
            self.used_standalone_ids = true;
            return self.database().next_node_id(ty);
        }
        NodeId::new(0, ty, 0).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let seq = self.replicated_seq(node_counter_key(ty), 0, 1u64 << 42)?;
        NodeId::new(0, ty, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    pub fn next_edge_id(&mut self) -> Result<EdgeId, StoreError> {
        if !self.replicated_ids {
            self.used_standalone_ids = true;
            return self.database().next_edge_id();
        }
        let seq = self.replicated_seq(EDGE_COUNTER_KEY.to_vec(), 1, 1u64 << 54)?;
        EdgeId::new(0, seq).map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    fn replicated_seq(&mut self, key: Vec<u8>, first: u64, bound: u64) -> Result<u64, StoreError> {
        let current = self
            .get_for_update(Cf::Meta, &key)?
            .map(|bytes| {
                let raw: [u8; 8] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| StoreError::Corrupt("id counter".into()))?;
                Ok::<_, StoreError>(u64::from_be_bytes(raw))
            })
            .transpose()?
            .unwrap_or(first);
        if current < first || current >= bound {
            return Err(StoreError::Corrupt("id sequence exhausted".into()));
        }
        let next = current
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("id sequence exhausted".into()))?;
        self.put(Cf::Meta, key, next.to_be_bytes().to_vec());
        Ok(current)
    }
}

impl Database {
    pub(crate) fn clear_id_cache_after_apply(&self) {
        self.alloc.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use brainwire_core::ids::{EdgeId, NodeId, TypeId};

    use crate::db::{Database, Resources, StoreConfig, StoreError};
    use crate::txn::Cf;

    #[test]
    fn ids_grow_and_never_repeat_across_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let ids: Vec<NodeId> = (0..10)
            .map(|_| db.next_node_id(TypeId(1)).unwrap())
            .collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_reopen_never_reuses_ids() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let first = Database::open(dir.path(), &res)
            .unwrap()
            .next_node_id(TypeId(1))
            .unwrap();
        let second = Database::open(dir.path(), &res)
            .unwrap()
            .next_node_id(TypeId(1))
            .unwrap();
        assert!(second.seq() >= first.seq() + StoreConfig::for_tests().id_block);
    }

    #[test]
    fn edge_ids_start_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        assert_eq!(db.next_edge_id().unwrap(), EdgeId::new(0, 1).unwrap());
    }

    #[test]
    fn concurrent_allocation_is_unique() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        let mut all: Vec<NodeId> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        (0..50)
                            .map(|_| db.next_node_id(TypeId(2)).unwrap())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 200);
    }

    // Additional tests beyond the brief: the plan's carried-over review rule
    // requires every public function and every error path to be exercised by
    // a test, and the task dispatch additionally asked for a stress test that
    // shares one `Database` across real OS threads via `Arc` (as a server
    // would), not just borrowed scoped threads.

    #[test]
    fn next_node_id_rejects_an_out_of_range_type() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();
        // TypeId(u16) accepts any u16 at construction; only NodeId::new
        // rejects one over TypeId::MAX, so next_node_id must propagate that
        // rejection as an error instead of panicking.
        match db.next_node_id(TypeId(TypeId::MAX + 1)) {
            Err(StoreError::Corrupt(_)) => {}
            other => panic!("expected StoreError::Corrupt for an out-of-range type, got {other:?}"),
        }
    }

    #[test]
    fn next_node_id_errors_instead_of_panicking_at_the_sequence_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();

        // Seed the counter one block before the 42-bit sequence ceiling so
        // the very next reservation straddles it.
        let boundary = (1u64 << 42) - 1;
        let mut seed = db.begin();
        seed.put(
            Cf::Meta,
            super::node_counter_key(TypeId(9)),
            boundary.to_be_bytes().to_vec(),
        );
        seed.commit().unwrap();

        // The last id that still fits in 42 bits is handed out normally.
        let last_valid = db.next_node_id(TypeId(9)).unwrap();
        assert_eq!(last_valid.seq(), boundary);

        // The next id in the same block overflows the 42-bit sequence;
        // NodeId::new's rejection must come back as an error, not a panic.
        match db.next_node_id(TypeId(9)) {
            Err(StoreError::Corrupt(_)) => {}
            other => panic!("expected StoreError::Corrupt at the sequence boundary, got {other:?}"),
        }
    }

    #[test]
    fn next_edge_id_errors_instead_of_panicking_at_the_sequence_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();

        // Same boundary proof as above, for the 54-bit edge sequence.
        let boundary = (1u64 << 54) - 1;
        let mut seed = db.begin();
        seed.put(
            Cf::Meta,
            super::EDGE_COUNTER_KEY.to_vec(),
            boundary.to_be_bytes().to_vec(),
        );
        seed.commit().unwrap();

        let last_valid = db.next_edge_id().unwrap();
        assert_eq!(last_valid, EdgeId::new(0, boundary).unwrap());

        match db.next_edge_id() {
            Err(StoreError::Corrupt(_)) => {}
            other => panic!("expected StoreError::Corrupt at the sequence boundary, got {other:?}"),
        }
    }

    #[test]
    fn reserve_propagates_a_store_error_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        // Create every column family, then close the read-write handle so a
        // second, read-only handle can attach to the same path (mirrors
        // txn.rs's a_failed_write_is_a_store_error_not_a_panic).
        drop(Database::open(dir.path(), &res).unwrap());

        let opts = rocksdb::Options::default();
        let cfs = crate::db::COLUMN_FAMILIES
            .iter()
            .map(|name| rocksdb::ColumnFamilyDescriptor::new(*name, rocksdb::Options::default()));
        let ro = rocksdb::DB::open_cf_descriptors_read_only(&opts, dir.path(), cfs, false).unwrap();
        let db = Database::for_test(ro, 1_000_000);

        // The reservation's read succeeds (reads work against a read-only
        // handle); its commit's write does not, and reserve's
        // `Err(TxnError::Store(e)) => return Err(e)` branch must surface
        // that as a StoreError rather than panicking or looping forever.
        match db.next_node_id(TypeId(1)) {
            Err(StoreError::Rocks(_)) => {}
            other => panic!(
                "expected StoreError::Rocks for a write against a read-only db, got {other:?}"
            ),
        }
    }

    #[test]
    fn concurrent_allocation_from_an_arc_database_is_unique() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        // Arc, not a borrow: this is how a real server shares one Database
        // across connection-handling threads, unlike the scoped-borrow test
        // above.
        let db = std::sync::Arc::new(Database::open(dir.path(), &res).unwrap());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let db = std::sync::Arc::clone(&db);
                std::thread::spawn(move || {
                    (0..50)
                        .map(|_| db.next_node_id(TypeId(3)).unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<NodeId> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 200);
    }

    // Follow-up (Task 8 review, Important): `.expect("allocator lock")`
    // panicked on a poisoned lock, wedging id allocation for the life of the
    // process — unlike the committer, which already recovered. Proves the
    // fix: after a real poisoning, both `next_node_id` and `next_edge_id`
    // keep working, every id handed out afterward is greater than every id
    // handed out before, and nothing repeats — the in-memory blocks at the
    // time of poisoning are dropped and never trusted again, so their unused
    // tail is skipped rather than reused.
    #[test]
    fn a_poisoned_allocator_recovers_and_never_reuses_or_repeats_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let res = Resources::new(StoreConfig::for_tests());
        let db = Database::open(dir.path(), &res).unwrap();

        // Hand out ids before poisoning, so recovery has something at risk
        // of reusing if it trusted the in-memory blocks instead of dropping
        // them.
        let before_nodes: Vec<NodeId> = (0..5)
            .map(|_| db.next_node_id(TypeId(1)).unwrap())
            .collect();
        let before_edges: Vec<EdgeId> = (0..5).map(|_| db.next_edge_id().unwrap()).collect();

        let panicked = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _guard = db.alloc.blocks.lock().unwrap();
                    panic!("simulated panic while holding the allocator lock");
                })
                .join()
                .is_err()
        });
        assert!(panicked, "expected the spawned thread to panic");

        // Neither call panics on the now-poisoned lock.
        let after_nodes: Vec<NodeId> = (0..5)
            .map(|_| db.next_node_id(TypeId(1)).unwrap())
            .collect();
        let after_edges: Vec<EdgeId> = (0..5).map(|_| db.next_edge_id().unwrap()).collect();

        assert!(after_nodes
            .iter()
            .all(|after| before_nodes.iter().all(|before| after > before)));
        assert!(after_edges
            .iter()
            .all(|after| before_edges.iter().all(|before| after > before)));

        let mut all_nodes = before_nodes;
        all_nodes.extend(after_nodes);
        let node_count = all_nodes.len();
        all_nodes.sort();
        all_nodes.dedup();
        assert_eq!(all_nodes.len(), node_count, "a node id repeated");

        let mut all_edges = before_edges;
        all_edges.extend(after_edges);
        let edge_count = all_edges.len();
        all_edges.sort();
        all_edges.dedup();
        assert_eq!(all_edges.len(), edge_count, "an edge id repeated");
    }
}
