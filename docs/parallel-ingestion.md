# Parallel writes and DuckLake synchronization

This guide is for a writer that has to load a lot of data and keep it current:
how to write, how many processes to run, what happens when they collide, and
how to read a DuckLake table directly, first in full and then incrementally.

## How to write

Send batches, not rows. Every write transaction ends with a synchronous WAL
write, so one-row transactions pay that cost per row. Use `UNWIND $rows AS row
MERGE (n:Label {entity_id: row.id}) SET ...` with 250–1000 rows per
transaction, which is what `GraphClient.load_nodes`/`load_edges` do. The
[Python client](python-client.md) describes the property rules (missing keeps,
`None` removes) and the matching delete helpers.

| Need | Use |
| --- | --- |
| Insert or update nodes | `GraphClient.load_nodes(label, rows)` |
| Insert or update edges (endpoints must exist) | `GraphClient.load_edges(source_label, type, target_label, rows)` |
| Delete nodes and their edges | `GraphClient.delete_nodes(label, ids)` |
| Delete edges, keep nodes | `GraphClient.delete_edges(...)` |
| Mirror a DuckLake table, then keep it current | `examples/python/ducklake_sync.py` |

Load nodes before the edges that reference them; an edge batch whose endpoint
is missing rolls back as a whole. Read back what you wrote, including edge
properties: `MATCH (a:Customer)-[r:FOLLOWS]->(b:Customer) WHERE r.weight > 0.5
RETURN a.entity_id, b.entity_id, r.weight`.

## Many writers at once

The server accepts concurrent sessions: each connection has its own worker and
transaction, and transactions commit with optimistic conflict detection. Two
transactions that touch the same node or edge cannot both commit; the loser
gets `Neo.TransientError.Transaction.Outdated`, and nothing it staged is
visible. The Neo4j driver's `session.execute_write` (used by `GraphClient`)
retries transient errors with backoff, by default for up to 30 seconds.

So any number of writers is *correct*. To make it *fast*:

- **Partition by key.** Give each process a disjoint set of identities, for
  example `hash(id) % workers`. Writers that never share a node do not
  conflict. `ducklake_sync.py` does this.
- **Keep transactions small** (hundreds of rows). A conflict retries the whole
  batch.
- **Retry only through `execute_write`** or an equivalent loop on transient
  errors. Do not retry a write whose outcome is unknown unless it is
  idempotent; MERGE + SET with source values is.
- **Stay under the connection limit.** The server admits 64 connections by
  default ([connection limits](connection-limits.md)); one driver per process
  with one session in use needs one connection.

Ten Python processes is a reasonable starting point. Measure before going
higher: commits are serialized at the WAL, so past a point more writers only
add contention.

## DuckLake: full load, then incremental

`examples/python/ducklake_sync.py` reads a DuckLake table directly and mirrors
it into one node label or one edge type. Describe the mapping in JSON:

```json
{"table": "main.customers", "label": "Customer", "id": "customer_id",
 "properties": {"name": "name", "segment": "segment", "score": "score"}}
```

```json
{"table": "main.follows", "type": "FOLLOWS",
 "source": {"label": "Customer", "column": "follower_id"},
 "target": {"label": "Customer", "column": "followed_id"},
 "properties": ["weight"]}
```

`properties` is a list of column names or a `{property: column}` map. Every
mapped column is authoritative: a source `NULL` removes the property. The graph
ontology must declare the label/type and properties, plus a cursor label:

```json
{"label": "ImportCheckpoint", "properties": ["snapshot", "lease"], "indexed_fields": []}
```

Before the first run, create the graph with that ontology and start the
server; the ontology changes offline, so declare every mapped property up
front:

```sh
brainwire db create crm --ontology ontology.json
brainwire serve
```

On a protected server, point `BRAINWIRE_BOLT_AUTH_FILE` at a writer
credentials file ([Bolt authentication](bolt-auth.md)); every worker reads it.

Run it once per table, nodes first:

```sh
uv run --no-project --with duckdb==1.5.5 --with neo4j==5.28.6 \
  python -m examples.python.ducklake_sync customers.json \
  --lake ducklake:sqlite:/abs/catalog.sqlite --database crm \
  --source-id crm-customers --workers 10 --batch-size 500
uv run --no-project --with duckdb==1.5.5 --with neo4j==5.28.6 \
  python -m examples.python.ducklake_sync follows.json \
  --lake ducklake:sqlite:/abs/catalog.sqlite --database crm \
  --source-id crm-follows --workers 10
```

It prints a JSON summary with the snapshot range, mode, upserts, deletes and
seconds. Run the same commands again later (by hand or from a scheduler) to
apply only what changed.

### What a run does

1. Takes a lease on `ImportCheckpoint {entity_id: source_id}` and reads the
   stored snapshot.
2. Pins the target snapshot (latest, or `--to-snapshot`) and refuses null or
   duplicate identities at that snapshot.
