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
pub(super) struct Reservation {
    pool: Arc<QueryMemoryPool>,
    bytes: Cell<usize>,
}
impl Reservation {
    pub fn new(pool: Arc<QueryMemoryPool>) -> Self {
        Self {
            pool,
            bytes: Cell::new(0),
        }
    }
    fn grow_to(&self, bytes: usize) -> Result<(), ExecError> {
        let extra = bytes.saturating_sub(self.bytes.get());
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
                self.bytes.set(bytes);
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
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.pool
            .reserved
            .fetch_sub(self.bytes.get(), Ordering::Relaxed);
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
        Ok(())
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
                        assert_eq!(reservation.bytes.get(), 160);
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
