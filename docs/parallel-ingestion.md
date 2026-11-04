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

## Test run against an existing lake

A complete first run, from a source checkout, with a protected server. Keep
the credential directory outside the repository.

1. Build and start the server from this checkout so the server and Python
   helper use matching code:

   ```sh
   docker build -t brainwire:local .
   python3 -m examples.python.create_service_tokens ~/brainwire-auth --graph lake --acl-writer
   docker volume create brainwire-data
   docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
     --mount type=bind,src="$PWD/ontology.json",dst=/ontology.json,readonly \
     brainwire:local db create lake --ontology /ontology.json
   docker run --detach --name brainwire-local --init -p 127.0.0.1:7687:7687 \
     --mount type=volume,src=brainwire-data,dst=/data \
     --mount type=bind,src="$HOME/brainwire-auth/policy.json",dst=/auth/policy.json,readonly \
     --env BRAINWIRE_SERVER_AUTH_FILE=/auth/policy.json \
     --env BRAINWIRE_SERVER_DEFAULT_DATABASE=lake \
     --env BRAINWIRE_MEMORY_CACHE=1GiB --env BRAINWIRE_MEMORY_QUERY=512MiB \
     brainwire:local
   docker exec brainwire-local brainwire probe --mode ready
   ```

   `--acl-writer` makes `writer` an instance admin so it can stamp ACLs; omit
   it when no mapping has an `acl` block. `reader` is a plain graph reader;
   add a `node_visibility` policy to a reader to test tenant isolation
   ([node ACL](node-acl.md)).

2. Write one mapping file per table (see below) and declare every mapped label,
   property and edge type, plus `ImportCheckpoint`, in `ontology.json`. The
   ontology changes offline only, so get it right before loading.

3. Point the sync at the lake. A Postgres catalog with data in S3-compatible
   storage needs the `postgres`, `httpfs` and `aws` extensions installed in
   DuckDB. Credentials come from the environment (`PGPASSWORD`,
   `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`), never from files:

   ```sql
   -- lake-init.sql: endpoint settings only, no secrets
   CREATE SECRET lake_s3 (TYPE s3, PROVIDER credential_chain, CHAIN 'env',
                          ENDPOINT 'minio.internal:9000', URL_STYLE 'path', USE_SSL false);
   ```

   ```sh
   export BRAINWIRE_BOLT_AUTH_FILE=~/brainwire-auth/writer.json
   uv run --no-project --python 3.12 --with duckdb==1.5.5 --with neo4j==5.28.6 \
     python -m examples.python.ducklake_sync people.json \
     --lake 'ducklake:postgres:dbname=lake_catalog host=pg.internal user=lake_reader' \
     --load ducklake,postgres,httpfs,aws --init-sql lake-init.sql \
     --database lake --source-id clean-people --workers 8
   ```

   Run node tables first (they may run at the same time), then edge tables.
   Each command prints a JSON summary. Rerun the same commands to apply only
   what changed since the stored snapshot.

   `--batch-size` must be a positive integer. Invalid batch sizes are rejected
   before connecting to Brainwire or acquiring the source lease, so a local
   configuration mistake does not leave the source locked.

4. Check the result with Cypher as `reader`, for example
   `MATCH (p:Person)-[:WORKS_AT]->(c:Company) RETURN c.segment, count(*)`.

## Many writers at once

The server accepts concurrent sessions: each connection has its own worker and
transaction, and transactions commit with optimistic conflict detection. Two
transactions that touch the same node or edge cannot both commit; the loser
gets `Neo.TransientError.Transaction.Outdated`, and nothing it staged is
visible. `GraphClient` uses explicit one-shot transactions and propagates
failures. Earlier beta helpers used the Neo4j driver's managed `execute_write`,
which can replay a callback after a lost COMMIT response. Automatic retries
were removed so an ambiguous write is not silently submitted again.

Concurrent writers still need bounded admission and workload qualification.
To reduce contention:

- **Partition by key.** Give each process a disjoint set of identities, for
  example `hash(id) % workers`. `ducklake_sync.py` does this, but deleting
  different nodes can still contend when they share an incident edge.
