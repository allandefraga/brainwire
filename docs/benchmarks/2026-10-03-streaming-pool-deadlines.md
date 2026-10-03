# Streaming reads, shared reservations and deadlines

October 3, 2026, second requested one-hour local session. Engine source commit
`410e9ae`; synthetic data only. No push, publication or remote deployment.
The [raw results](2026-10-03-session2-results/) preserve individual observations.

## Delivered and checked

- Row-at-a-time read-only MATCH/UNWIND, ordinary RETURN and ungrouped counts.
  LIMIT stops upstream iteration; validation still runs before LIMIT 0.
- A process-wide server pool shared across connections and named graphs.
  Conservative transaction high-water reservations survive multiple RUNs and
  remain held until commit, rollback, failure or drop. Contention is retryable.
- `CALL db.memory()` exposes current/peak/limit/rejection counters. The query
  itself needs a reservation and can be rejected when the pool is exhausted.
- A cooperative per-RUN execution deadline, configured through
  `server.query_timeout_ms` (default 30,000) or its environment override.
  Expiry discards prior staged writes and releases reservations.
- Scalar property evaluation moves the requested value instead of constructing
  and cloning every property. Tests preserve extra/column/entity_id semantics
  through ontology promotion.
- Bolt parameter/output conversion errors now discard earlier staged writes at
  the worker boundary; the regression test failed before the fix.
- Memory/Pipeline TUI explanations in EN/PT-BR, admin JSON and config provenance.
  [13 terminal captures](../design/assets/2026-10-03-session2/README.md) cover
  100K nodes, both languages, wide/narrow panels and Disk F1 inspection.

All **383 Rust tests passed**, as did workspace clippy with warnings denied and
formatting. Engine regressions were implemented test-first. A differential
regression compares streaming reads against the eager fallback on correlated
patterns, repeated bindings, empty inputs and null counts. No new dependency.

## 100K-node bounded reads

Native macOS arm64 debug executor, Apple M5 with 32 GiB RAM, Rust 1.98.1,
warm local RocksDB, cache 64 MiB, memtables 4 MiB,
**64 KiB query guard/pool**. One warmup and five measured samples per query.
The machine also ran other workloads; these are observations, not capacity or
production latency guarantees. Import took 10.03 seconds in this run.

| Query | p50 ms | Peak accounted reservation, bytes |
| --- | ---: | ---: |
| Label read, LIMIT 1 | 0.145 | 5,202 |
| Indexed equality, LIMIT 1 | 0.142 | 5,902 |
| Full label count, 100K matches | 1,673.978 | 4,389 |
| Indexed count, 14,286 matches | 411.077 | 5,093 |
| Unindexed predicate matches last node, LIMIT 1 | 3,120.321 | 5,830 |
| Two 100K-node inputs, Cartesian LIMIT 1 | 0.186 | 6,622 |
| Three 100-value UNWINDs, LIMIT 1 | 0.083 | 34,523 |

The Cartesian input has 10 billion potential pairs; only the first required
result was evaluated. This is **not** a 10-billion-record throughput test.
Unlimited output correctly hit the guard and released its reservation.
`/usr/bin/time -l` recorded 33,308,672 bytes maximum resident size for the whole
benchmark process (import, reopen and queries), about 31.77 MiB. This is distinct
from estimated per-query reservations and is not a server memory measurement.
There is no consistent measured count-latency speedup from the scalar-property
change; the established improvement is bounded intermediates and early LIMIT.

## One-million-node extension

The same executable was rebuilt from the final engine source and run with
**1,000,000 synthetic nodes**, one index and no edges. All assertions passed,
including the rejected unbounded result and complete pool release. The query
budget stayed **64 KiB**; import took **39.54 s**. No execution deadline was
configured in this embedded benchmark.

| Query | p50 ms | Peak accounted reservation, bytes |
| --- | ---: | ---: |
| Label read, LIMIT 1 | 0.051 | 5,202 |
| Indexed equality, LIMIT 1 | 0.073 | 5,902 |
| Full label count, 1M matches | 12,943.187 | 4,390 |
| Indexed count, 142,858 matches | 3,417.697 | 5,095 |
| Unindexed match at the final node, LIMIT 1 | 25,714.866 | 5,835 |
| Two 1M-node inputs, Cartesian LIMIT 1 | 0.060 | 6,622 |

A trillion potential pairs were not evaluated: LIMIT stopped after one result.
Whole-process maximum resident size was **68,124,672 bytes (64.97 MiB)** across
import/reopen/queries. Full run: 296.64 seconds. The small-query timings are
lower than the earlier 100K run because these were separate shared-host samples;
they do not show that larger graphs are inherently faster. The meaningful result
is nearly unchanged intermediate accounting at ten times the node count, while
full scans continue to take proportionally substantial time. This does not
measure edge-heavy traversal, production release performance or billion-scale
capacity.

## Real Python/Bolt

Final engine code, driver **5.28.6**, 100K nodes, batches 250, query budget 256 MiB:

- Insert: **15.66 s**; idempotent replay: **17.63 s**.
- Primary point read: p50 **1.913 ms**, p95 **3.125 ms**, 25 samples.
- Text-index point read: p50 **2.318 ms**, p95 **4.130 ms**, 25 samples.
- Numeric-index group count: p50 **515.137 ms**, p95 **562.836 ms**, 25 samples.
- Two forced process exits/reopens; 99,999 recovered nodes because the workload
  deliberately deletes one. Current/stale indexes, lists, numeric precision,
  rollback and replay checks passed. Driver **6.3.0** passed the same acceptance
  at 10K nodes. This is process recovery on a retained local directory.

