//! Conservative per-RUN allocation accounting, not a process RSS limit.
//! Scoped read-pipeline temporaries are released; eager operators retain charges.
//! ponytail: pool reservations keep each transaction's high-water estimate until
//! it ends; replace with allocator-aware leases when exact live accounting exists.
use super::*;
use std::cell::Cell;
use std::mem::size_of;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

/// Shared accounting for transaction high-water reservations, not allocator RSS.
pub struct QueryMemoryPool {
    limit: usize,
    reserved: AtomicUsize,
    peak: AtomicUsize,
    rejections: AtomicUsize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryMemoryUsage {
    pub limit_bytes: usize,
    pub reserved_bytes: usize,
    pub peak_bytes: usize,
    pub rejections: usize,
}
impl QueryMemoryPool {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            limit: limit_bytes,
            reserved: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            rejections: AtomicUsize::new(0),
        }
    }
    /// Individual counters are atomic; this telemetry is not one atomic snapshot.
    pub fn usage(&self) -> QueryMemoryUsage {
        QueryMemoryUsage {
            limit_bytes: self.limit,
            reserved_bytes: self.reserved.load(Ordering::Relaxed),
            peak_bytes: self.peak.load(Ordering::Relaxed),
            rejections: self.rejections.load(Ordering::Relaxed),
        }
    }
}
#[derive(Clone)]
pub(super) struct Reservation(Arc<ReservationInner>);