- **Keep transactions small** (hundreds of rows). A confirmed conflict rejects
  the whole batch; retrying it repeats all its work.
- **Distinguish rejection from uncertainty.** Apply bounded backoff only when
  the server confirms the transaction was rejected or rolled back and the
  source operation is still current. A connection failure after COMMIT may
  hide a successful write: reconcile before retrying. MERGE + SET can overwrite
  concurrent newer values even when it does not create duplicate nodes. For
  retained quorum identities, use `write_transaction(..., request_id=...)` and
  follow the [request lookup contract](quorum-replication.md).
- **Stay under the connection limit.** The server admits 64 connections by
  default ([connection limits](connection-limits.md)); one driver per process
  with one session in use needs one connection.

Ten Python processes is a reasonable starting point. Measure before going
higher: commits are serialized at the WAL, so past a point more writers only
add contention.

## DuckLake: full load, then incremental

`examples/python/ducklake_sync.py` reads a DuckLake table directly and mirrors
it into one node label or one edge type. Describe the mapping in JSON:

Edge mappings use only source and target IDs as identity. The sync refuses a
keyed relationship type before acquiring its source lease; it cannot safely
reconcile multiple keyed siblings. The target relationship signature must be
visible through the authorized graph schema.

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

Rows that carry access control map their columns to the node's private ACL
with an `acl` block (node mappings only). The values are stamped with
`db.setNodeAcls` in the same transaction as the properties, so a node never
commits without its ACL; this needs a writer principal with graph-write and
instance-admin grants and no `node_visibility` policy ([node ACL](node-acl.md)):

```json
{"table": "clean.people", "label": "Person", "id": "external_id",
 "properties": ["name", "email", "company_id", "updated_at"],
 "acl": {"allow": "acl_allow", "connectorIds": "acl_connector_ids",
         "sourceTable": "acl_source_table"}}
```

The `acl` options are `allow`, `deny`, `connectorIds`, `sourceTable` and
`sources`; list options take `VARCHAR[]` columns, and a `NULL` leaves that
option empty. The ACL is replaced on every upsert of the row.

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
   releases the lease in one transaction. Failed work retains the lease, even
   for validation or conflict errors. A lost final release reply may already
   have advanced the cursor and cleared the lease; reconcile before continuing.

Replaying a range converges under the single-owner source contract, provided
there are no concurrent newer writes. A batch retries up to eight times with
backoff only when the server returns `Transaction.Outdated` and explicitly
confirms rollback. Network errors, unknown commit outcomes and lease operations
are not retried automatically. After failure, resolve previous workers and all in-flight write
outcomes before explicitly using `--break-lease` and reconciling the graph. A
client disconnect alone does not prove its server transaction has finished.

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
  `memory.query`, and anti-joined against the snapshot in DuckDB). The Python
  client fetches and deletes missing IDs in batches, rather than keeping all
  missing IDs in one list. This does not bound DuckDB's anti-join memory or the
  server's graph enumeration. Delete batches commit separately. Use
  `--full --prune` when history has expired or the graph may have drifted.
- **Node deletes remove edges.** Deleting a node also deletes its edges in the
  graph. If a later snapshot brings that key back while its edge rows did not
  change, the incremental edge run does not see them; run the edge table with
  `--full --prune` after node deletions you expect to be undone.
- **Missing endpoints.** An edge row whose source or target node does not
  exist fails its batch, the run stops and the cursor stays put, so a dangling
  foreign key in the lake blocks that table until it is fixed. Pass
  `--allow-missing-endpoints` to skip and count such rows (`skipped` in the
  summary) instead; a skipped edge is not retried when its node arrives later
  unless its row changes again, so follow up with `--full --prune`.
- **Reserved columns.** `snapshot_id`, `rowid`, `change_type` and
  `brainwire_rank` cannot be mapped; they come from the change feed.
- **One run per source.** The lease coordinates source ownership; it is not a
  fencing token. Never break it while prior workers or writes may remain active.
  Independent sources can run in parallel if they do not own overlapping data.
- **Ownership.** Deletes are physical (`DETACH DELETE` for nodes). Do not point
  two sources at the same label/keys.
- **Values.** Decimals become floats, dates/times become ISO strings, UUIDs
  become strings; blobs are refused. Identity columns are cast to strings.
