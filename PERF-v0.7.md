# Brainwire v0.7 read and memory qualification

Status: **work in progress; not production-qualified**. The full acceptance workload
has not been reproduced in this branch. Values below are the supplied reference
measurements, not new results. Dashes mean unmeasured, not zero. No performance
or process-memory target is claimed as met.

| Workload | Supplied Brainwire reference | Target | Before (this branch) | After (this branch) |
| --- | ---: | ---: | ---: | ---: |
| 4M nodes + 4M edges, concurrent writes/reads, server peak RSS | OOM at 4 GiB; >5.9 GiB at 8 GiB | ≤ 4 GiB, no OOM | — | — |
| `count` with delegated `allow_all` | ~20 s; unrestricted ~0.08 s | ≤ 2× unrestricted | — | 0.0775 s (Bolt; unrestricted comparison pending) |
| Restricted-ACL `count` via client | 16.7 s | ≤ 0.5 s | — | 1.369 s (Bolt; target not met) |
| Top-N aggregation with ACL | 20.5 s | ≤ 1 s | — | 1.788 s (Bolt; target not met) |
| Bolt cursor, 1M nodes | 16 s | ≤ 5 s | — | — |
| ACL cursor, 1M nodes, pages of 200 | exceeded 30 min | ≤ 60 s | — | 11.30 s for 500k visible rows; 1M unmeasured |
| Unlabelled `MATCH (n)` with limit | 3.6 s | ≤ 1 s | — | — |
| Batch writes, eight sessions | 13.7–22k rows/s | ≥ 60k rows/s | — | — |

`python3 scripts/bench_v07.py --help` describes a preliminary embedded harness.
It creates an isolated synthetic dataset, records the executable hash, host
power/load, sampled RSS and `check` result, and never overwrites an existing
output directory. It does **not** measure the full concurrent Bolt workload,
ACL pagination or enforce a process RSS ceiling. Build the benchmark first;
do not build or test concurrently with measured runs. See
[scripts/bench_v07.md](scripts/bench_v07.md) for instructions and gaps.
The separate [Bolt diagnostic](scripts/bench_v07_bolt.md) can measure delegated
ACL reads and 200-row cursor pages against an explicitly identified, frozen
synthetic server. It does not run concurrent ingestion or measure server RSS.
Both harnesses passed their local fake-process/driver unit tests. The embedded diagnostics and an isolated Bolt ACL diagnostic produced receipts.
The full concurrent acceptance workload remains unmeasured.

The host reported AC power and low-power mode off during preparation. Its load
was elevated while compilation was underway; no benchmark was run under those
conditions. The existing `memory.query` pool is an estimate of selected query
allocations, not a cap on RocksDB, native buffers, process RSS, or cgroup memory.

## Embedded baseline: 27 October 2026

The synthetic four-label fixture loaded 4M nodes and 4M edges in eight sessions.
The separate load, read, write and check processes completed, and the invariant
check reported `status: ok`. The host was on AC with low-power mode off; load
averages at start were 7.81 / 13.24 / 18.21. This is a local diagnostic, not the
requested simultaneous 4 GiB server test. The executable SHA-256 was
`17cf5c65b50e36b9a3dcd850a2ed9ca93ec3f4608a37b51239f2466c146c2ae6`.

| Embedded baseline measurement | Result |
| --- | ---: |
| Count of 1M relationship pairs, median | 0.084 s |
| Top-brand aggregation, median | 0.356 s |
| Cursor walk, 1M rows, pages of 1,000 | 1.994 s |
| Write with prior read, eight sessions | 215,592 rows/s; zero retries |
| Highest sampled RSS across independent phases | 1,856,962,560 bytes (check process) |

The raw receipt and phase outputs are retained in the local disposable benchmark
output directory. The RSS value is sampled process RSS, not a hard limit or a
concurrent server measurement. These figures do not establish the ACL, Bolt,
200-row cursor, concurrent load/read or 4 GiB acceptance criteria.

## After delegated-visibility change: embedded diagnostic

The same four-label fixture was measured after the ACL change, with AC power,
low-power mode off and load averages 6.46 / 7.32 / 12.38 at the start. Both
invariant checks passed. These runs do **not** exercise delegated visibility;
they check for regressions in the existing unfiltered embedded workloads.
The post-change binary SHA-256 was
`84f16d44962474fe85fc9606224bc9191a6f169fe62ca3e27b2fadc111ce71d5`.
The binary was built from commit `d684e14e2e84765f959c59e3babfc4baf0fe951c`
(tree `19662e3c19732a2b71672a753884958cb2dcd2d2`); that commit message was
subsequently amended to record these measurements, without changing the tree.