Both Python drivers passed 16 simultaneous connections across two graphs with a
256 KiB shared pool. Some transactions remained held and others were rejected
with the retryable pool error. Peak accounting stayed within 262,144 bytes;
cleanup left only the observer query's 312-byte reservation. A 200^4 low-memory
fanout hit a 1-second cooperative deadline and rolled back its earlier CREATE.

Each driver also completed 400 reads plus 400 indexed overwrites on eight
connections over a 2K-node fixture, without retrying errors. Writers used
disjoint identities, so this does not measure contention on one hot key.
Final current/stale index checks passed. Driver 5 observed 783 operations/s
(read/write p95 21.53/22.30 ms); driver 6 observed 1,907 operations/s
(p95 10.93/11.26 ms). These short runs are correctness/latency samples on a
shared machine, **not a comparative driver benchmark or sustainable throughput**.

A further four-writer collision on one existing node passed with both drivers:
one commit succeeded, three returned retryable commit conflicts, and only the
winner's new index entry remained. Conflicting overwrites left neither duplicate
nodes nor stale index entries. This is checked by the same `query_memory.py`
script (test commits `733b38c` and `415e5e8`), with separate raw results.
The final script also closes the server and runs offline `db check` on both
graphs; both drivers passed physical graph/index audits after the workload.

A separately spawned Python client was killed while holding an explicit
transaction with a staged indexed CREATE and a consumed 16K-string result.
Both drivers passed: observed reservations fell from 65,671 bytes to the
observer's 312 bytes, the pending node disappeared, and both offline graph/index
audits passed. The server stayed running. Test commit `d4edec6` covers actual
socket loss as well as the earlier explicit rollback paths.

## Container and Kubernetes status

Image `brainwire:20261003-session2-release` built successfully for Linux arm64,
revision label `ba683ad`, image ID
`sha256:94e734987038bcf20e964eea25fac00ca414646bad0488d0c8ac72d35bc0700e`.
Docker retained-volume recovery passed with 10K bulk Customers: verification
cycles 0..6 cover the initial interrupted transaction plus five requested cycles,
current/stale index lookups and the final offline audit. Compose service
recreation and its offline audit also passed. Owned resources were removed.
Kubernetes **1.36.4** acceptance passed on an isolated kind node with **10K nodes**:
container stop with zero timeout and Pod replacement both retained the same PVC,
acknowledged data and current numeric/string indexes; pending overwrites vanished.
The original store and native checkpoint passed offline audits, and the checkpoint
copy was queried over Bolt. The cluster and its private kubeconfig were removed.
This proves the tested process/Pod recovery path on retained local storage, not
machine loss, replication or HA. An earlier cluster became ready but was removed
after timing out waiting for the image; the successful retry used the ready image.
The Docker builder now pins its installed Rust 1.98.1 toolchain even though local
development follows stable, and defaults to two compiler jobs to reduce RocksDB
C++ memory pressure. Kubernetes command timeouts preserve diagnostic output;
`ab576f1` makes a missing-image preflight fail before creating a cluster, verified
with a nonexistent image and an unavailable kind path.
A queued build first exposed stale workspace artifacts in the shared target
cache. `ba683ad` runs `cargo clean --release --workspace` before rebuilding,
preserving the expensive dependency/RocksDB cache while preventing old cross-crate
APIs from appearing fresh. The repaired image exposes the new timeout setting.
The full query-memory Python acceptance also passed against this **Linux release
image** using a temporary Docker CLI wrapper: shared-pool contention, deadline
rollback, mixed traffic, hot-key conflicts, killed-client cleanup and both offline
audits. The wrapper mounts only each synthetic temporary directory, maps the
chosen localhost port and explicitly overrides the image's data-directory env
setting. The supplemental bind-mount test uses the host's numeric UID/GID;
Docker/Compose/Kubernetes recovery separately tested the normal image user.
Its saved result identifies the Linux release environment; the harness's original
native/debug scope labels were corrected in this derived artifact. The wrapper is
saved beside the result for reproduction.
No replica/HA claim follows from these changes.

## Run again

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo fmt --all -- --check
cargo run --locked --offline -p brainwire-cypher --example streaming_reads -- 100000
python crates/server/tests/query_memory.py --binary target/debug/brainwire
python crates/server/tests/python_ingestion.py --binary target/debug/brainwire --rows 100000 --batch-size 250 --query-mib 256
```

Use a Python environment with the Neo4j driver installed. Scripts allocate only
temporary synthetic stores and stop their servers. Run builds before timing;
these are debug measurements. Server telemetry:

```cypher
CALL db.memory()
YIELD poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections
RETURN poolLimitBytes, poolReservedBytes, poolPeakBytes, poolRejections
```

## Boundaries and next work

The pool is conservative accounting, not an allocator or RSS cap. Individual
decodes, parsing, transport and serialization can allocate before accounting.
The deadline starts after parsing/catalog setup and does not preempt native
calls, commit or idle transactions. Writes, grouped aggregates and CALL remain
eager; final Bolt results remain buffered. The TUI is still offline/read-only.
Next: bounded Bolt cursors and cancellation, live allocation leases, then
streaming grouped/write operators. Replication, machine-loss recovery, auth/TLS,
vector ANN search and DuckLake/Arrow incremental ingestion remain separate work.
