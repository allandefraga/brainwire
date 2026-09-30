# Local sparse analytics workload — 2026-09-30

Measured on the local arm64 macOS 26.6.2 host, Rust 1.98.1, **debug profile**.
One measurement per final workload size; these are development observations,
not production latency guarantees, p95 estimates or cold-device benchmarks.
The OS file cache was not cleared. The million-edge run overlapped compilation
work on the host, which may affect timing.

## Dataset and procedure

The checked-in [example](../../crates/cypher/examples/sparse_analytics.rs) creates
an isolated temporary RocksDB database with `Page` nodes and `LINK` relationships.
Each node has ten distinct outgoing edges: nine ring offsets and a common hub
(or the tenth ring offset where the hub would duplicate an existing target).
The hub has skewed in-degree, so the ranking is nonuniform. Each edge has unit
weight. The benchmark uses graph API writes with normal durable commits, not
raw key injection. Node batches contain 1,000 nodes; edge batches contain at
most 1,000 edges. Conflict history is capped at 50,000 keys; no concurrent
transactions run during measurement.

The database is closed and reopened before projection. After CSR/PageRank
timing, the example executes a second projection through Cypher and returns
the top five entities. Both sizes verify record counts, PageRank mass and
`synthetic-0` as the highest-ranked entity. Their temporary databases are
automatically removed; existing databases are not used.

## Results

| Metric | 100,000 relationships | 1,000,000 relationships |
| --- | ---: | ---: |
| Nodes | 10,000 | 100,000 |
| Node import | 95.68 ms | 1,015.11 ms |
| Relationship import | 1,477.43 ms | 20,173.39 ms |
| Reopen | 375.35 ms | 335.95 ms |
| CSR construction after reopen | 119.36 ms | 1,201.78 ms |
| PageRank on constructed CSR | 114.94 ms | 1,132.09 ms |
| Cypher top five, including another projection | 293.24 ms | 3,347.21 ms |
| Iterations to L1 tolerance 1e-8 | 64 | 64 |
| CSR allocated bytes | 1,280,008 | 12,800,008 |
| Whole-process peak RSS, macOS `time -l` | 50,839,552 bytes | 160,923,648 bytes |
| Whole example elapsed time | 2.63 s | 28.61 s |
| Sum of rank scores | 0.9999999999997904 | 0.9999999999997082 |

Both used a 64 MiB RocksDB cache budget and a 16 MiB memtable budget. CSR bytes
exclude NodeId mapping, degrees, input sorting/compaction, PageRank buffers and
query results. RSS covers the entire process, including import and two analytic
runs; it is not a per-query incremental allocation measurement.

The Cypher statement measured is:

```cypher
CALL graph.pagerank({
  relationship: 'LINK', maxIterations: 100, tolerance: 0.00000001
})
YIELD node, score
RETURN node.entity_id AS id, score
LIMIT 5
```

## Reproduce

```sh
cargo build --locked --offline -p brainwire-cypher --example sparse_analytics
/usr/bin/time -l ./target/debug/examples/sparse_analytics 100000
/usr/bin/time -l ./target/debug/examples/sparse_analytics 1000000
```

The JSON printed by the example is the source of individual phase times.
Raw local outputs from this run are in ignored `target/analytics-100k.json`,
`target/analytics-1m.json` and matching `.log` files. This report preserves the
numbers independently of build-directory cleanup.

## Single-node Cypher reads

A subsequent run of the same checked-in example measured two queries after the
database had been reopened and the analytics phases had run. For each query,
the example issued 100 warm-up reads followed by 1,000 timed reads using
deterministically varied `entity_id` parameters. Each selected node has ten
outgoing `LINK` relationships. The timing includes embedded Cypher parsing,
execution and result creation, but excludes process startup, Bolt transport,
JSON encoding, and import. It is a single sequential debug-profile run with
warm OS/database caches; no concurrency or cold-cache control was applied.

```cypher
MATCH (p:Page {entity_id:$id}) RETURN p.entity_id
MATCH (p:Page {entity_id:$id})-[:LINK]->(q:Page) RETURN q.entity_id
```

| Query and dataset | Mean | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: |
| Point read, 10K nodes / 100K edges | 0.042 ms | 0.038 ms | 0.058 ms | 0.117 ms |
| Ten-neighbor read, 10K nodes / 100K edges | 0.164 ms | 0.160 ms | 0.187 ms | 0.227 ms |
| Point read, 100K nodes / 1M edges | 0.060 ms | 0.043 ms | 0.129 ms | 0.276 ms |
| Ten-neighbor read, 100K nodes / 1M edges | 0.404 ms | 0.255 ms | 0.973 ms | 2.399 ms |

The `entity_id` pattern uses the existing ID lookup; matching an arbitrary
property without `entity_id` currently scans nodes of that type. Outgoing
relationships are fetched from the selected node's adjacency keys. These
results do not establish latency for high-degree hubs, large properties,
remote clients or a production server. The earlier analytics table above
comes from a different run and must not be interpreted as the timings of these
point reads.

## Remaining scale work

- Compare release builds, repeated runs and multiple graph shapes, including
  isolated nodes, very high-degree hubs, long chains and heavy property values.
- Measure peak allocation by phase; avoid treating CSR size as a query budget.
- Enforce a byte-based query budget and cancellation/deadlines across the engine.
- Measure concurrent readers/writers and expensive output projections.
- Measure point reads and neighbor expansion through Bolt, including cold-cache
  and high-degree cases; add secondary property indexes for selective lookup.
- Extend to 10M+ edges only after the memory and query execution limits are in
  place. The five-billion-element target is not demonstrated by this workload.
