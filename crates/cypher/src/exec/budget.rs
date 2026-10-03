//! Conservative per-RUN allocation accounting, not a process RSS limit.
//! ponytail: charges are not refunded within a RUN; replace with live allocation
//! reservations when streaming operators and a shared instance pool exist.
use super::*;
use std::cell::Cell;
use std::mem::size_of;

#[derive(Debug, Clone, Copy)]
pub struct ExecutionLimits {
    pub memory_bytes: usize,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            memory_bytes: usize::MAX,
        }
    }
}

pub(super) struct Budget {
    limit: usize,
    used: Cell<usize>,
}
impl Budget {
    pub fn new(limits: ExecutionLimits) -> Self {
        Self {
            limit: limits.memory_bytes,
            used: Cell::new(0),
        }
    }
    pub fn charge(&self, bytes: usize) -> Result<(), ExecError> {
        let attempted = self.used.get().saturating_add(bytes);
        if attempted > self.limit {
            return Err(ExecError::MemoryLimit {
                limit: self.limit,
                attempted,
            });
        }
        self.used.set(attempted);
        Ok(())
    }
    pub fn remaining(&self) -> usize {
        self.limit.saturating_sub(self.used.get())
    }
    pub fn bindings(&self, row: &Bindings) -> Result<(), ExecError> {
        self.charge(
            size_of::<Bindings>()
                + size_of::<Vec<Bindings>>()
                + row.iter().fold(0usize, |sum, (key, binding)| {
                    sum.saturating_add(128)
                        .saturating_add(key.len())
                        .saturating_add(match binding {
                            Binding::Node(_) => size_of::<NodeId>(),
                            Binding::Value(v) => value_bytes(v),
                        })
                }),
        )
    }
    pub fn output(&self, row: &[Out]) -> Result<(), ExecError> {
        self.charge(
            size_of::<Vec<Out>>()
                + row.iter().fold(0usize, |sum, out| {
                    sum.saturating_add(match out {
                        Out::Value(v) => size_of::<Out>().saturating_add(value_bytes(v)),
                        Out::Node(n) => size_of::<Out>()
                            .saturating_add(n.label.len())
                            .saturating_add(map_bytes(&n.props)),
                    })
                }),
        )
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
