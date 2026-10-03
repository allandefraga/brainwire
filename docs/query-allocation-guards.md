# Conservative query allocation guards

`memory.query` / `BRAINWIRE_MEMORY_QUERY` now controls a conservative **per-RUN**
allocation guard in the real Bolt worker and embedded CLI. It must be positive.
A new RUN resets accounting and charges the existing transaction write overlay,
parameters and query text before execution. Charges cover selected candidates,
bindings, values, result rows, grouping, write-overlay growth and a bounded CSR
projection estimate. The store visits label/index/adjacency keys incrementally
rather than copying all record payloads before the check.

A limit failure discards the entire query transaction, including writes staged
by earlier RUNs. The CLI returns a resource error; Bolt reports the Brainwire
extension `Neo.ClientError.Statement.MemoryLimitExceeded`. A fresh transaction
can run normally. Library callers choose `execute_with_limits` or
`QueryTransaction::begin_with_limits`; the older entry points remain unlimited.

```sh
BRAINWIRE_MEMORY_QUERY=16MiB brainwire cypher --database ./data/research \
  --query 'MATCH (c:Customer) RETURN c.entity_id LIMIT 10'
```

This is not an exact memory ceiling. Charges are monotonic within a RUN rather
than refunded when a temporary is freed, so some low-peak queries can be
rejected. Query parsing, Bolt decoding/encoding, allocator overhead, one record's
storage decoding, an individual store operation and RocksDB commit internals
are not fully reserved before allocation. One operation can overshoot before
the next check. Disk reads and candidates rejected by residual filters also
consume accounting. `LIMIT` bounds returned rows, not upstream MATCH/UNWIND or
CSR work. Cardinality caps still apply to analytics.

There is no shared pool across concurrent queries; 64 connections do not share
one `memory.query` allowance. Container memory limits can still terminate the
process. Process RSS includes RocksDB/cache, transport and other allocations.
The TUI labels this distinction; offline cache counters describe its own
inspection process, not the live server. Admin JSON keeps
`query_budget_enforced:false` for a hard budget and separately reports
`query_allocation_guard_enforced:true` with `conservative_per_run` scope.

Next: streaming operators, live reservations and a shared pool, followed by
large-volume RSS and concurrent-load measurements. The current guard improves
failure behavior without establishing a production hard-memory guarantee.