- **Object storage.** Pass `--init-sql` with a `CREATE SECRET` that reads
  credentials from the environment or a provider chain; extensions must
  already be installed (`--load`). A Postgres catalog with Parquet data in a
  MinIO bucket was qualified with the workload below (throwaway containers,
  credentials only in environment variables); other object stores and
  managed Postgres services were not.

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

This historical run used managed retries: 11 transient conflicts (deletes
touching neighbours in other partitions) were retried automatically. Current
helpers propagate those errors; these timings have not been requalified under
the one-attempt policy. Afterwards every node, property and edge in the graph
matched the lake at the target snapshot, the lease was enforced and
`--break-lease` recovered it. A 20K run also added stray nodes and edges
directly in the graph; `--full --prune` removed exactly those and the graph
matched the lake again. `db check` was clean.

**Multi-source workload** (`lake_workload.py`, scale 10,000). The lake has a
`clean` schema with 10K companies, 30K people and 60K tickets, every row
carrying tenant ACL columns, plus two foreign-key edge types (person
`WORKS_AT` company, person `OPENED` ticket). The server is protected: a loader
principal with write and instance-admin grants, and one restricted reader per
tenant. The three node tables sync at the same time, then the two edge types,
each with 4 worker processes (12 processes writing at once):

| Step | Changes | Seconds |
| --- | --- | --- |
| First load, 3 node tables with ACL | 100,000 upserts | 4.7 |
| First load, 2 edge types | 90,000 upserts | 2.0 |
| Each of 3 change cycles, nodes | ~10,000 upserts, ~2,000 deletes | ~1.5 |
| Each of 3 change cycles, edges | ~9,000 upserts, ~900 deletes | ~1.0 |

A cycle renames companies, moves 3% of people to another company, shares 1%
of one tenant's people with the other tenant (ACL change only), closes 10% of
open tickets, deletes 1% of people with their tickets and 2% of other tickets,
and inserts new people and tickets. During the cycles the two tenant readers
ran 420 queries over traversals and indexed filters: no errors, no row from
the other tenant (p50 30 ms, p95 88 ms). Afterwards every node, property and
edge matched the lake, each tenant saw exactly the people its group is granted
(including the shared ones), and `db check` was clean.

**Same workload at scale 100,000, release build** (one local run in a Linux
container with 10 CPUs, 4 processes per table, 2 cycles):

| Step | Changes | Seconds |
| --- | --- | --- |
| First load, 3 node tables with ACL | 1,000,000 upserts | 12.9 |
| First load, 2 edge types | 900,000 upserts | 6.1 |
| Change cycle, nodes | ~100,000 upserts, ~21,000 deletes | 4.4–4.7 |
| Change cycle, edges | ~95,000 upserts, ~9,000 deletes | 1.9–2.1 |

Tenant readers ran 2,464 queries during the cycles with no errors and no
foreign rows (p50 7 ms, p95 30 ms). The final graph (314K people, 604K
tickets, 918K edges) matched the lake, per-tenant visibility was exact and
`db check` was clean. The lake was a local SQLite-catalog DuckLake; a network
catalog or object store adds its own read time.

## Reproduce

```sh
uv run --no-project --python 3.12 --with duckdb==1.5.5 \
  python -m unittest examples.python.test_ducklake_sync -v
python3 crates/server/tests/ducklake_sync.py --binary target/debug/brainwire --rows 100000
python3 crates/server/tests/concurrent_writers.py --binary target/debug/brainwire
python3 crates/server/tests/lake_workload.py --binary target/debug/brainwire --scale 10000
# Same workload on an empty Postgres-catalog DuckLake with data in S3-compatible storage:
PGPASSWORD=... AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... AWS_REGION=us-east-1 \
python3 crates/server/tests/lake_workload.py --binary target/debug/brainwire --scale 1000 \
  --lake 'ducklake:postgres:dbname=lake_catalog host=pg user=postgres' --data-path s3://lake/data/ \
  --load ducklake,postgres,httpfs,aws --init-sql lake-init.sql
```

The acceptance scripts create only temporary synthetic data, start their own
server, stop it and remove everything afterwards.
