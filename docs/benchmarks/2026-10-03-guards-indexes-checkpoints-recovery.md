# Allocation guards, numeric indexes, checkpoints and recovery - 2026-10-03

Local session on Apple M5 / arm64, 32 GiB host RAM, Rust 1.98.1. Native timings
use the unoptimized development profile. Docker uses a release Linux/arm64
build in a shared Docker Desktop VM (10 CPUs, about 23.4 GiB). Other local
workloads were running, including compilation; these are observations, not
isolated production benchmarks or a latency SLA. All data are synthetic and
all resources are local. No code or image was pushed.

## Equality indexes on 100,000 nodes

[index_lookup.rs](../../crates/cypher/examples/index_lookup.rs) inserts distinct
values into indexed `email` and unindexed `name`, audits the store, reopens it,
then runs five warmups and 25 measured samples per query. Both values are equal;
only the index declaration differs. Every result is asserted against the
expected entity_id. Explicit allocation guard 256 MiB, cache 64 MiB, memtables
16 MiB. Opening, network and initial cache warming are excluded from latency.

| Key type / query | p50 ms | p95 ms |
| --- | ---: | ---: |
| String / pattern index | 0.0712 | 0.1853 |
| String / WHERE index | 0.0639 | 0.1449 |
| String / WHERE scan | 3,442.187 | 3,797.189 |
| Integer / pattern index | 0.0765 | 0.1025 |
| Integer / WHERE index | 0.0615 | 0.0626 |
| Integer / WHERE scan | 3,162.551 | 3,535.971 |

Direct store import took 3.360 s for strings and 2.396 s for integers. This is
not Cypher/Bolt ingestion throughput. Raw data:
[string](2026-10-03-results/index-string-100k.json),
[number](2026-10-03-results/index-number-100k.json).

```sh
cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 100000 string
cargo run --locked --offline -p brainwire-cypher --example index_lookup -- 100000 number
```

## Python/Bolt ingestion and failures

Python Bolt driver 5.28.6, one local connection, batches of 250, synchronous
acknowledged WAL writes. Each Customer carries email, Boolean, list, embedding
list and numeric segment. Three scalar indexes (`email`, `active`, `segment`).
Preparation/opening and verification queries are outside the import clock;
Python batch construction, network and commit acknowledgement are included.

| Rows / query guard | First ingestion | Idempotent replay |
| --- | ---: | ---: |
| 10,000 / 32 MiB | 1.334 s | 1.654 s |
| 100,000 / 256 MiB | 14.472 s | 17.253 s |

The 100K first pass is about 6,910 nodes/s for this shape. This is one machine,
not a parallel throughput target. Replay overwrites the same identities and
must not duplicate nodes. Updates remove stale index entries; explicit NULL
removes email; one node is intentionally deleted. After two SIGKILL/reopen
cycles, 99,999 nodes remain in the 100K test and the offline audit passes.
An interrupted indexed overwrite must disappear, including its numeric entry.
An explosive UNWIND with LIMIT 1 must raise the allocation-guard error and
undo an earlier RUN's overwrite; the same driver session can query afterward.
The self-path and reachable procedures round-trip through Bolt as well.

Raw data: [10K](2026-10-03-results/python-ingestion-10k.json),
[100K](2026-10-03-results/python-ingestion-100k.json).

```sh
python crates/server/tests/python_ingestion.py --binary target/debug/brainwire --rows 100000 --batch-size 250 --query-mib 256
```

## Scope and remaining ceilings

The [allocation guard](../query-allocation-guards.md) is conservative per RUN,
not an RSS limit or shared concurrent pool. Eager full scans remain expensive.
Numeric encoding preserves exact int/float equality; v1 stores must scan until
an atomic v2 rebuild, with the existing 100,000-key-change ceiling. Large online
resumable builds, ranges and vector similarity are pending.

Native checkpoints capture one graph's committed state and reopen independently;
pending writes are excluded. They are local copies, not off-machine backup.
The real offline Disk TUI now explains actual SST/native metadata; it cannot
attach to a running server or execute arbitrary Cypher. See
[all terminal captures](../design/assets/2026-10-03-session/README.md).

The CSR workload and orchestration status are recorded below. Templates and
acceptance scripts alone are not runtime evidence.