pub(super) struct ReservationInner {
    pool: Arc<QueryMemoryPool>,
    bytes: AtomicUsize,
}
impl std::ops::Deref for Reservation {
    type Target = ReservationInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Reservation {
    pub fn new(pool: Arc<QueryMemoryPool>) -> Self {
        Self(Arc::new(ReservationInner {
            pool,
            bytes: AtomicUsize::new(0),
        }))
    }
    // Each reservation has one charging executor; workers own separate leases.
    fn grow_to(&self, bytes: usize) -> Result<(), ExecError> {
        let extra = bytes.saturating_sub(self.bytes.load(Ordering::Relaxed));
        if extra == 0 {
            return Ok(());
        }
        let result =
            self.pool
                .reserved
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |reserved| {
                    reserved
                        .checked_add(extra)
                        .filter(|total| *total <= self.pool.limit)
                });
        match result {
            Ok(previous) => {
                self.bytes.store(bytes, Ordering::Relaxed);
                self.pool
                    .peak
                    .fetch_max(previous + extra, Ordering::Relaxed);
                Ok(())
            }
            Err(reserved) => {
                self.pool.rejections.fetch_add(1, Ordering::Relaxed);
                Err(ExecError::MemoryPoolExhausted {
                    limit: self.pool.limit,
                    reserved,
                    requested: extra,
                })
            }
        }
    }
    fn absorb(&self, other: Reservation) {
        assert_eq!(
            Arc::strong_count(&other.0),
            1,
            "worker reservation must have one owner before transfer"
        );
        assert!(Arc::ptr_eq(&self.pool, &other.pool));
        let bytes = other.bytes.swap(0, Ordering::Relaxed);
        // Both leases already contribute to the same bounded pool total.
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}
impl Drop for ReservationInner {
    fn drop(&mut self) {
        self.pool
            .reserved
            .fetch_sub(self.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionLimits {
    pub memory_bytes: usize,
    /// Cooperative per-RUN execution deadline; parsing/storage calls can overshoot.
    pub timeout: Option<Duration>,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            memory_bytes: usize::MAX,
            timeout: None,
        }
    }
}

pub(super) struct Budget<'a> {
    limit: usize,
    used: Cell<usize>,
    retained: Cell<usize>,
    peak: Cell<usize>,
    reservation: Option<&'a Reservation>,
    deadline: Option<Instant>,
    ticks: Cell<u32>,
}
impl<'a> Budget<'a> {
    pub fn new(limits: ExecutionLimits, reservation: Option<&'a Reservation>) -> Self {
        Self {
            limit: limits.memory_bytes,
            used: Cell::new(0),
            retained: Cell::new(0),
            peak: Cell::new(0),
            reservation,
            deadline: limits
                .timeout
                .and_then(|timeout| Instant::now().checked_add(timeout)),
            ticks: Cell::new(0),
        }
    }
    pub fn charge(&self, bytes: usize) -> Result<(), ExecError> {
        self.tick()?;
        let used = self.used.get().saturating_add(bytes);
        let attempted = used.saturating_add(self.retained.get());
        if attempted > self.limit {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted,
            });
        }
        if let Some(reservation) = self.reservation {
            reservation.grow_to(attempted)?;
        }
        self.used.set(used);
        self.peak.set(self.peak.get().max(attempted));
        Ok(())
    }
    /// Optional raw-row caching must not reject a query that needs only IDs.
    /// Admit growth and replacement overlap together, before changing accounting.
    pub fn try_retain_cache(&self, growth: usize, overlap: usize) -> Result<bool, ExecError> {
        self.check_deadline()?;
        let retained = self.retained.get().checked_add(growth);
        let attempted = retained
            .and_then(|retained| self.used.get().checked_add(retained))
            .and_then(|used| used.checked_add(overlap));
        let Some(attempted) = attempted.filter(|attempted| *attempted <= self.limit) else {
            return Ok(false);
        };
        if let Some(reservation) = self.reservation {
            match reservation.grow_to(attempted) {
                Ok(()) => {}
                Err(ExecError::MemoryPoolExhausted { .. }) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        self.retained.set(retained.expect("checked above"));
        self.peak.set(self.peak.get().max(attempted));
        Ok(true)
    }

    /// Deadline check for hot paths: reads the clock on every 32nd call.
    pub fn tick(&self) -> Result<(), ExecError> {
        if self.deadline.is_none() {
            return Ok(());
        }
        let ticks = self.ticks.get().wrapping_add(1);
        self.ticks.set(ticks);
        if ticks % 32 == 1 {
            self.check_deadline()?;
        }
        Ok(())
    }
    pub fn check_deadline(&self) -> Result<(), ExecError> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ExecError::TimedOut);
        }
        Ok(())
    }
    pub fn retain(&self, bytes: usize) -> Result<(), ExecError> {
        self.charge(bytes)?;
        self.used.set(self.used.get().saturating_sub(bytes));
        self.retained.set(self.retained.get().saturating_add(bytes));
        Ok(())
    }
    /// Restore only temporary accounting when this row leaves the pipeline.
    /// Output rows and eager collections use `retain` and survive this scope.
    pub fn scope(&self) -> Scope<'_> {
        Scope {
            budget: self,
            mark: self.used.get(),
        }
    }
    /// Independent lease for one parallel worker; keep it until results are merged.
    pub fn worker_reservation(&self) -> Option<Reservation> {
        self.reservation
            .map(|parent| Reservation::new(Arc::clone(&parent.pool)))
    }
    /// A worker gets an equal share of the remaining query allowance and its own
    /// pool lease, so charging workers never concurrently mutate one reservation.
    pub fn fork<'b>(&self, parts: usize, reservation: Option<&'b Reservation>) -> Budget<'b> {
        Budget {
            limit: self.remaining() / parts.max(1),
            used: Cell::new(0),
            retained: Cell::new(0),
            peak: Cell::new(0),
            reservation,
            deadline: self.deadline,
            ticks: Cell::new(0),
        }
    }
    pub fn high_water(&self) -> usize {
        self.peak.get()
    }
    /// Retain worker high-water accounting while partials and merged data coexist.
    /// On failure, leases stay with the caller until it drops the partial results.
    pub fn absorb_worker_reservations(
        &self,
        reservations: &mut [Option<Reservation>],
        high_waters: impl IntoIterator<Item = usize>,
    ) -> Result<(), ExecError> {
        self.check_deadline()?;
        let bytes = high_waters.into_iter().try_fold(0usize, usize::checked_add);
        let retained = bytes.and_then(|bytes| self.retained.get().checked_add(bytes));
        let attempted = retained.and_then(|retained| self.used.get().checked_add(retained));
        let Some(attempted) = attempted.filter(|attempted| *attempted <= self.limit) else {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted: attempted.unwrap_or(usize::MAX),
            });
        };
        if let Some(parent) = self.reservation {
            for reservation in reservations.iter_mut().filter_map(Option::take) {
                parent.absorb(reservation);
            }
        }
        self.retained.set(attempted - self.used.get());
        self.peak.set(self.peak.get().max(attempted));
        Ok(())
    }
    pub fn pool_usage(&self) -> Option<QueryMemoryUsage> {
        self.reservation.map(|r| r.pool.usage())
    }
    pub fn remaining(&self) -> usize {
        self.limit
            .saturating_sub(self.used.get())
            .saturating_sub(self.retained.get())
    }
    pub fn bindings(&self, row: &Bindings) -> Result<(), ExecError> {
        self.charge(row.iter().fold(size_of::<Bindings>(), |sum, (key, value)| {
            sum.saturating_add(128)
                .saturating_add(key.len())
                .saturating_add(value.bytes())
        }))
    }
    /// Transient charge for one slot row.
    pub fn row(&self, row: &[super::val::Val]) -> Result<(), ExecError> {
        self.charge(Self::row_bytes(row))
    }
    pub fn retain_row(&self, row: &[super::val::Val]) -> Result<(), ExecError> {
        self.retain(Self::row_bytes(row))
    }
    /// Admit the worker's owned bindings before deep-cloning strings or lists.
    pub fn clone_row(&self, row: &[super::val::Val]) -> Result<Vec<super::val::Val>, ExecError> {
        self.retain_row(row)?;
        Ok(row.to_vec())
    }
    /// Give back what [`Self::retain_row`] retained for a row dropped early.
    pub fn release_row(&self, row: &[super::val::Val]) {
        let bytes = Self::row_bytes(row);
        self.retained.set(self.retained.get().saturating_sub(bytes));
    }
    fn row_bytes(row: &[super::val::Val]) -> usize {
        row.iter()
            .fold(size_of::<Vec<super::val::Val>>() + 32, |sum, value| {
                sum.saturating_add(value.bytes())
            })
    }
    pub fn output(&self, row: &[Out]) -> Result<(), ExecError> {
        self.retain(Self::output_bytes(row))
    }
    fn output_bytes(row: &[Out]) -> usize {
        size_of::<Vec<Out>>()
            + row
                .iter()
                .fold(0usize, |sum, out| sum.saturating_add(Self::out_bytes(out)))
    }
    fn out_bytes(out: &Out) -> usize {
        size_of::<Out>().saturating_add(match out {
            Out::Value(v) => value_bytes(v),
            Out::Node(n) => n.label.len().saturating_add(map_bytes(&n.props)),
            Out::Relationship(r) => r
                .rel_type
                .len()
                .saturating_add(r.element_id.len())
                .saturating_add(map_bytes(&r.props)),
            Out::Path(p) => p
                .nodes
                .iter()
                .map(|n| Self::out_bytes(&Out::Node(n.clone())))
                .chain(
                    p.rels
                        .iter()
                        .map(|r| Self::out_bytes(&Out::Relationship(r.clone()))),
                )
                .fold(0usize, usize::saturating_add),
            Out::List(items) => items
                .iter()
                .map(Self::out_bytes)
                .fold(0usize, usize::saturating_add),
            Out::Map(map) => map
                .iter()
                .map(|(k, v)| {
                    k.len()
                        .saturating_add(64)
                        .saturating_add(Self::out_bytes(v))
                })
                .fold(0usize, usize::saturating_add),
        })
    }
}
pub(super) struct Scope<'a> {
    budget: &'a Budget<'a>,
    mark: usize,
}
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.budget.used.set(self.mark);
    }
}
pub(super) fn map_bytes(map: &BTreeMap<String, Value>) -> usize {
    map.iter()
        .fold(size_of::<BTreeMap<String, Value>>(), |sum, (key, value)| {
            sum.saturating_add(128)
                .saturating_add(key.len())
                .saturating_add(value_bytes(value))
        })
}
pub(super) fn value_bytes(value: &Value) -> usize {
    size_of::<Value>().saturating_add(match value {
        Value::Str(s) => s.len(),
        Value::List(items) => items
            .iter()
            .fold(0usize, |sum, v| sum.saturating_add(value_bytes(v))),
        Value::Map(items) => map_bytes(items),
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_cache_admission_skips_pool_pressure_without_partial_charge() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let parent_reservation = Reservation::new(Arc::clone(&pool));
        let mut parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 1000,
                timeout: None,
            },
            Some(&parent_reservation),
        );
        parent.charge(100).unwrap();
        let competing = Reservation::new(Arc::clone(&pool));
        competing.grow_to(800).unwrap();
        assert!(!parent.try_retain_cache(200, 0).unwrap());
        assert_eq!(parent.remaining(), 900);
        assert_eq!(parent.high_water(), 100);
        assert_eq!(pool.usage().reserved_bytes, 900);
        drop(competing);
        assert!(!parent.try_retain_cache(1000, 0).unwrap());
        assert_eq!(parent.remaining(), 900);
        assert_eq!(pool.usage().reserved_bytes, 100);
        assert!(parent.try_retain_cache(200, 100).unwrap());
        assert_eq!(parent.remaining(), 700);
        assert_eq!(parent.high_water(), 400);
        assert_eq!(pool.usage().reserved_bytes, 400);
        parent.deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(matches!(
            parent.try_retain_cache(1, 0),
            Err(ExecError::TimedOut)
        ));
    }

    #[test]
    fn parallel_worker_allocations_reserve_shared_pool_capacity() {
        let pool = Arc::new(QueryMemoryPool::new(700));
        let parent_reservation = Reservation::new(Arc::clone(&pool));
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 1000,
                timeout: None,
            },
            Some(&parent_reservation),
        );
        parent.charge(100).unwrap();
        let worker_reservation = parent.worker_reservation();
        let worker = parent.fork(2, worker_reservation.as_ref());
        worker.charge(400).unwrap();
        assert_eq!(pool.usage().reserved_bytes, 500);
        let competing = Reservation::new(Arc::clone(&pool));
        assert!(matches!(
            competing.grow_to(300),
            Err(ExecError::MemoryPoolExhausted { .. })
        ));
    }

    #[test]
    fn joining_workers_transfers_accounting_without_double_reserving_or_leaking() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        {
            let parent_reservation = Reservation::new(Arc::clone(&pool));
            let parent = Budget::new(
                ExecutionLimits {
                    memory_bytes: 1000,
                    timeout: None,
                },
                Some(&parent_reservation),
            );
            parent.charge(100).unwrap();
            let mut leases: Vec<_> = (0..2).map(|_| parent.worker_reservation()).collect();
            for lease in &leases {
                parent.fork(2, lease.as_ref()).retain(200).unwrap();
            }
            assert_eq!(pool.usage().reserved_bytes, 500);
            parent
                .absorb_worker_reservations(&mut leases, [200, 200])
                .unwrap();
            assert_eq!(pool.usage().reserved_bytes, 500);
            assert_eq!(parent.remaining(), 500);
            {
                let _scope = parent.scope();
                parent.charge(100).unwrap();
            }
            assert_eq!(parent.remaining(), 500);
            assert_eq!(pool.usage().reserved_bytes, 600);
        }
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn oversized_worker_input_is_rejected_before_clone() {
        let pool = Arc::new(QueryMemoryPool::new(100));
        let reservation = Reservation::new(Arc::clone(&pool));
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 4096,
                timeout: None,
            },
            Some(&reservation),
        );
        let lease = parent.worker_reservation();
        let worker = parent.fork(2, lease.as_ref());
        let row = vec![super::super::val::Val::Str("x".repeat(1024))];
        assert!(matches!(
            worker.clone_row(&row),
            Err(ExecError::MemoryPoolExhausted { .. })
        ));
        assert_eq!(worker.high_water(), 0);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn worker_high_water_is_adopted_without_a_shared_pool() {
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 1000,
                timeout: None,
            },
            None,
        );
        parent.charge(100).unwrap();
        let worker = parent.fork(2, None);
        {
            let _scope = worker.scope();
            worker.charge(400).unwrap();
        }
        assert_eq!(worker.high_water(), 400);
        let mut leases = vec![None];
        parent
            .absorb_worker_reservations(&mut leases, [worker.high_water()])
            .unwrap();
        assert_eq!(parent.remaining(), 500);
        assert!(matches!(
            parent.charge(501),
            Err(ExecError::MemoryLimit { .. })
        ));
    }

    #[test]
    fn rejected_adoption_and_panicking_worker_release_their_leases() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let parent_reservation = Reservation::new(Arc::clone(&pool));
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 300,
                timeout: None,
            },
            Some(&parent_reservation),
        );
        parent.charge(100).unwrap();
        let worker = parent.worker_reservation().unwrap();
        worker.grow_to(250).unwrap();
        let mut leases = vec![Some(worker)];
        assert!(matches!(
            parent.absorb_worker_reservations(&mut leases, [250]),
            Err(ExecError::MemoryLimit {
                limit: 300,
                attempted: 350
            })
        ));
        assert_eq!(pool.usage().reserved_bytes, 350);
        assert!(leases[0].is_some());
        drop(leases);
        assert_eq!(pool.usage().reserved_bytes, 100);
        assert_eq!(parent.remaining(), 200);
        let pool_for_panic = Arc::clone(&pool);
        assert!(std::panic::catch_unwind(move || {
            let worker = Reservation::new(pool_for_panic);
            worker.grow_to(200).unwrap();
            panic!("synthetic worker panic");
        })
        .is_err());
        assert_eq!(pool.usage().reserved_bytes, 100);
        drop(parent_reservation);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn deadline_failure_keeps_worker_leases_until_caller_releases_results() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let reservation = Reservation::new(Arc::clone(&pool));
        let mut parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 1000,
                timeout: None,
            },
            Some(&reservation),
        );
        let mut leases = vec![parent.worker_reservation()];
        parent.fork(1, leases[0].as_ref()).retain(400).unwrap();
        parent.deadline = Some(Instant::now() - Duration::from_secs(1));
        assert!(matches!(
            parent.absorb_worker_reservations(&mut leases, [400]),
            Err(ExecError::TimedOut)
        ));
        assert_eq!(pool.usage().reserved_bytes, 400);
        assert!(leases[0].is_some());
        drop(leases);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn racing_reservations_never_exceed_capacity_or_leak() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let barrier = Arc::new(std::sync::Barrier::new(17));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let pool = Arc::clone(&pool);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let reservation = Reservation::new(pool);
                    barrier.wait();
                    let result = reservation.grow_to(160);
                    barrier.wait();
                    barrier.wait();
                    if result.is_ok() {
                        assert_eq!(reservation.bytes.load(Ordering::Relaxed), 160);
                    }
                });
            }
            barrier.wait();
            barrier.wait();
            assert_eq!(pool.usage().reserved_bytes, 960);
            assert_eq!(pool.usage().peak_bytes, 960);
            assert_eq!(pool.usage().rejections, 10);
            barrier.wait();
        });
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
}
