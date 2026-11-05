//! Shared admission for assembled input capacity and decoded storage.
//! Socket buffers, allocator slack and downstream engine copies are separate.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

/// Sampled process growth allowance. Called synchronously; keep it bounded and nonblocking.
pub type ProcessHeadroom = Arc<dyn Fn() -> usize + Send + Sync>;

pub(crate) struct IngressPool {
    limit: usize,
    reserved: AtomicUsize,
    process_headroom: Option<ProcessHeadroom>,
    ledger: Option<Arc<brainwire_core::admission::MemoryAdmission>>,
}
impl std::fmt::Debug for IngressPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngressPool")
            .field("limit", &self.limit)
            .field("reserved", &self.reserved)
            .field(
                "process_headroom_configured",
                &self.process_headroom.is_some(),
            )
            .finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exhausted;
#[derive(Debug)]
pub(crate) struct IngressLease {
    pool: Arc<IngressPool>,
    bytes: usize,
}
impl IngressPool {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Self::with_process_headroom(limit, None)
    }
    pub(crate) fn with_process_headroom(
        limit: usize,
        process_headroom: Option<ProcessHeadroom>,
    ) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserved: AtomicUsize::new(0),
            process_headroom,
            ledger: None,
        })
    }
    pub(crate) fn with_ledger(
        limit: usize,
        ledger: Arc<brainwire_core::admission::MemoryAdmission>,
    ) -> Arc<Self> {
        Arc::new(Self {
            limit,
            reserved: AtomicUsize::new(0),
            process_headroom: None,
            ledger: Some(ledger),
        })
    }
    fn capacity(&self) -> usize {
        self.process_headroom
            .as_ref()
            .map_or(self.limit, |headroom| self.limit.min(headroom()))
    }
    pub(crate) fn lease(self: &Arc<Self>) -> IngressLease {
        IngressLease {
            pool: Arc::clone(self),
            bytes: 0,
        }
    }
    #[cfg(test)]
    pub(crate) fn reserved(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }
}
impl IngressLease {
    pub(crate) fn grow(&mut self, extra: usize) -> Result<(), Exhausted> {
        // A zero-field recovery message allocates nothing during decoding.
        if extra == 0 {
            return Ok(());
        }
        let bytes = self.bytes.checked_add(extra).ok_or(Exhausted)?;
        let credit = self
            .pool
            .ledger
            .as_ref()
            .map(|ledger| ledger.reserve(extra).ok_or(Exhausted))
            .transpose()?;
        let capacity = self.pool.capacity();
        self.pool
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(extra).filter(|next| *next <= capacity)
            })
            .map_err(|_| Exhausted)?;
        if let Some(credit) = credit {
            credit.commit();
        }
        self.bytes = bytes;
        Ok(())
    }
    pub(crate) fn release(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        self.pool.reserved.fetch_sub(bytes, Ordering::AcqRel);
        if let Some(ledger) = &self.pool.ledger {
            ledger.release(bytes);
        }
    }
    /// Size hint for a refusal: bytes this message asked for in total, and how many
    /// the shared pool could give it right now (its own share plus what is free).
    pub(crate) fn hint(&self, extra: usize) -> (u64, u64) {
        let reserved = self.pool.reserved.load(Ordering::Acquire);
        let available = self
            .pool
            .capacity()
            .saturating_sub(reserved)
            .saturating_add(self.bytes);
        let available = self.pool.ledger.as_ref().map_or(available, |ledger| {
            available.min(ledger.available().saturating_add(self.bytes))
        });
        (self.bytes.saturating_add(extra) as u64, available as u64)
    }
}
impl Drop for IngressLease {
    fn drop(&mut self) {
        self.pool.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
        if let Some(ledger) = &self.pool.ledger {
            ledger.release(self.bytes);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_query_and_ingress_pools_share_one_ledger() {
        use brainwire_cypher::{ExecutionLimits, QueryMemoryPool, QueryTransaction};
        use brainwire_store::db::{Database, Resources, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let available = Arc::new(AtomicUsize::new(1 << 20));
        let observed = Arc::clone(&available);
        let ledger = brainwire_core::admission::MemoryAdmission::new(Arc::new(move || {
            observed.load(Ordering::Acquire)
        }));
        let queries = Arc::new(QueryMemoryPool::new(1 << 20));
        queries.set_shared_admission(Arc::clone(&ledger));
        let ingress = IngressPool::with_ledger(1 << 20, Arc::clone(&ledger));
        let mut query = QueryTransaction::begin_with_pool(
            &db,
            ExecutionLimits {
                memory_bytes: 1 << 20,
                timeout: None,
            },
            Arc::clone(&queries),
        )
        .unwrap();
        let output = query
            .run("RETURN 1", &std::collections::BTreeMap::new())
            .unwrap();
        let query_bytes = queries.usage().reserved_bytes;
        assert!(query_bytes > 0);
        assert_eq!(ledger.reserved(), query_bytes);
        available.store(query_bytes + 60, Ordering::Release);
        let mut input = ingress.lease();
        input.grow(60).unwrap();
        assert_eq!(ledger.reserved(), query_bytes + 60);
        assert_eq!(input.grow(1), Err(Exhausted));
        assert!(matches!(
            query.run("RETURN range(1,1000)", &std::collections::BTreeMap::new()),
            Err(brainwire_cypher::ExecError::ProcessMemoryLimit { .. })
        ));
        drop(output);
        drop(query);
        assert_eq!(queries.usage().reserved_bytes, 0);
        assert_eq!(ledger.reserved(), 60);
        drop(input);
        assert_eq!(ledger.reserved(), 0);
    }
    #[test]
    fn shared_admission_rolls_back_local_failure_and_releases_at_zero_headroom() {
        let available = Arc::new(AtomicUsize::new(100));
        let observed = Arc::clone(&available);
        let ledger = brainwire_core::admission::MemoryAdmission::new(Arc::new(move || {
            observed.load(Ordering::Acquire)
        }));
        let pool = IngressPool::with_ledger(50, Arc::clone(&ledger));
        let mut lease = pool.lease();
        assert_eq!(lease.grow(60), Err(Exhausted));
        assert_eq!(ledger.reserved(), 0);
        let query = ledger.reserve(60).unwrap();
        assert_eq!(lease.grow(50), Err(Exhausted));
        drop(query);
        lease.grow(50).unwrap();
        available.store(0, Ordering::Release);
        assert_eq!(lease.grow(0), Ok(()));
        assert_eq!(lease.grow(1), Err(Exhausted));
        lease.release(20);
        assert_eq!(ledger.reserved(), 30);
        drop(lease);
        assert_eq!(ledger.reserved(), 0);
        assert_eq!(pool.reserved(), 0);
    }
    #[test]
    fn falling_process_headroom_refuses_growth_but_preserves_release() {
        let available = Arc::new(AtomicUsize::new(10));
        let observed = Arc::clone(&available);
        let pool = IngressPool::with_process_headroom(
            100,
            Some(Arc::new(move || observed.load(Ordering::Acquire))),
        );
        let mut a = pool.lease();
        let mut b = pool.lease();
        a.grow(6).unwrap();
        assert_eq!(b.grow(5), Err(Exhausted));
        b.grow(4).unwrap();
        available.store(0, Ordering::Release);
        assert_eq!(a.grow(0), Ok(()));
        assert_eq!(a.grow(1), Err(Exhausted));
        assert_eq!(a.hint(1), (7, 6));
        drop(a);
        drop(b);
        assert_eq!(pool.reserved(), 0);
        available.store(10, Ordering::Release);
        pool.lease().grow(10).unwrap();
    }
    #[test]
    fn concurrent_leases_respect_exact_cap_and_release_once() {
        let pool = IngressPool::new(10);
        let mut a = pool.lease();
        let mut b = pool.lease();
        a.grow(6).unwrap();
        b.grow(4).unwrap();
        assert_eq!(b.grow(1), Err(Exhausted));
        assert_eq!(pool.reserved(), 10);
        drop(a);
        assert_eq!(pool.reserved(), 4);
        b.grow(6).unwrap();
        drop(b);
        assert_eq!(pool.reserved(), 0);
    }
    #[test]
    fn overflow_does_not_change_accounting() {
        let pool = IngressPool::new(usize::MAX);
        let mut lease = pool.lease();
        lease.grow(usize::MAX).unwrap();
        assert_eq!(lease.grow(1), Err(Exhausted));
        assert_eq!(pool.reserved(), usize::MAX);
        drop(lease);
        assert_eq!(pool.reserved(), 0);
    }
    #[test]
    fn contention_never_overbooks() {
        let pool = IngressPool::with_process_headroom(1000, Some(Arc::new(|| 100)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let pool = Arc::clone(&pool);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        let mut lease = pool.lease();
                        if lease.grow(51).is_ok() {
                            assert!(pool.reserved() <= 100);
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(pool.reserved(), 0);
    }
}
