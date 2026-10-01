# RocksDB, query and ontology audit — 2026-10-01

This is the baseline measured **before** the first physical string/Boolean
equality index was implemented later on 2026-10-01. The `idx` findings and
lookup times below describe that earlier code state, not the current planner.

Brainwire uses RocksDB as a durable ordered key/value foundation, not as a
complete graph query engine. The current single-node implementation can store
and recover a synthetic graph, but the **5-billion-element/16-GiB target is
unproven**. The largest retained fixture so far is 100,000 nodes and 1,000,000
relationships. Query execution and secondary indexes, rather than RocksDB's
basic storage primitives, are the immediate scale limits.

## Current path

`brainwire db apply-ontology` validates a JSON `GraphMapping`, merges it into a
versioned `Catalog` and commits that catalog in the `meta` column family.
Existing node-type and column IDs are retained across additive revisions.
Node labels, properties, edge signatures and stream definitions are checked
against the catalog by the Cypher executor. The stored `node` row carries its
catalog version; unknown properties remain in a flexible extra map.
The value codec persists null, Boolean, signed integer, float, string, list
and map values. A dense embedding can be represented as a list of floats, but
there is no vector-similarity index or Qdrant-like search path yet. Temporal
and spatial types, blob tiering and object storage are also future work.

RocksDB has 11 column families. `node` stores compact rows by encoded node ID;
`pk` maps `entity_id` plus type to node ID; `out` and `in` store both directions
of an edge. A query constrained by `entity_id` uses the `pk` prefix. Expansion
uses an `out` or `in` prefix. A query by another property scans every node of
the label and evaluates the predicate in the executor. The `idx` and `ident`
column families exist, but no code writes or reads them yet; ontology
`indexed_fields`, `er_fields` and `identifier_columns` currently describe
intent, not physical indexes. Reapplying an ontology does not backfill indexes.

Each write transaction commits an atomic RocksDB `WriteBatch` across column
families with `WriteOptions::set_sync(true)`. Snapshots plus Brainwire's overlay
provide read-your-writes and first-committer-wins conflict checking. The shared
cache and write-buffer manager bound major RocksDB memory components across
named databases. They do **not** cap all process memory: indexes/filters,
iterators, query row vectors, transaction overlays and the Bolt result buffer
also consume RAM. `MATCH`, `UNWIND`, expansion and result rows are currently
materialized; `LIMIT` does not yet make an early streaming plan.

RocksDB documents atomic cross-column-family batches and WAL-backed recovery
in its [overview](https://github.com/facebook/rocksdb/wiki/RocksDB-Overview).
Its [synchronous-write documentation](https://github.com/facebook/rocksdb/wiki/Basic-Operations)
explains what `sync=true` protects; the
[memory guide](https://github.com/facebook/rocksdb/wiki/Memory-usage-in-RocksDB)
lists components outside a simple block-cache limit. These are storage
guarantees, not proof that Brainwire's query layer meets its scale goal.

## Focused local measurement

One temporary database used the checked-in Customer ontology, which declares
`email` in `indexed_fields`. A Neo4j Python 6.3.0 driver loaded Customers with
unique `entity_id` and `email`, then queried the last row through Bolt. Times
include the driver and local loopback. The cache was warm; there were no other
clients. Values below are medians (10 point lookups, three email lookups), not
production latency guarantees.

| Customers | `entity_id` lookup | Declared-index `email` lookup |
|---:|---:|---:|
| 10,000 | 1.94 ms | 148.00 ms |
| 50,000 | 3.31 ms | 514.22 ms |

The email path scans and materializes all nodes of the type, so its growth is
expected. The exact numbers vary by host and cache state. This test used a
temporary directory that was removed afterward and did not touch the retained
100K/1M graph.

`brainwire db metrics staging-100k --json` now exposes RocksDB's per-family
estimates. On the retained synthetic 100K-node/1M-edge graph, `node` reported
100,000 estimated keys, `pk` 100,000, and both `out` and `in` 1,000,000. `idx`
and `ident` each reported zero keys, confirming that those secondary stores
are still unused. Live-data byte estimates on this fixture are small because
its values are highly compressible; they must not be extrapolated to real
documents. Metrics are estimates and do not equal process RSS.

## Next engineering gates

1. Implement physical secondary and identifier keys in `idx`/`ident`, updated
   atomically with node create, update and delete. Define value encoding,
   null/list semantics and uniqueness where required. Add a backfill state
   machine for indexes newly requested by an ontology revision. Make the
   planner choose and test index seeks; expose the chosen access path.
2. Stream label scans, expansions and Bolt records; enforce query-memory and
   result-size budgets. A `LIMIT` query must not first allocate every candidate
   or output row. Add cancellation and slow-query diagnostics.
3. Measure actual RocksDB cache, memtables, table indexes/filters, pinned
   blocks, write stalls, compaction, WAL and total process RSS under 1M, 10M
   and larger heterogeneous workloads. Test cold reads, high-degree nodes,
   concurrent writes, disk-full behavior and kill/restart at each gate.
4. Keep a strict distinction between process crash and machine/storage loss.
   The former passes in local tests; the latter requires retained/restored
   storage and eventually Brainwire's own replication protocol.

No current measurement justifies a claim that five billion graph elements fit
or answer quickly with 16 GiB of RAM. RocksDB is a plausible foundation; the
query planner, indexes, memory discipline and distributed recovery still have
to earn that claim.