## Sparse algorithms on 100,000 nodes / 1,000,000 relationships

The ring-plus-hub fixture in `sparse_analytics` now uses an explicit 256 MiB
allocation guard for projection/Cypher. Direct store import: 1.551 s nodes,
45.169 s relationships, 0.684 s reopening. Projection took 2.440 s; allocated
CSR vectors were 12,800,008 bytes, excluding construction buffers, mapping,
results and RSS. Weighted PageRank converged in 64 iterations (2.278 s directly,
5.053 s through Cypher including projection), with mass 0.9999999999997082.

BFS depth 10 from the fixture's source reached 91 nodes in 0.091 ms, after
projection. The minimum-hop algorithm stops upon discovering its target; a
one-hop source-to-target path took 0.037 ms directly. **The same procedure through
Cypher took 2.519 s**, because it rebuilds the full bounded projection. A short
path does not yet imply a small projection or low end-to-end latency.

Warm point reads (1,000 samples after 100 warmups) had p50 0.0378 ms / p95
0.2128 ms. Expanding ten outgoing neighbors had p50 0.2704 ms / p95 0.7882 ms.
These embedded timings include the allocation guard and exclude network/open.
[Raw workload data](2026-10-03-results/sparse-100k-1m.json).

```sh
cargo run --locked --offline -p brainwire-cypher --example sparse_analytics -- 1000000
```

## End-to-end warm Python/Bolt reads on 100K Customers

A second 100K acceptance run adds 25 measured reads after five warmups. These
latencies include the driver, local network, query worker and result consumption,
using the debug native server; they must not be compared as identical conditions
to embedded or release timings above. Import/replay in this run took 17.978 s /
17.739 s under the shared host load, also showing variability across runs.

| Read | p50 ms | p95 ms | Matching nodes |
| --- | ---: | ---: | ---: |
| Primary entity_id | 1.704 | 3.162 | 1 |
| Indexed email | 2.245 | 3.629 | 1 |
| Indexed numeric segment, count | 582.590 | 653.228 | 14,286 |

The numeric query is intentionally nonselective: an equality index must still
visit/filter every candidate. Counting these rows is currently eager and reads
properties; there is no count pushdown. Both precision and selectivity matter.
[Raw driver data](2026-10-03-results/python-bolt-100k.json).

## Final native regression verification

The final source passed 372 Rust tests, workspace formatting, and clippy with
warnings denied. The guard review reproduced and fixed an additional baseline
case: individually small query text and unused parameters must share one RUN's
limit, not be checked independently. Oversized legacy backfills are also tested
at 50,001 entries; a rejected migration preserves the complete previous index.
The [final 10K driver rerun](2026-10-03-results/python-final-10k.json) passed after
that correction, including guard rollback, index recovery and analytics.

The final native acceptance also passed with Python Bolt driver 6.3.0, including
the custom memory-limit failure, rollback/reset recovery, numeric indexes and
shortest-path procedure. [Driver 6 raw result](2026-10-03-results/python-driver6-final-10k.json).

## Kubernetes attempt

The first isolated kind 0.33.0 cluster was created, but the Brainwire image was
still compiling; that attempt was interrupted and its cluster removed. A second
attempt pinned Kubernetes 1.36.4 from the [official kind release images](https://github.com/kubernetes-sigs/kind/releases/tag/v0.33.0).
It exceeded the 600-second cluster-bootstrap timeout, before applying Brainwire.
Its own cluster was removed in `finally`; no existing Kubernetes context was
changed. [Recorded status](2026-10-03-results/kubernetes-acceptance.json).

The reusable script is implemented, but live retained-PVC recovery is **not
validated in this session**. Retry after a current image is built, without a
concurrent cold C++ build. This outcome is an infrastructure/bootstrap limit,
not evidence that a Brainwire restart lost data. Docker/Compose acceptance on
the final image did not start: the cold Linux RocksDB compilation was still
running after more than 36 minutes, and the final-source build was waiting on
the same target cache. Both owned builds were interrupted at session closure;
no daemon restart or shared-workload changes were made.
[Container status](2026-10-03-results/container-acceptance.json).
The Dockerfile/cache changes passed source review but their current-image
runtime behavior remains unverified.
