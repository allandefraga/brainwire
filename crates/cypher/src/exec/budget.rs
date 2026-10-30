//! Conservative per-RUN allocation accounting, not a process RSS limit.
//! Scoped read-pipeline temporaries are released; eager operators retain charges.
//! ponytail: pool reservations keep each transaction's high-water estimate until
//! it ends; replace with allocator-aware leases when exact live accounting exists.
use super::*;
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

/// Shared accounting for transaction high-water reservations, not allocator RSS.
pub struct QueryMemoryPool {
    limit: usize,
    /// Private embedded budgets must not appear as server-wide pool telemetry.
    observable: bool,
    reserved: AtomicUsize,
    peak: AtomicUsize,
    rejections: AtomicUsize,
    /// Bytes the whole process may still grow by (the process memory ceiling).
    headroom: std::sync::OnceLock<
        Box<dyn Fn() -> usize + Send + Sync + std::panic::RefUnwindSafe + std::panic::UnwindSafe>,
    >,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryMemoryUsage {
    pub limit_bytes: usize,
    pub reserved_bytes: usize,
    pub peak_bytes: usize,
    pub rejections: usize,
    /// Bytes the process may still grow before its memory ceiling; None without one.
    pub process_headroom_bytes: Option<usize>,
}
impl QueryMemoryPool {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            limit: limit_bytes,
            observable: true,
            reserved: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            rejections: AtomicUsize::new(0),
            headroom: std::sync::OnceLock::new(),
        }
    }
    pub(super) fn new_private(limit_bytes: usize) -> Self {
        Self {
            observable: false,
            ..Self::new(limit_bytes)
        }
    }
    /// Conservatively bounds all live reservations by sampled process headroom.
    /// This may count reservations already reflected in RSS twice and refuse early.
    /// Set once, at startup; it does not establish a hard process RSS ceiling.
    pub fn set_process_headroom(
        &self,
        headroom: impl Fn() -> usize
            + Send
            + Sync
            + std::panic::RefUnwindSafe
            + std::panic::UnwindSafe
            + 'static,
    ) {
        let _ = self.headroom.set(Box::new(headroom));
    }
    /// Individual counters are atomic; this telemetry is not one atomic snapshot.
    pub fn usage(&self) -> QueryMemoryUsage {
        QueryMemoryUsage {
            limit_bytes: self.limit,
            reserved_bytes: self.reserved.load(Ordering::Relaxed),
            peak_bytes: self.peak.load(Ordering::Relaxed),
            rejections: self.rejections.load(Ordering::Relaxed),
            process_headroom_bytes: self.headroom.get().map(|headroom| headroom()),
        }
    }
}
#[derive(Clone)]
pub(super) struct Reservation(Arc<ReservationInner>);