3. **First run:** reads the whole table `AT (VERSION => target)`.
   **Later runs:** reads `ducklake_table_changes` for (stored, target] and keeps
   one final state per key: the last snapshot wins, and within one snapshot an
   insert or post-image wins over a delete or pre-image. A row whose key
   changed becomes a delete of the old key plus an upsert of the new one; an
   edge whose endpoint moved deletes the old pair.
4. Starts `--workers` processes. Each attaches the lake read-only, reads only
   `hash(key) % workers == k`, and writes upserts with `load_nodes`/`load_edges`
   and deletes with `delete_nodes`/`delete_edges` in atomic batches.
5. After every worker succeeds, advances the cursor to the target snapshot and
   releases the lease in one transaction. On failure it releases the lease
   without advancing.

Because each run applies final states, applying the same range twice gives the
same graph. A failed or killed run is repaired by running again. If a process
was killed hard, its lease stays; `--break-lease` takes it over, but only use it
once that run is really gone, because a live old run could still write.

### Row by row or in batches?

Batches. DuckDB already returns the changes collapsed per key, so a run sends
each changed key once, in batched transactions. Row-at-a-time sync only adds
a synchronous commit per row; see the measurement below.

### Limits

- **Retained history.** Incremental runs need every snapshot from the cursor to
  the target. If they were expired (`ducklake_expire_snapshots`), the run
  fails; reconcile with `--full --prune`.
- **`--full` alone does not delete.** It upserts every source row but leaves
  graph keys that are absent from the source. Add `--prune` to then delete
  those keys (graph keys are listed in one query, bounded by the server's
  `memory.query`, and anti-joined against the snapshot in DuckDB). Use
  `--full --prune` when history has expired or the graph may have drifted.
- **Node deletes remove edges.** Deleting a node also deletes its edges in the
  graph. If a later snapshot brings that key back while its edge rows did not
  change, the incremental edge run does not see them; run the edge table with
  `--full --prune` after node deletions you expect to be undone.
- **Reserved columns.** `snapshot_id`, `rowid`, `change_type` and
  `brainwire_rank` cannot be mapped; they come from the change feed.
- **One run per source.** The lease enforces it. Different tables or graphs run
  in parallel freely.
- **Ownership.** Deletes are physical (`DETACH DELETE` for nodes). Do not point
  two sources at the same label/keys.
- **Values.** Decimals become floats, dates/times become ISO strings, UUIDs
  become strings; blobs are refused. Identity columns are cast to strings.
- **Object storage.** Pass `--init-sql` with a `CREATE SECRET` that reads
  credentials from the environment or a provider chain; extensions must
  already be installed (`--load`). Postgres catalogs work through DuckDB's
  `postgres` extension but are not part of the tests here.

This is a bridge for keeping a graph current from DuckLake, not native CDC:
readers can see partial progress between committed batches during a run.

## Measured

One local run each, debug build in a Linux container, synthetic data, 10
worker processes, 500-row batches. These are smoke numbers, not benchmarks;
a release build is faster.

**Concurrent writers** (`concurrent_writers.py`, 20K rows): 10 processes
writing disjoint keys reached about 51,000 rows/s with no conflicts. Ten
processes rewriting the same 200 keys, and a mix of edge creation, `DETACH
DELETE` and re-upserts, produced 26 conflicts in total; every one was retried
by the driver and succeeded, and the final state was exact. The same 2,000
rows took 4.2 s as one-row transactions (about 475 rows/s) and 0.05 s in
batches: row-by-row is roughly 90 times slower. `db check` was clean.

**DuckLake sync** (`ducklake_sync.py`, 100K customers and 100K edges):

| Run | Changes | Seconds |
| --- | --- | --- |
| First load, nodes | 100,000 upserts | 2.8 |
| First load, edges | 100,000 upserts | 3.7 |
| Incremental, nodes (10% updated, 5% deleted, 5% new, one key change) | 15,001 upserts, 5,001 deletes | 5.0 |
| Incremental, edges (endpoint moves) | 1,785 upserts, 1,785 deletes | 1.1 |
| Rerun with no new snapshot | none | 0.4 |

11 transient conflicts (deletes touching neighbours in other partitions) were
retried automatically. Afterwards every node, property and edge in the graph
matched the lake at the target snapshot, the lease was enforced and
`--break-lease` recovered it. A 20K run also added stray nodes and edges
directly in the graph; `--full --prune` removed exactly those and the graph
matched the lake again. `db check` was clean.

## Reproduce

```sh
uv run --no-project --python 3.12 --with duckdb==1.5.5 \
  python -m unittest examples.python.test_ducklake_sync -v
python3 crates/server/tests/ducklake_sync.py --binary target/debug/brainwire --rows 100000
python3 crates/server/tests/concurrent_writers.py --binary target/debug/brainwire
```

The acceptance scripts create only temporary synthetic data, start their own
server, stop it and remove everything afterwards.
