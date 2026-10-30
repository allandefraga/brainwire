//! Shared admission for assembled input capacity and decoded storage.
//! Socket buffers, allocator slack and downstream engine copies are separate.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Debug)]
pub(crate) struct IngressPool {
    limit: usize,
    reserved: AtomicUsize,
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
        Arc::new(Self {
            limit,
            reserved: AtomicUsize::new(0),
        })
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
        let bytes = self.bytes.checked_add(extra).ok_or(Exhausted)?;
        self.pool
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(extra)
                    .filter(|next| *next <= self.pool.limit)
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
        let available = self.pool.limit.saturating_sub(reserved) + self.bytes;
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
        let pool = IngressPool::new(100);
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
