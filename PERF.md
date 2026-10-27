# Read and write performance, October 2026

Work on making Brainwire a primary read/write graph backend for an ingestion
platform: faster scans and aggregations, the Cypher that agents write, online
catalog changes and concurrent writes without false conflicts.

## How it was measured

- Dataset: four labels with 1M nodes each (`Usuario`, `Carro`, `Endereco`,
  `Casa`) and four relationship types with 1M edges each, synthetic.
- Embedded engine: `cargo bench -p brainwire-cypher --bench baseline -- load|read|write|check DIR`.
  Reads report the median of three runs after one warm-up. Writes upsert 200k
  disjoint rows in batches of 1000: `UNWIND ... MERGE ... SET` plus
  `db.setNodeAcls` in one transaction, optionally reading the batch first.
- Bolt: `crates/server/tests/bolt_read_bench.py`, the same queries over the
  Python driver against a release server and against `neo4j:5-community`
  (3 GiB heap and page cache), with the same data and indexes.
- Correctness: `crates/server/tests/differential.py` runs 92 read queries on
  Brainwire and Neo4j over one seeded graph. **92/92 results are identical**,
  with tie order allowed to differ, and `db check` is ok.
- Host: Apple silicon on AC power, `lowpowermode 0`, Docker Linux VM with 10
  CPUs. Final runs had load 5–7, with other services running on the host.

## Before and after

The "reference" columns are the evaluation that asked for this work: Neo4j
and Brainwire over Bolt, on a different and more loaded host, with richer
nodes.

| Query | Ref. Neo4j | Ref. Brainwire | Goal | Before (embedded) | After (embedded) | After (Bolt) | Neo4j, same host (Bolt) |
|---|---:|---:|---:|---:|---:|---:|---:|
| `count` of Usuario→Carro | 0.58 s | 6.6 s | ≤ 0.6 s | 3.08 s | **0.08 s** | 0.07 s | 0.37 s |
| Top brands (group, order, limit) | 0.74 s | 10.1 s | ≤ 0.8 s | 4.31 s | **0.29 s** | 0.27 s | 0.50 s |
| Houses per state (group) | 0.75 s | 12.3 s | ≤ 0.8 s | 4.52 s | **0.25 s** | 0.21 s | 0.40 s |
| `IN` of 100 ids, unindexed property | 0.72 s | 5.0 s | ≤ 50 ms | 3.97 s | **0.09 s** | 0.085 s | 0.22 s |
| `IN` of 100 entity ids | — | — | ≤ 50 ms | 6.79 s | **< 1 ms** | 1 ms | 4 ms |
| `IN` of 100 values, indexed property | — | — | ≤ 50 ms | 6.96 s | **< 1 ms** | — | — |
| 1 hop from an unindexed property | 0.28 s | 3.1 s | ≤ 0.3 s | 1.37 s | **0.04 s** | 0.03 s | 0.16 s |
| Indexed lookup | 1 ms | 1 ms | keep | < 1 ms | < 1 ms | < 1 ms | 1 ms |
| `count` of a label | — | — | — | 0.94 s | 0.04 s | 0.04 s | 2 ms |
| Walk 1M nodes by cursor (pages of 1000) | — | ~150 s | ≤ 10 s | ~3230 s | **2.3 s** | 9.8 s | 11.1 s |

Writes, embedded, 0 conflict retries in every run:

| Sessions | Before | After | After, reading the batch first |
|---:|---:|---:|---:|
| 1 | 94.6k rows/s | 72.3k rows/s | 67.0k rows/s (before: 23 rows/s) |
| 4 | 173.5k | 120.8k | — |
| 8 | 36.7k | **109.2k** | **125.4k** (before: 81 rows/s) |
| 16 | 46.2k | **117.0k** | 123.4k (before: 86 rows/s) |

Loading 4M nodes and 4M edges on 8 threads runs at 153k–189k nodes/s and
215k–260k edges/s, with 0 retries. `db check` is ok after the load and after
the write runs. The reference run reached 26.7k rows/s with 8 sessions over
Bolt, including the platform's own writer.

Single-session embedded writes are about 25% slower than before. The new
executor builds a general row pipeline where the old one had a special case.
With 4 or more sessions throughput is much higher than before, and it no longer
collapses as sessions are added.

## What changed, per item