| Embedded measurement | Before | After |
| --- | ---: | ---: |
| 1M relationship-pair count, median | 0.084 s | 0.091 s |
| Top-brand aggregation, median | 0.356 s | 0.387 s |
| 1M-row cursor, pages of 1,000 | 1.994 s | 1.944 s |
| Eight-session writes with prior reads | 215,592 rows/s | 203,975 rows/s |
| Highest sampled RSS across independent phases | 1,856,962,560 B | 1,829,568,512 B |

Single local runs with different host loads are not evidence of a stable
regression or improvement. Both checks returned `status: ok`. Before claiming
ACL targets, run the Bolt diagnostic against the documented synthetic fixture.

## After query-memory accounting change

The same embedded diagnostic completed and `check` returned `status: ok`.
The host load averages at start were 17.86 / 11.15 / 9.51, higher than the
post-ACL run; AC power and low-power-off were confirmed. The measured source
commit was `950e08e7278cebaaff2fc1fe65fcd1054b778230`; its message was later
amended to include these results without changing its tree. The binary SHA-256
was `f0f736601bd8f17feb8b1b1654a725267b069a4f5463aa47a036529bd80996a9`.

| Embedded measurement | Before memory change | After |
| --- | ---: | ---: |
| 1M relationship-pair count, median | 0.091 s | 0.131 s |
| Top-brand aggregation, median | 0.387 s | 0.616 s |
| 1M-row cursor, pages of 1,000 | 1.944 s | 3.693 s |
| Eight-session writes with prior reads | 203,975 rows/s | 99,660 rows/s |
| Highest sampled RSS across independent phases | 1,829,568,512 B | 1,735,917,568 B |

The slower values need a repeated, quieter run and profiling before assigning
cause. This measures independent embedded phases, not the requested concurrent
Bolt workload or an enforced RSS limit. Query-memory accounting is conservative
and may reject memory-heavy queries earlier; not all allocator/native memory is
accounted.

## Bolt ACL diagnostic: 27 October 2026

A separate disposable graph copied the synthetic baseline, added ACL metadata to
1M Usuario and 1M Carro nodes, and included 800,000 extra Pessoa nodes from the
embedded write phase. The reader had delegated visibility; the restricted policy
saw 500,000 source nodes and 500,000 of the 1M tested relationships. The graph
was frozen for measurement. Python Neo4j driver 5.28.8 used explicit read
transactions; each cursor page was a separate transaction. Three measured query
samples followed a warmup. The host was on AC with low-power mode off and load
averages 8.14 / 18.63 / 16.62 at start. The server binary SHA-256 was
`4e9a539aaefe7b61ae60e55cb331a3f9ceecec221629f32117afe6214ecda2bd`.

| Bolt read | allow_all | Restricted ACL |
| --- | ---: | ---: |
| Relationship count, median | 0.0775 s (1M pairs) | 1.369 s (500k pairs) |
| Top-N aggregation, median | 0.386 s | 1.788 s |
| Cursor, pages of 200 | 14.94 s (1M rows) | 11.30 s (500k rows) |

The server's sampled peak RSS during the diagnostic was 710,459,392 bytes;
this is not a hard RSS limit or a concurrent write/read workload. The driver
journal adds some overhead to cursor wall time; summed query times were 14.65 s
and 11.15 s respectively. The post-benchmark offline check returned `ok`.
Restricted count and top-N miss the stated targets; the 1M-visible-row ACL
cursor target is not yet measured. Raw receipts and the private fixture are local.

## Outstanding

- Reproduce the complete workload, record binary provenance and host load, and
  run `db check`; measure before and after each implementation item.
- Measure a pre-change Bolt baseline and optimize restricted count and top-N;
  retest a cursor with 1M visible rows. Implementation tests cover write denial,
  edge visibility and procedure guards.
- Profile the slower post-memory diagnostic and repeat under comparable host load.
  Parallel worker, cache and selected-field accounting now have tests, but a global
  memory budget and hard 4 GiB RSS target remain unimplemented.
- Qualify seek pagination under sparse ACLs and unlabelled scans.
- Implement and differentially test the requested Cypher features, batch
  procedures and independent synchronous/asynchronous Python client.
