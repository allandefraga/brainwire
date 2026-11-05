//! Cooperative shared reservation accounting; not an allocator or RSS ceiling.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub type Headroom =
    Arc<dyn Fn() -> usize + Send + Sync + std::panic::RefUnwindSafe + std::panic::UnwindSafe>;

pub struct MemoryAdmission {
    reserved: AtomicUsize,
    headroom: Headroom,
}
impl MemoryAdmission {
    pub fn new(headroom: Headroom) -> Arc<Self> {
        Arc::new(Self {
            reserved: AtomicUsize::new(0),
            headroom,
        })
    }
    pub fn reserved(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }
    pub fn available(&self) -> usize {
        (self.headroom)().saturating_sub(self.reserved())
    }
    pub fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Credit> {
        if bytes != 0 {
            let limit = (self.headroom)();
            self.reserved
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(bytes).filter(|total| *total <= limit)
                })
                .ok()?;
        }
        Some(Credit {
            ledger: Arc::clone(self),
            bytes,
        })
    }
    /// Release previously committed credit. Ownership transfers must not call this.
    pub fn release(&self, bytes: usize) {
        if bytes != 0 {
            self.reserved
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_sub(bytes)
                })
                .expect("released more memory credit than reserved");
        }
    }
}
impl std::fmt::Debug for MemoryAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryAdmission")
            .field("reserved", &self.reserved())
            .finish_non_exhaustive()
    }
}
/// Rolls back a tentative reservation unless its owner commits it.
pub struct Credit {
    ledger: Arc<MemoryAdmission>,
    bytes: usize,
}
impl Credit {
    pub fn commit(mut self) {
        self.bytes = 0;
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.ledger.release(self.bytes);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_components_cannot_spend_same_headroom() {
        let ledger = MemoryAdmission::new(Arc::new(|| 100));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let jobs: Vec<_> = (0..2)
            .map(|_| {
                let (ledger, barrier) = (Arc::clone(&ledger), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    let credit = ledger.reserve(60);
                    barrier.wait();
                    let admitted = credit.is_some();
                    drop(credit);
                    admitted
                })
            })
            .collect();
        barrier.wait();
        barrier.wait();
        assert_eq!(
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .filter(|admitted| *admitted)
                .count(),
            1
        );
        assert_eq!(ledger.reserved(), 0);
    }
    #[test]
    fn provisional_credit_is_released_on_panic() {
        let ledger = MemoryAdmission::new(Arc::new(|| 100));
        let result = std::panic::catch_unwind({
            let ledger = Arc::clone(&ledger);
            move || {
                let _credit = ledger.reserve(60).unwrap();
                panic!("synthetic cancellation");
            }
        });
        assert!(result.is_err());
        assert_eq!(ledger.reserved(), 0);
    }
    #[test]
    fn provisional_credit_rolls_back_and_committed_credit_is_released_by_owner() {
        let ledger = MemoryAdmission::new(Arc::new(|| 100));
        drop(ledger.reserve(60).unwrap());
        assert_eq!(ledger.reserved(), 0);
        ledger.reserve(60).unwrap().commit();
        assert_eq!(ledger.reserved(), 60);
        ledger.release(60);
        assert_eq!(ledger.reserved(), 0);
    }
}