1. **Scans and aggregations.**
   - A label scan followed by one expansion streams the adjacency column
     family and never reads the nodes.
   - Property reads decode one field, not the whole row.
   - Label scans reuse the bytes they already hold.
   - Grouping is hashed; it used to be a linear search.
   - Scans of labels with ≥ 50k nodes that end in an aggregation, top-k or the
     final RETURN are split across up to 8 threads. Results are merged in id
     order, so they match a sequential scan.
   - The deadline clock is read every 32nd charge.
2. **`IN` and ranges use indexes.**
   - `entity_id =`/`IN`/`>`/`<`/`STARTS WITH` use the primary key.
   - Indexed properties use `=`, `IN`, ranges (numbers and strings) and
     `STARTS WITH`.
   - WHERE conjuncts move to the earliest pattern step that binds their
     variables.
   - Parameter lists are converted once per query.
3. **One-hop expansion** reads adjacency keys only. The target's label comes
   from its id. Multi-hop patterns are anchored on their cheapest node, then
   expanded both ways.
4. **Cursor pagination.**
   - `WHERE n.entity_id > $after ... ORDER BY n.entity_id LIMIT k` scans the
     primary key in order and stops after k rows.
5. **Cypher.** A new planner and streaming executor (see
   `docs/local-cypher.md`) adds:
   - multi-hop and variable-length patterns, untyped and undirected
     relationships, `MATCH (n)`;
   - OPTIONAL MATCH, WITH, UNION;
   - CASE, comprehensions, list predicates, reduce, map projection;
   - pattern predicates and `+`;
   - STARTS/ENDS WITH and CONTAINS;
   - labels/id/elementId/type/properties/keys;
   - `RETURN r` and `RETURN p` as Bolt Relationship/Path;
   - aggregates with DISTINCT;
   - REMOVE, `SET +=`/`=`, MERGE ON CREATE/ON MATCH;
   - DELETE without DETACH.

   Checked by `cypher_features.rs` and the 92-query Neo4j differential test.
6. **Online catalog.**
   - `CREATE/DROP INDEX`, `db.createLabel`, `db.createRelationshipType` and
     `db.dropProperty` run on a live server.
   - Indexes build in the background in resumable batches. Writes maintain
     them from creation, and they are used only once online. See
     `docs/online-catalog.md`.
7. **Nodes without `entity_id`.** CREATE generates an id. MERGE derives a
   stable id from the property map, which covers schema marker nodes.
8. **False conflicts.**
   - The committer drops history only past the oldest active writer, never
     past one, up to 8× the cap.
   - Read-only snapshots no longer pin it, and Cypher reads take the catalog
     dependency only when they write.
   - Result: 0 retries for disjoint writers with 1–16 sessions.
9. **Write throughput.**
   - Snapshot iterators have an upper bound. Before, every point-sized prefix
     check on the write path made RocksDB skip all newer, invisible keys from
     the other sessions, which was the collapse beyond 4 sessions.
   - RocksDB background jobs scale with CPUs.
   - `UNWIND ... MERGE ... SET` is folded into one node write per row.
10. **ACL lists** accept 5000 entries (was 256). Large lists use a hashed
    membership check.
11. **`acl_*` and other caller mistakes** return `Neo.ClientError.Statement.SemanticError`
    (they returned `DatabaseError`).
12. **Healthcheck.** `brainwire probe --mode handshake` checks the Bolt
    handshake without credentials, so the image is healthy in protected mode.

## Still open

- **`count(n)` of a whole label** is a key scan, 40 ms per 1M nodes. Neo4j
  reads a count store in 2 ms. Per-label counters would need conflict-free
  increments in the committer and the replicated path.
- **Single-session writes** are about 25% below the old special-cased
  executor.
- **Commits** take one WAL fsync each under the committer lock. Group commit
  would raise the multi-session ceiling, but it changes the durability and
  ordering code, so it was not attempted.
- **Not supported:**
  - `CALL {}` and `EXISTS {}` subqueries;
  - pattern comprehensions;
  - regular expressions, temporal and spatial types;
  - `SHOW INDEXES` (use `CALL db.indexes()`);
  - catalog DDL in clustered (replicated) mode;
  - dropping labels and relationship types online.
- **Parallel scans** cover a label scan that starts a segment. Visibility-
  restricted transactions and DISTINCT aggregates stay sequential.
