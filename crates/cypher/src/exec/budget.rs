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

type ProcessRss = Box<
    dyn Fn() -> Option<usize> + Send + Sync + std::panic::RefUnwindSafe + std::panic::UnwindSafe,
>;

/// Shared accounting for transaction high-water reservations, not allocator RSS.
pub struct QueryMemoryPool {
    process_memory: std::sync::OnceLock<(usize, ProcessRss)>,
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
    /// Configured process guard limit; None without a telemetry provider.
    pub process_limit_bytes: Option<usize>,
    /// On-demand process sample; None if unavailable or no provider is installed.
    pub process_rss_bytes: Option<usize>,
}
impl QueryMemoryPool {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            process_memory: std::sync::OnceLock::new(),
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
    /// Install process telemetry once. RSS is sampled on demand; None means unavailable.
    /// This observation does not change admission or establish an RSS guarantee.
    pub fn set_process_memory(
        &self,
        limit: usize,
        rss: impl Fn() -> Option<usize>
            + Send
            + Sync
            + std::panic::RefUnwindSafe
            + std::panic::UnwindSafe
            + 'static,
    ) {
        let _ = self.process_memory.set((limit, Box::new(rss)));
    }
    /// Individual counters are atomic; this telemetry is not one atomic snapshot.
    pub fn usage(&self) -> QueryMemoryUsage {
        QueryMemoryUsage {
            limit_bytes: self.limit,
            reserved_bytes: self.reserved.load(Ordering::Relaxed),
            peak_bytes: self.peak.load(Ordering::Relaxed),
            rejections: self.rejections.load(Ordering::Relaxed),
            process_headroom_bytes: self.headroom.get().map(|headroom| headroom()),
            process_limit_bytes: self.process_memory.get().map(|(limit, _)| *limit),
            process_rss_bytes: self.process_memory.get().and_then(|(_, rss)| rss()),
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
    fn node_payload_bytes(node: &NodeOut) -> usize {
        node.label.len().saturating_add(map_bytes(&node.props))
    }
    fn relationship_payload_bytes(relationship: &RelationshipOut) -> usize {
        relationship
            .rel_type
            .len()
            .saturating_add(relationship.element_id.len())
            .saturating_add(map_bytes(&relationship.props))
    }
    fn out_bytes(out: &Out) -> usize {
        size_of::<Out>().saturating_add(match out {
            Out::Value(v) => value_bytes(v),
            Out::Node(n) => Self::node_payload_bytes(n),
            Out::Relationship(r) => Self::relationship_payload_bytes(r),
            Out::Path(p) => {
                p.nodes
                    .iter()
                    .map(|n| size_of::<Out>().saturating_add(Self::node_payload_bytes(n)))
                    .chain(p.rels.iter().map(|r| {
                        size_of::<Out>().saturating_add(Self::relationship_payload_bytes(r))
                    }))
                    .fold(0usize, usize::saturating_add)
            }
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
    use std::alloc::{GlobalAlloc, Layout, System};

    thread_local! {
        static ACCOUNTING_ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct AccountingAllocator;

    fn track_allocation(bytes: usize) {
        // Const TLS needs no allocation; teardown can make it unavailable.
        let _ = ACCOUNTING_ALLOCATED.try_with(|counter| {
            if let Some(total) = counter.get() {
                counter.set(Some(total.saturating_add(bytes)));
            }
        });
    }

    unsafe impl GlobalAlloc for AccountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            track_allocation(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            track_allocation(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            track_allocation(size);
            unsafe { System.realloc(ptr, layout, size) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ACCOUNTING_ALLOCATOR: AccountingAllocator = AccountingAllocator;

    #[test]
    fn scalar_inspection_does_not_copy_its_already_admitted_argument() {
        use brainwire_store::db::{Resources, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let txn = db.begin();
        let catalog = Catalog {
            version: 1,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let vars = plan::Vars::default();
        for (name, argument, expected) in [
            (
                "size",
                Value::Str("x".repeat(1 << 20)),
                val::Val::Int(1 << 20),
            ),
            (
                "size",
                Value::List(vec![Value::Str("x".repeat(1 << 20))]),
                val::Val::Int(1),
            ),
            (
                "isempty",
                Value::List(vec![Value::Str("x".repeat(1 << 20))]),
                val::Val::Bool(false),
            ),
            (
                "isempty",
                Value::Map(BTreeMap::from([(
                    "body".into(),
                    Value::Str("x".repeat(1 << 20)),
                )])),
                val::Val::Bool(false),
            ),
            (
                "exists",
                Value::Map(BTreeMap::from([(
                    "body".into(),
                    Value::Str("x".repeat(1 << 20)),
                )])),
                val::Val::Bool(true),
            ),
        ] {
            let param_vals = BTreeMap::from([("argument".into(), val::Val::from(&argument))]);
            let params = BTreeMap::from([("argument".into(), argument)]);
            let budget = Budget::new(
                ExecutionLimits {
                    memory_bytes: (1 << 20) + (64 << 10),
                    timeout: None,
                },
                None,
            );
            let ctx = Context {
                txn: &txn,
                catalog: &catalog,
                params: &params,
                budget: &budget,
                visibility: None,
                checkpoint_dir: None,
                vars: &vars,
                cache: None,
                param_vals: Some(&param_vals),
                partition: None,
            };
            let expr = Expr::Call {
                name: name.into(),
                distinct: false,
                args: vec![Expr::Param("argument".into())],
            };
            ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
            let result = eval::eval(&expr, &[], &ctx);
            let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
            assert_eq!(result.unwrap(), expected);
            assert!(
                allocated < (1 << 20) + (64 << 10),
                "{name} copied a large argument beyond its admitted allocation: {allocated}"
            );
        }
    }

    #[test]
    fn map_projection_does_not_clone_unselected_payloads() {
        use brainwire_store::db::{Resources, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let txn = db.begin();
        let catalog = Catalog {
            version: 1,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let params = BTreeMap::new();
        let vars = plan::Vars::default();
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 64 << 10,
                timeout: None,
            },
            None,
        );
        let ctx = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &vars,
            cache: None,
            param_vals: None,
            partition: None,
        };
        let plan = plan::plan(&parse("WITH {} AS doc RETURN doc{.title}").unwrap(), &ctx).unwrap();
        let ctx = Context {
            vars: &plan.vars,
            ..ctx
        };
        let row = vec![val::Val::Map(BTreeMap::from([
            ("title".into(), val::Val::Str("small".into())),
            ("body".into(), val::Val::Str("x".repeat(1 << 20))),
        ]))];
        let expr = Expr::MapProjection {
            var: "doc".into(),
            items: vec![MapItem::Prop("title".into())],
        };
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let value = eval::eval(&expr, &row, &ctx).unwrap();
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        assert_eq!(
            value,
            val::Val::Map(BTreeMap::from([(
                "title".into(),
                val::Val::Str("small".into())
            )]))
        );
        assert!(
            allocated < 64 << 10,
            "small projection allocated {allocated} bytes from an unrelated 1 MiB property"
        );
        let expr = Expr::Index(
            Box::new(Expr::Var("doc".into())),
            Box::new(Expr::Literal(Value::Str("title".into()))),
        );
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let result = eval::eval(&expr, &row, &ctx);
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        assert_eq!(result.unwrap(), val::Val::Str("small".into()));
        assert!(
            allocated < 64 << 10,
            "dynamic map access copied {allocated} bytes"
        );
        let list_row = vec![val::Val::List(vec![
            val::Val::Str("small".into()),
            val::Val::Str("x".repeat(1 << 20)),
        ])];
        for index in [0, -2] {
            let expr = Expr::Index(
                Box::new(Expr::Var("doc".into())),
                Box::new(Expr::Literal(Value::Int(index))),
            );
            ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
            let result = eval::eval(&expr, &list_row, &ctx);
            let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
            assert_eq!(result.unwrap(), val::Val::Str("small".into()));
            assert!(
                allocated < 64 << 10,
                "dynamic list access copied {allocated} bytes"
            );
        }
        for index in [Value::Int(2), Value::Int(-3), Value::Null] {
            let expr = Expr::Index(
                Box::new(Expr::Var("doc".into())),
                Box::new(Expr::Literal(index)),
            );
            assert_eq!(eval::eval(&expr, &list_row, &ctx).unwrap(), val::Val::Null);
        }
        let invalid = Expr::Index(
            Box::new(Expr::Var("doc".into())),
            Box::new(Expr::Literal(Value::Bool(true))),
        );
        assert!(matches!(
            eval::eval(&invalid, &list_row, &ctx),
            Err(ExecError::Semantic(_))
        ));
        let large = Expr::Index(
            Box::new(Expr::Var("doc".into())),
            Box::new(Expr::Literal(Value::Int(-1))),
        );
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let denied = eval::eval(&large, &list_row, &ctx);
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        assert!(matches!(denied, Err(ExecError::MemoryLimit { .. })));
        assert!(
            allocated < 64 << 10,
            "denied list item allocated {allocated} bytes"
        );
        for expr in [
            Expr::Prop(Box::new(Expr::Var("doc".into())), "body".into()),
            Expr::MapProjection {
                var: "doc".into(),
                items: vec![MapItem::Prop("body".into())],
            },
        ] {
            ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
            let result = eval::eval(&expr, &row, &ctx);
            let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
            assert!(matches!(result, Err(ExecError::MemoryLimit { .. })));
            assert!(
                allocated < 64 << 10,
                "denied property allocated {allocated} bytes before admission"
            );
        }
        let large_budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 8 << 20,
                timeout: None,
            },
            None,
        );
        let ctx = Context {
            budget: &large_budget,
            ..ctx
        };
        let expr = Expr::Slice(
            Box::new(Expr::Literal(Value::List(vec![Value::Str(
                "x".repeat(1 << 20),
            )]))),
            Some(Box::new(Expr::Literal(Value::Int(0)))),
            None,
        );
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let result = eval::eval(&expr, &[], &ctx);
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        let val::Val::List(items) = result.unwrap() else {
            panic!("expected a sliced list")
        };
        assert_eq!(items.len(), 1);
        let val::Val::Str(payload) = &items[0] else {
            panic!("expected string payload")
        };
        assert_eq!(payload.len(), 1 << 20);
        assert!(
            allocated < (1 << 20) + (64 << 10),
            "owned slice recopied selected payload: {allocated}"
        );
        let narrow = Expr::Slice(
            Box::new(Expr::Literal(Value::List(vec![Value::Null; 10000]))),
            Some(Box::new(Expr::Literal(Value::Int(0)))),
            Some(Box::new(Expr::Literal(Value::Int(1)))),
        );
        let val::Val::List(items) = eval::eval(&narrow, &[], &ctx).unwrap() else {
            panic!("expected list")
        };
        assert_eq!(items.len(), 1);
        assert!(
            items.capacity() <= 2,
            "narrow slice retained source capacity {}",
            items.capacity()
        );
        for (from, to, expected) in [
            (Some(-2), Some(-1), vec![2]),
            (Some(3), Some(1), vec![]),
            (None, None, vec![1, 2, 3]),
            (Some(-99), Some(99), vec![1, 2, 3]),
        ] {
            let bound = |value: Option<i64>| value.map(|n| Box::new(Expr::Literal(Value::Int(n))));
            let expr = Expr::Slice(
                Box::new(Expr::Literal(Value::List(vec![
                    Value::Int(1),
                    Value::Int(2),
                    Value::Int(3),
                ]))),
                bound(from),
                bound(to),
            );
            assert_eq!(
                eval::eval(&expr, &[], &ctx).unwrap(),
                val::Val::List(expected.into_iter().map(val::Val::Int).collect())
            );
        }
        for bound in [Value::Null, Value::Bool(true)] {
            let expr = Expr::Slice(
                Box::new(Expr::Literal(Value::List(vec![Value::Int(1)]))),
                Some(Box::new(Expr::Literal(bound.clone()))),
                None,
            );
            let result = eval::eval(&expr, &[], &ctx);
            if bound == Value::Null {
                assert_eq!(result.unwrap(), val::Val::Null);
            } else {
                assert!(matches!(result, Err(ExecError::Semantic(_))));
            }
        }
    }

    #[test]
    fn map_projection_admits_selected_fields_cumulatively() {
        use brainwire_store::db::{Resources, StoreConfig};
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path(), &Resources::new(StoreConfig::for_tests())).unwrap();
        let txn = db.begin();
        let catalog = Catalog {
            version: 1,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let params = BTreeMap::new();
        let vars = plan::Vars::default();
        let budget = Budget::new(
            ExecutionLimits {
                memory_bytes: 64 << 10,
                timeout: None,
            },
            None,
        );
        let ctx = Context {
            txn: &txn,
            catalog: &catalog,
            params: &params,
            budget: &budget,
            visibility: None,
            checkpoint_dir: None,
            vars: &vars,
            cache: None,
            param_vals: None,
            partition: None,
        };
        let plan = plan::plan(&parse("WITH {} AS doc RETURN doc{.a,.b}").unwrap(), &ctx).unwrap();
        let ctx = Context {
            vars: &plan.vars,
            ..ctx
        };
        let row = vec![val::Val::Map(BTreeMap::from([
            ("a".into(), val::Val::Str("x".repeat(40 << 10))),
            ("b".into(), val::Val::Str("y".repeat(40 << 10))),
        ]))];
        let expr = Expr::MapProjection {
            var: "doc".into(),
            items: vec![MapItem::Prop("a".into()), MapItem::Prop("b".into())],
        };
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let result = eval::eval(&expr, &row, &ctx);
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        assert!(matches!(result, Err(ExecError::MemoryLimit { .. })));
        assert!(
            allocated < 64 << 10,
            "projection copied more selected fields than admitted: {allocated}"
        );
    }

    #[test]
    fn path_output_accounting_borrows_properties_without_allocating() {
        let node = NodeOut {
            id: NodeId(1),
            label: "Document".into(),
            props: BTreeMap::from([("body".into(), Value::Str("n".repeat(64 << 10)))]),
        };
        let rel = RelationshipOut {
            id: 2,
            element_id: "relationship-2".into(),
            start: NodeId(1),
            end: NodeId(1),
            rel_type: "REFERENCES".into(),
            props: BTreeMap::from([("body".into(), Value::Str("r".repeat(64 << 10)))]),
        };
        let expected = size_of::<Vec<Out>>()
            + size_of::<Out>()
            + Budget::out_bytes(&Out::Node(node.clone()))
            + Budget::out_bytes(&Out::Relationship(rel.clone()));
        let row = [Out::Path(PathOut {
            nodes: vec![node],
            rels: vec![rel],
        })];
        ACCOUNTING_ALLOCATED.with(|counter| counter.set(Some(0)));
        let estimated = Budget::output_bytes(&row);
        let allocated = ACCOUNTING_ALLOCATED.with(|counter| counter.replace(None).unwrap());
        assert_eq!(
            estimated, expected,
            "path accounting must preserve its byte estimate"
        );
        assert_eq!(
            allocated, 0,
            "estimating path output must not clone its properties"
        );
    }

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
