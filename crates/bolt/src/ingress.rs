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
        let capacity = self.pool.capacity();
        self.pool
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(extra).filter(|next| *next <= capacity)
            })
            .map_err(|_| Exhausted)?;
        self.bytes = bytes;
        Ok(())
    }
    pub(crate) fn release(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        self.pool.reserved.fetch_sub(bytes, Ordering::AcqRel);
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
        (self.bytes.saturating_add(extra) as u64, available as u64)
    }
}
impl Drop for IngressLease {
    fn drop(&mut self) {
        self.pool.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
