# Typed neighborhood performance — 2026-10-06

Typed Cypher hops now seek the existing `[node ID, relationship type]` adjacency
prefix. Previously they visited every edge of the starting node and filtered
its type in Rust. The storage format, scalar indexes, ontology and transaction
conflict rules are unchanged. Both incoming and outgoing MATCH use the new
path, including matching before eager writes.

## Workload and method

The fixture has one hub and either 10,000 or 50,000 neighbors. Each neighbor has
ten noise relationship types to and from the hub. Only the first ten neighbors
also have WANTED edges in each direction. ABSENT is a declared type with no
instances. Stored node and edge property maps are empty. The larger fixture contains 50,001 nodes and 1,000,020 relationships;
the hub has 500,010 edges in each direction.

These are **warm embedded Rust dev/debug measurements**, not production
throughput or cold disk latency. Database opening, import and graph statistics
are excluded. Each query has five warmups and thirty recorded samples, with
exact result assertions. Before and after read the same closed synthetic
fixture in separate processes. Raw JSON includes all measured sample times;
p50/p95 select the lower percentile sample without interpolation.

Host: Apple M5, 32 GiB RAM, rustc 1.98.1. Store budgets: 64 MiB block cache,
16 MiB memtables. Query accounting limit: 64 MiB; the embedded measurement
disables the execution deadline. Dependencies are the locked rocksdb 0.25.0 /
librocksdb-sys 0.19.0+11.8.1. The original executor is from local commit
`14a5c3d`; the same benchmark source was compiled against both engine versions.

| Query | Before p50 / p95 (ms) | After p50 / p95 (ms) |
|---|---:|---:|
| Outgoing, 10 matches | 367.388 / 398.369 | 0.112 / 0.117 |
| Incoming, 10 matches | 383.565 / 425.939 | 0.111 / 0.120 |
| Absent type, 0 matches | 379.930 / 415.120 | 0.052 / 0.055 |
| NOISE_0, 50,000 matches | 683.062 / 935.319 | 316.302 / 384.004 |
| Projection LIMIT 1 | 368.481 / 414.269 | 0.058 / 0.059 |
| COUNT(*) LIMIT 1 | 389.367 / 428.570 | 0.112 / 0.116 |
| Primary-key count control | 0.040 / 0.043 | 0.040 / 0.041 |

At the smaller scale (10,001 nodes / 200,020 relationships), outgoing COUNT
with ten matches changed from 77.926 ms to
0.139 ms p50. Counting all 10,000 NOISE_0 neighbors
changed from 149.884 ms to 73.645 ms.

The selective queries deliberately represent a favorable case: many unrelated
edges and ten relevant ones. `denseType` counts every NOISE_0 neighbor, showing
the remaining cost of validating and processing many selected nodes.
`projectionLimit` returns a constant with LIMIT 1 and can stop early.
`aggregateLimitOne` is COUNT(*) LIMIT 1: it still counts every selected row and
must not be interpreted as an early-stop test. `pointLookup` is the unchanged
primary-key/count control, not an optimized path in this change.

Typed queries no longer inspect malformed keys of unrelated relationship
types. A truncated selected key still fails; the full storage integrity audit
remains separate. This optimization does not change CSR projection, PageRank, aggregation
algorithms, arbitrary scans or vector search.

## Reproduce the current engine

Use a fresh temporary location; `build` refuses an existing path. The example
requires no server and touches no existing named graph.

```sh
cargo build --locked --offline -p brainwire-cypher --example typed_adjacency
BENCH_DIR=$(mktemp -d)
./target/debug/examples/typed_adjacency build "$BENCH_DIR/fixture" 50000
./target/debug/examples/typed_adjacency run "$BENCH_DIR/fixture"
```

The [benchmark source](../../crates/cypher/examples/typed_adjacency.rs) supports
10–100,000 neighbors. For a before/after comparison, archive `14a5c3d` into a
separate temporary source directory, copy this example into it, and compile
that source against the same Cargo.lock. Save each executable separately.
Prefer separate Cargo target directories. If sharing one, clean the affected
store/Cypher/server packages before rebuilding each engine: Cargo can otherwise
consider an artifact fresh after another checkout replaced the same output
filename. This measurement preserves the baseline binary, then rebuilds the
canonical engine after targeted package cleanup. Keep builds and other load
out of the timed runs. Release and cold-cache measurements remain future work.

## Python and transaction acceptance

The acceptance script copies the closed fixture into an isolated temporary
server directory. It never writes to the supplied fixture. It uses the Neo4j
5.28.6 driver and the same Bolt/Cypher engine available to external application:

```sh
cargo build --locked --offline -p brainwire-server --bin brainwire
uv run --no-project --with neo4j==5.28.6 python \
  docs/benchmarks/2026-10-06-typed-adjacency/python_acceptance.py \
  "$BENCH_DIR/fixture"
```

Four reader tasks submit 100 count queries alongside ten writer batches of ten
nodes/edges. A transaction opened before the writes retains its old snapshot.
Replaying MERGE overwrites 100 nodes without duplicating edges. A rolled-back
edge is visible inside its transaction and absent afterwards. DETACH DELETE
removes one inserted node. The server is abruptly killed with an acknowledged uncommitted edge still
visible inside its open transaction. After restart, the
acknowledged result is still 109 WANTED edges, and both the explicitly rolled-back
node and the node staged in the open transaction at the crash are absent. Exact
offline counts are 50,100 nodes and 1,000,119 relationships; the invariant audit
passes. Per-column-family RocksDB metrics are also captured; these are estimates,
not exact graph counts or process RSS. The warmed localhost Python count
(109 matching edges, 30 samples) measured 0.761 ms p50 / 0.805 ms p95. All test servers and copied data are
removed by the script. This is concurrent-client acceptance, not a sustained
ingestion throughput test.

The production change adds five regression tests covering direction, exact
type-byte boundaries, node boundaries, edge ID order, old snapshots, pending
inserts/deletes, callback errors, invalid column families, corrupt selected
keys, self-loops, residual predicates, eager SET, rollback, reopening and
projection LIMIT cancellation. Workspace validation: 414 tests passed;
Clippy with warnings denied, formatting and diff checks passed.

Raw fixture, paired query measurements and Python results are in
[the results directory](2026-10-06-typed-adjacency/).

## Remaining cost

A separate five-second macOS sampling run of the optimized debug workload
places most of the selected dense-query call-tree branch under `node_record`
and `get_node`/`read_node`; empty-row decoding occupies a much smaller part.
The saved `profile-summary.json` contains overlapping inclusive sample counts,
not additive percentages. This profiling run is excluded from latency tables.
Next investigation: bounded node-read batching/reuse that retains snapshot,
corruption checks, memory accounting and early LIMIT behavior.
