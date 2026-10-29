# Bounded optimistic conflict-history retention

Status: candidate implementation, scoped to the transaction committer.

## Problem

Each database retains committed keys in an ordered conflict map and a commit
history. The existing key-count limit cannot bound variable-length keys; a
long-lived transaction can pin many large key copies independently of query
reservations. The hard count limit currently permits eight times the soft cap
while writers remain active.

## Alternatives

1. Lower the existing key cap. Simple, but still leaves key lengths unbounded.
2. Introduce a new public configuration field and thread it through every caller.
   This offers tuning but changes the Rust API and server configuration before
   a workload has established a useful tuning range.
3. Add a conservative fixed retained-size ceiling, preserving the public API and
   existing count policy. This bounds one otherwise separate memory component.

## Decision

Apply a 64 MiB retained-size budget per database. Charge both key copies,
inline key/vector/map fields, a conservative per-key tree allowance and the
reported backing capacity of the commit-history deque. The estimate excludes
allocator slack and is not a process RSS cap, a shared cross-graph quota or a
bound on the current transaction overlay and RocksDB write batch.

After the RocksDB write succeeds, make room by evicting whole commits in
sequence order before creating history key clones. Advance `pruned_through`
monotonically for each eviction. If one commit cannot fit, clear conflict
history and advance the watermark through that commit without rejecting an
already durable write. All active writers older than the watermark fail closed;
new writers can progress from the current sequence. Empty history releases its
deque backing. Failed durable writes do not mutate retention state.

## Verification and tradeoff

Tests use small private limits to cover oversized keys, zero budget, exact
accounting, repeated overwrites, prefix phantom dependencies, old snapshot
readers promoted to writers, fresh-writer progress, release and reopen. These
are correctness and retained-size accounting tests. They do not qualify global
memory or production load. The byte ceiling can increase retry conflicts for
long-lived writers; measure those conflicts and throughput before claiming a
performance improvement or certifying the 5% regression gate.