pub(super) struct ReservationInner {
    pool: Arc<QueryMemoryPool>,
    bytes: AtomicUsize,
    output: Arc<AtomicUsize>,
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
            output: Arc::new(AtomicUsize::new(0)),
        }))
    }
    pub(super) fn output_bytes(&self) -> usize {
        self.output.load(Ordering::Acquire)
    }
    pub(super) fn reserve_output(
        &self,
        bytes: usize,
        limit: usize,
    ) -> Result<OutputReservation, ExecError> {
        let base = self.bytes.load(Ordering::Acquire);
        self.output
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(bytes)
                    .filter(|total| base.saturating_add(*total) <= limit)
            })
            .map_err(|live| ExecError::MemoryLimit {
                limit,
                attempted: base.saturating_add(live).saturating_add(bytes),
            })?;
        let reservation = Reservation::new(Arc::clone(&self.pool));
        if let Err(error) = reservation.grow_to(bytes) {
            self.output.fetch_sub(bytes, Ordering::AcqRel);
            return Err(error);
        }
        Ok(OutputReservation {
            _reservation: reservation,
            output: Arc::clone(&self.output),
            bytes,
        })
    }
    // Each reservation has one charging executor; workers own separate leases.
    fn grow_to(&self, bytes: usize) -> Result<(), ExecError> {
        let extra = bytes.saturating_sub(self.bytes.load(Ordering::Relaxed));
        if extra == 0 {
            return Ok(());
        }
        // Compare the shared total atomically, so concurrent/small increments cannot
        // each spend the same sampled headroom. Already resident reservations count
        // twice deliberately: an epoch-aware allocator lease can relax this later.
        let process_headroom = self.pool.headroom.get().map(|headroom| headroom());
        let capacity = process_headroom
            .map(|available| available.min(self.pool.limit))
            .unwrap_or(self.pool.limit);
        let result =
            self.pool
                .reserved
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |reserved| {
                    reserved
                        .checked_add(extra)
                        .filter(|total| *total <= capacity)
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
                if let Some(headroom) = process_headroom {
                    let available = headroom.saturating_sub(reserved);
                    if extra > available {
                        return Err(ExecError::ProcessMemoryLimit {
                            requested: extra,
                            available,
                        });
                    }
                }
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
pub(super) struct OutputReservation {
    _reservation: Reservation,
    output: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for OutputReservation {
    fn drop(&mut self) {
        self.output.fetch_sub(self.bytes, Ordering::AcqRel);
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

enum RegexCache {
    Empty,
    Cached {
        pattern: String,
        regex: regex_automata::meta::Regex,
        credit: usize,
    },
    Disabled,
}

pub(super) struct Budget<'a> {
    limit: usize,
    used: Cell<usize>,
    retained: Cell<usize>,
    peak: Cell<usize>,
    reservation: Option<&'a Reservation>,
    deadline: Cell<Option<Instant>>,
    ticks: Cell<u32>,
    regex_cache: RefCell<RegexCache>,
    cancellation: RefCell<Option<Arc<AtomicBool>>>,
}
impl<'a> Budget<'a> {
    pub fn new(limits: ExecutionLimits, reservation: Option<&'a Reservation>) -> Self {
        Self {
            limit: limits.memory_bytes,
            used: Cell::new(0),
            retained: Cell::new(0),
            peak: Cell::new(0),
            reservation,
            deadline: Cell::new(
                limits
                    .timeout
                    .and_then(|timeout| Instant::now().checked_add(timeout)),
            ),
            ticks: Cell::new(0),
            regex_cache: RefCell::new(RegexCache::Empty),
            cancellation: RefCell::new(None),
        }
    }
    pub(super) fn set_cancellation(&self, cancellation: Arc<AtomicBool>) {
        *self.cancellation.borrow_mut() = Some(cancellation);
    }
    pub(super) fn output_lease(&self, bytes: usize) -> Result<QueryOutputLease, ExecError> {
        let bytes = bytes.saturating_mul(2).saturating_add(256);
        let reservation = self
            .reservation
            .map(|reservation| reservation.reserve_output(bytes, self.limit))
            .transpose()?;
        if bytes > self.limit {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted: bytes,
            });
        }
        Ok(QueryOutputLease {
            _reservation: reservation,
            page_bytes: (self.limit / 32).clamp(1, 4 << 20),
        })
    }
    pub fn cached_regex_match(&self, pattern: &str, text: &str) -> Option<bool> {
        let mut cache = self.regex_cache.borrow_mut();
        match &*cache {
            RegexCache::Cached {
                pattern: found,
                regex,
                ..
            } if found == pattern => return Some(regex.is_match(text.as_bytes())),
            RegexCache::Cached { .. } => {}
            _ => return None,
        }
        // Free the native cache before returning its exact retained credit.
        // Disable refill for this RUN to avoid pattern-dependent cache thrash.
        let old = std::mem::replace(&mut *cache, RegexCache::Disabled);
        drop(cache);
        if let RegexCache::Cached {
            pattern,
            regex,
            credit,
        } = old
        {
            drop(regex);
            drop(pattern);
            self.retained.set(
                self.retained
                    .get()
                    .checked_sub(credit)
                    .expect("regex cache owns its retained credit"),
            );
        }
        None
    }
    pub fn regex_cache_empty(&self) -> bool {
        matches!(*self.regex_cache.borrow(), RegexCache::Empty)
    }
    pub fn store_regex(&self, pattern: String, regex: regex_automata::meta::Regex, credit: usize) {
        debug_assert!(self.regex_cache_empty());
        *self.regex_cache.borrow_mut() = RegexCache::Cached {
            pattern,
            regex,
            credit,
        };
    }

    pub fn charge(&self, bytes: usize) -> Result<(), ExecError> {
        self.tick()?;
        let used = self.used.get().saturating_add(bytes);
        let execution = used.saturating_add(self.retained.get());
        let attempted =
            execution.saturating_add(self.reservation.map_or(0, Reservation::output_bytes));
        if attempted > self.limit {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted,
            });
        }
        if let Some(reservation) = self.reservation {
            reservation.grow_to(execution)?;
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
        let output = self.reservation.map_or(0, Reservation::output_bytes);
        let Some(attempted) = attempted.filter(|attempted| {
            attempted
                .checked_add(output)
                .is_some_and(|total| total <= self.limit)
        }) else {
            return Ok(false);
        };
        if let Some(reservation) = self.reservation {
            match reservation.grow_to(attempted) {
                Ok(()) => {}
                Err(
                    ExecError::MemoryPoolExhausted { .. } | ExecError::ProcessMemoryLimit { .. },
                ) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        self.retained.set(retained.expect("checked above"));
        self.peak.set(self.peak.get().max(attempted));
        Ok(true)
    }

    /// Deadline check for hot paths: reads the clock on every 32nd call.
    pub fn tick(&self) -> Result<(), ExecError> {
        if self
            .cancellation
            .borrow()
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            return Err(ExecError::TimedOut);
        }
        if self.deadline.get().is_none() {
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
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ExecError::TimedOut);
        }
        Ok(())
    }
    /// Moves the deadline by time the query spent waiting on its consumer (a lazy
    /// result blocked until the client PULLs), so only execution time counts.
    pub fn extend_deadline(&self, by: Duration) {
        if let Some(deadline) = self.deadline.get() {
            self.deadline
                .set(deadline.checked_add(by).or(Some(deadline)));
        }
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
            deadline: Cell::new(self.deadline.get()),
            ticks: Cell::new(0),
            regex_cache: RefCell::new(RegexCache::Empty),
            cancellation: RefCell::new(self.cancellation.borrow().clone()),
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
        let output = self.reservation.map_or(0, Reservation::output_bytes);
        let Some(attempted) = attempted.filter(|attempted| {
            attempted
                .checked_add(output)
                .is_some_and(|total| total <= self.limit)
        }) else {
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
        self.reservation
            .filter(|reservation| reservation.pool.observable)
            .map(|reservation| reservation.pool.usage())
    }
    pub fn remaining(&self) -> usize {
        self.limit
            .saturating_sub(self.used.get())
            .saturating_sub(self.retained.get())
            .saturating_sub(self.reservation.map_or(0, Reservation::output_bytes))
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
    pub(super) fn row_bytes(row: &[super::val::Val]) -> usize {
        row.iter()
            .fold(size_of::<Vec<super::val::Val>>() + 32, |sum, value| {
                sum.saturating_add(value.bytes())
            })
    }
    pub fn output(&self, row: &[Out]) -> Result<(), ExecError> {
        self.retain(Self::output_bytes(row))
    }
    pub(super) fn output_bytes(row: &[Out]) -> usize {
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
    fn regex_cache_credit_is_exact_and_worker_starts_empty() {
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 4096,
                timeout: None,
            },
            None,
        );
        parent.retain(77).unwrap();
        assert!(parent.try_retain_cache(128, 0).unwrap());
        let regex = regex_automata::meta::Regex::new(r"\Aa\z").unwrap();
        parent.store_regex("a".into(), regex, 128);
        assert_eq!(parent.cached_regex_match("a", "a"), Some(true));
        let worker = parent.fork(1, None);
        assert!(worker.regex_cache_empty());
        assert_eq!(worker.cached_regex_match("a", "a"), None);
        assert_eq!(parent.cached_regex_match("b", "a"), None);
        assert!(!parent.regex_cache_empty());
        assert_eq!(parent.remaining(), 4096 - 77);
        assert_eq!(parent.cached_regex_match("a", "a"), None);
    }

    #[test]
    fn optional_cache_admission_skips_pool_pressure_without_partial_charge() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let parent_reservation = Reservation::new(Arc::clone(&pool));
        let parent = Budget::new(
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
        parent
            .deadline
            .set(Some(Instant::now() - Duration::from_secs(1)));
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
        let parent = Budget::new(
            ExecutionLimits {
                memory_bytes: 1000,
                timeout: None,
            },
            Some(&reservation),
        );
        let mut leases = vec![parent.worker_reservation()];
        parent.fork(1, leases[0].as_ref()).retain(400).unwrap();
        parent
            .deadline
            .set(Some(Instant::now() - Duration::from_secs(1)));
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

    #[test]
    fn sampled_process_headroom_is_shared_by_all_live_reservations() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let headroom = Arc::new(AtomicUsize::new(100));
        let sampled = Arc::clone(&headroom);
        pool.set_process_headroom(move || sampled.load(Ordering::Relaxed));
        let first = Reservation::new(Arc::clone(&pool));
        let second = Reservation::new(Arc::clone(&pool));
        first.grow_to(60).unwrap();
        assert!(matches!(
            first.grow_to(120),
            Err(ExecError::ProcessMemoryLimit {
                requested: 60,
                available: 40
            })
        ));
        assert!(matches!(
            second.grow_to(60),
            Err(ExecError::ProcessMemoryLimit {
                requested: 60,
                available: 40
            })
        ));
        assert_eq!(pool.usage().reserved_bytes, 60);
        drop(first);
        second.grow_to(60).unwrap();
        headroom.store(160, Ordering::Relaxed);
        second.grow_to(120).unwrap();
        headroom.store(100, Ordering::Relaxed);
        assert!(matches!(
            second.grow_to(121),
            Err(ExecError::ProcessMemoryLimit {
                requested: 1,
                available: 0
            })
        ));
        drop(second);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn racing_reservations_cannot_reuse_the_same_process_headroom() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        pool.set_process_headroom(|| 100);
        let barrier = Arc::new(std::sync::Barrier::new(17));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let pool = Arc::clone(&pool);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let reservation = Reservation::new(pool);
                    barrier.wait();
                    let result = reservation.grow_to(60);
                    barrier.wait();
                    barrier.wait();
                    if result.is_ok() {
                        assert_eq!(reservation.bytes.load(Ordering::Relaxed), 60);
                    }
                });
            }
            barrier.wait();
            barrier.wait();
            assert_eq!(pool.usage().reserved_bytes, 60);
            assert_eq!(pool.usage().peak_bytes, 60);
            assert_eq!(pool.usage().rejections, 15);
            barrier.wait();
        });
        assert_eq!(pool.usage().reserved_bytes, 0);
    }
    #[test]
    fn retained_output_counts_against_execution_and_releases_on_drop() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let reservation = Reservation::new(Arc::clone(&pool));
        reservation.grow_to(20).unwrap();
        let output = reservation.reserve_output(60, 100).unwrap();
        assert_eq!(pool.usage().reserved_bytes, 80);
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 100,
                timeout: None,
            },
            Some(&reservation),
        );
        assert!(matches!(
            budget.charge(50),
            Err(ExecError::MemoryLimit { .. })
        ));
        assert!(reservation.reserve_output(21, 100).is_err());
        drop(output);
        budget.charge(50).unwrap();
        assert_eq!(pool.usage().reserved_bytes, 50);
        drop(budget);
        drop(reservation);
        assert_eq!(pool.usage().reserved_bytes, 0);
    }

    #[test]
    fn racing_output_leases_share_the_statement_and_pool_capacity() {
        let pool = Arc::new(QueryMemoryPool::new(1000));
        let reservation = Reservation::new(Arc::clone(&pool));
        reservation.grow_to(50).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(9));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let reservation = reservation.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let output = reservation.reserve_output(100, 500);
                    barrier.wait();
                    barrier.wait();
                    drop(output);
                });
            }
            barrier.wait();
            barrier.wait();
            assert_eq!(reservation.output_bytes(), 400);
            assert_eq!(pool.usage().reserved_bytes, 450);
            barrier.wait();
        });
        assert_eq!(reservation.output_bytes(), 0);
        assert_eq!(pool.usage().reserved_bytes, 50);
        // A global-pool refusal must return the statement credit immediately.
        let tiny = Arc::new(QueryMemoryPool::new(10));
        let other = Reservation::new(tiny);
        assert!(other.reserve_output(20, 100).is_err());
        assert_eq!(other.output_bytes(), 0);
    }
}
