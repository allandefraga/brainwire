# Connected components through Cypher — 2026-10-13

Python commands below use the environment from [Python setup](../python-package.md).
These commands use the current checkout's Python dependencies. They do not
recreate the historical dependency environment; original versions remain in the receipts.

Local release workload on Apple M5 (`Mac17,2`), 32 GiB RAM. Synthetic data,
single process, no distributed execution. The generated RocksDB graph was
closed and reopened before querying. Five measured warm samples follow one
warm-up per procedure; these are not production percentile estimates.

The fixture contains 100,000 Page nodes and 99,500 LINK relationships: 1,000
independent groups of 100 nodes, alternating directed rings and chains.
Assertions verify 1,000 weak components and 50,500 strong components, along
with the first representative and its size.

| End-to-end query | Median | Maximum observed | Verified groups |
| --- | --- | --- | --- |
| `graph.wcc` | 379.60 ms | 482.12 ms | 1,000 |
| `graph.scc` | 303.98 ms | 365.91 ms | 50,500 |

Times include the RocksDB scan, sparse projection, algorithm, eager node
bindings, representative filter and final `count(*)`. SCC constructs a sparse
transpose. The query accounting limit was 256 MiB; the cache was 64 MiB and
memtables 16 MiB. No RSS measurements were taken. Import took 707.28 ms through
embedded graph APIs; this is not a Python/Bolt ingestion throughput claim.

Reproduce from the repository root:

```sh
cargo run --release --locked --offline -p brainwire-cypher --example components -- 100000
cargo build --release --locked --offline -p brainwire-server --bin brainwire
.venv-brainwire/bin/python -m examples.analytics.qualify_components
```

Both examples own disposable synthetic databases and remove them. The protected
Python/Bolt test checks exact WCC/SCC rows, zero-weight edge properties,
read-only graph access, denial of writes and identical results after restart.
It passed against native executable SHA-256
`de80cbb78b9990fb00f028405f86d0536f56a057383e8bfb383eeb52952f320e`.
Credentials are private temporary files and are never included in receipts.

Raw receipts: [volume samples](../../benchmarks/archive/2026-10-13-components/volume.json),
[protected Python/Bolt result](../../benchmarks/archive/2026-10-13-components/bolt.json).
An additional fanout-10 fixture has 972,500 relationships with the same asserted
component counts. Its medians were 325.17 ms for WCC and 391.07 ms for SCC;
maximum observed values were 351.08 ms and 540.65 ms. It used the same 256 MiB
query accounting limit. Run it with `-- 100000 10`; see
[dense samples](../../benchmarks/archive/2026-10-13-components/dense-volume.json).
Differences between these small sample sets include host/load/cache variation;
they do not establish a scaling law.

All 554 workspace tests passed with zero failures/ignored cases, along with
workspace Clippy (warnings denied) and formatting checks. New tests include
differential reference connectivity, iterative 100,000-node chain/ring cases,
transaction snapshots and rollback, node aliases, strict option validation,
shared pool rejection/release and cooperative interruption. See
[source/binary provenance](../../benchmarks/archive/2026-10-13-components/provenance.json).

This is a new algorithm candidate; the earlier frozen beta image and its
qualification receipts remain unchanged. Kubernetes/Swarm and replication
were not exercised in this measurement.

See [procedure syntax and boundaries](../sparse-analytics.md). `LIMIT` and
`WHERE` operate after full computation. Cancellation is cooperative: storage
calls and CSR construction/sorting can overrun deadlines. Shared pool accounting
reserves projection growth before allocation, but is not a hard process RSS cap.
