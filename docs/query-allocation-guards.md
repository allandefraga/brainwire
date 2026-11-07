# Query memory reservations and cooperative deadlines

`memory.query` / `BRAINWIRE_MEMORY_QUERY` bounds conservative per-RUN accounting
and the server's shared query reservation pool. All connections and named graphs
in one server share that pool. Embedded transactions use a private pool with a
transaction high-water reservation; `QueryTransaction::begin_with_pool` opts
into a caller-supplied shared pool. Private-pool telemetry is not exposed as
server-wide usage.
These counters estimate execution allocations, not process RSS or reserved RAM.

Read-only MATCH/UNWIND pipelines process one row at a time for ordinary RETURN
and ungrouped `count` aggregates. LIMIT stops upstream scans, index iterators,
hops and UNWIND once enough output rows exist. A count still visits every matching
row. Static validation happens before traversal, including when LIMIT is zero.
Writes, grouped aggregates, CALL and plans deeper than 64 stages retain eager
execution. Bolt still materializes final results before PULL; a large result
can exceed the guard even when its input is processed incrementally.

Temporary read-pipeline charges are released on leaving a row scope. Output rows
and eager collections retain accounting for the RUN. Source text, parameters and
an existing transaction write overlay form its baseline. The shared pool holds
each transaction's high-water reservation until commit, rollback, failure or
drop. This includes explicit transactions waiting idle and results waiting for
a client to PULL/DISCARD. Repeated RUNs reuse a reservation instead of adding a
new independent quota. No transaction can reserve above its per-RUN limit, and
the sum of pool reservations cannot exceed `memory.query`.

Every memory refusal uses `Neo.TransientError.General.MemoryPressure` with a
`[brainwire.memory reason=... requested_bytes=... available_bytes=...
rolled_back=true]` hint: `statement_limit` for the per-RUN guard, `query_pool`
for pool contention, `process_limit` near `memory.limit`
([contract](design/memory-error-contract.md)). Python Bolt drivers 5 and 6
classify it as retryable. All discard the complete query transaction,
including writes and indexes from earlier RUNs. Conversion errors at the Bolt
boundary also release reservations and discard prior writes. Use the driver's
managed transaction retry policy where appropriate; an outcome-unknown network
failure still requires separate handling.

## Observe the running server

```cypher
CALL db.memory()
YIELD poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections,
      processLimitBytes, processRssBytes
RETURN poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections,
       processLimitBytes, processRssBytes
```

`processLimitBytes` is the configured `memory.limit` when the process guard is
installed. `processRssBytes` samples process resident memory when this procedure
runs; it returns `null` if sampling is unavailable. Both fields are `null` without
a process guard. On platforms other than Linux/macOS the sample is the available
allocator estimate, rather than OS-reported RSS. Neither field proves a hard RSS
ceiling; sampling and pool counters are separate observations. The procedure is
subject to normal query admission and can itself be refused under memory pressure.

These are process-wide pool counters, not per-graph figures. Each counter is
atomic, but the row is not one atomic snapshot. Peak and rejection counters last
for the pool's lifetime. The observer query has its own reservation and can be
rejected when the pool is full. An unpooled embedded invocation returns nulls.
The offline TUI explicitly reports that it cannot observe server pool usage.
Admin JSON retains `query_budget_enforced:false` for a hard memory ceiling and
separately exposes allocation-guard/pool enforcement and configured limits.

## Execution deadline

`server.query_timeout_ms` / `BRAINWIRE_SERVER_QUERY_TIMEOUT_MS` sets a cooperative
execution deadline for each RUN: default 30,000 ms, accepted range 1..3,600,000.
It applies to the server and embedded CLI. Library `ExecutionLimits::default()`
has no deadline; set its `timeout` to opt in. Each RUN starts a fresh deadline after parsing and catalog setup.

The executor checks its deadline at allocation/accounting boundaries and before
successful completion. Long low-memory fanouts therefore stop with the
Brainwire extension `Neo.ClientError.Statement.ExecutionTimedOut`, discarding
prior staged writes and freeing pool reservations. It does not time out an idle
transaction. Parsing, Bolt transport, a single storage operation, commit and
long analytics/native calls are not preempted and can exceed the deadline.
This is not hard cancellation or a total network request deadline.

## Remaining boundaries

Parser/transport allocations, allocator overhead, individual storage decoding,
RocksDB commit internals and all final-response serialization are not fully
reserved before allocation. One operation can overshoot before a check. Eager
operators remain conservative rather than refunding every temporary allocation.
Returned library values can outlive their query transaction. A container can
still terminate the process when actual memory exceeds its cgroup limit.

Next: bounded Bolt cursors, live allocation leases, streaming grouped/write
operators and cancellation inside long storage/analytics operations. The current
pool provides admission/backpressure and bounded read intermediates; it does
not establish a production hard-memory guarantee.
