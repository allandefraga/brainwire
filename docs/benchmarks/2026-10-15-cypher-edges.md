# Cypher traversal over a 100k-edge hub — October 15, 2026

A release-mode local fixture has 100,001 `Item` nodes, one indexed `key`
property and 100,000 directed `LINK` relationships from the `hub` node. The
graph is reopened and checked with `check_invariants` before measuring one warmup
and seven warm reads per query.

| Query | Result | p50 | Maximum | Peak reservation (warmup + samples) |
|---|---:|---:|---:|---:|
| Indexed hub lookup, one outgoing edge, `LIMIT 1` | 1 row | 0.012 ms | 0.033 ms | 2.7 KiB |
| Indexed hub lookup, traverse and count all outgoing edges | 100,000 | 234.941 ms | 238.149 ms | 2.5 KiB |

The first query stops after one relationship. The count query visits each
outgoing edge while keeping query-accounted memory low; its runtime scales with
hub degree. A separate 200k-edge run measured 0.010 ms `LIMIT 1` p50 and
399.205 ms for counting all links, roughly 1.7× the 100k count runtime for 2×
the degree. Its import took 1,042.674 ms over 400 local transactions. See the
[200k raw receipt](2026-10-15-cypher-edges-200k.json). The 100k peer-and-edge
load took 449.259 ms in its saved run, including 200 local transactions of 500
peers and links each; both loads exclude database/ontology setup and creation
of the hub node.

This is a warm embedded Cypher measurement, not a Bolt/server or quorum timing.
Peak reservation is the high-water mark across one warmup and seven measured runs; it is query accounting, not RSS. The
[raw receipt](2026-10-15-cypher-edges-100k.json) contains every sample, the
queries and configuration.

Reproduce with:

```sh
cargo run --release --locked --offline -p brainwire-cypher \
  --example streaming_edges -- 100000
# Repeat with 200000 to reproduce the high-degree comparison.
cargo run --release --locked --offline -p brainwire-cypher \
  --example streaming_edges -- 200000
```
