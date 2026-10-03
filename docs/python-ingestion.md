# Python ingestion and the durable write path

Python is the client; Brainwire runs the graph engine in Rust. Send parameterized
Cypher over Bolt using the Neo4j Python driver. The acceptance harness currently
verifies driver 5.28.6; this is a tested protocol subset, not full Neo4j feature
compatibility. The client-specific adapter remains a separate integration task.

## Run the example

From the repository root, with the server stopped:

```sh
cargo build --locked --offline -p brainwire-server
python3 -m venv .venv
.venv/bin/python -m pip install neo4j==5.28.6
./target/debug/brainwire db create customers --ontology examples/python/schema.json
./target/debug/brainwire serve
```

In another terminal:

```sh
.venv/bin/python examples/python/ingest_customers.py examples/python/customers.jsonl
```

Run the same input again: stable `entity_id` values update the same nodes. Each
batch commits independently. JSONL is read incrementally and batches default to
250 rows; large strings/vectors can still make even one batch large. This is an
example importer, not a general-purpose SDK. The example has explicit fields and
requires all of them, preventing an accidentally omitted field from turning into
an unintended property removal. Empty strings are values; JSON `null` removes a
property. Unknown extra fields are not sent by this fixed query.

```python
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    with driver.session(database="customers") as session:
        records = session.run(
            "MATCH (c:Customer {email:$email}) RETURN c",
            email="ada@example.test",
        )
        print(records.single()["c"])
```

The example uses a trusted local server with no authentication/TLS. Different
session database names select separate graphs. They share process cache and
write-buffer resources, but have independent RocksDB directories and catalogs.

## Insertion, overwrite and MERGE

| Operation | Current behavior |
| --- | --- |
| `CREATE` with a new `entity_id` | Creates one node and its index entries |
| `CREATE` with an existing label + `entity_id` | Fails; the transaction is rolled back |
| `MERGE (c:Customer {entity_id:row.entity_id})` | Finds or creates by stable identity |
| `SET c.email = row.email` | Replaces that property; preserves other properties |
| `SET c.email = null` | Removes the property and its old index entry |
| `DETACH DELETE c` | Deletes the node, primary key, active scalar indexes and connected edges |
| Whole-map `SET c = $map` / `SET c += $map` | Not supported yet; enumerate fields explicitly |

Use identity-only MERGE followed by SET for ingestion. Brainwire's MERGE is an
MVP identity upsert and does not implement every openCypher pattern/constraint
rule. Retrying a deterministic SET on the same IDs converges to the same values;
that does not make arbitrary increments or external side effects safe to retry.
A disconnected client can have an unknown commit outcome. Replay only a batch
whose operations have deliberately been made idempotent.

Lists and numeric vectors are stored as values and round-trip through Python.
Storing an embedding does **not** provide an ANN/vector-search index. Current
secondary indexes accelerate string, Boolean and finite numeric equality in
inline MATCH patterns and eligible WHERE predicates. List and map values scan.

## How storage stays consistent

1. Python passes values as Bolt parameters; the Cypher executor opens a snapshot.
2. UNWIND produces bindings; MERGE resolves identity; SET updates the row overlay.
3. Old index keys are removed and new keys inserted in the same transaction.
4. Commit checks conflicts, then applies one RocksDB WriteBatch with synchronous
   WAL. Successful acknowledgement comes after that write succeeds.
5. Memtables hold writes in memory; flush later makes SSTs and compaction
   reorganizes files. A successful transaction does not have to wait for an SST
   flush because the WAL already protects it on restart.

Index backfills track concurrent changes in the scanned node/index prefixes.
If a writer wins before migration commit, migration aborts without publishing
its catalog/index batch. If migration commits first, old Cypher write transactions
conflict on the catalog. Backfill is still a single atomic batch, capped at
100,000 removed/inserted index keys; resumable online index builds are pending.

## Inspect and audit after stopping the server

```sh
./target/debug/brainwire db check customers --json
./target/debug/brainwire shell --overview --estimated --graph customers --view query
./target/debug/brainwire shell --overview --estimated --graph customers --view memory
./target/debug/brainwire shell --overview --estimated --graph customers --view disk
```

Tab changes panels, arrows select a graph, PgUp/PgDn scroll long explanations,
and q exits. The pipeline panel explains the write path and shows persisted
index readiness/declarations. Memory shows shared configured budgets and actual
cache/write-buffer accounting **for the offline inspection process**. Those
numbers are neither live-server RAM nor RSS. Memtable cache reservations are
included in cache accounting; do not add the two. `memory.query` bounds a conservative per-RUN guard and the shared server
reservation pool; neither is RSS; see [guard limits](query-allocation-guards.md). Disk now lists real
files with sizes, native live-SST CF/level metadata and explanations. Left/Right
selects a graph; Up/Down selects a file; F1 reveals its explanation on narrow
terminals. Inspection is offline and does not read file contents.

`db check` performs an expensive offline scan: primary-key/node correspondence,
edge mirrors/endpoints, allocator high-water marks, and missing/stale entries
for ready active scalar indexes. It does not repair data, audit unused retired
index prefixes, or verify indexes whose legacy catalog has no readiness marker;
JSON reports that last case as `skipped_not_ready`. It is not a replacement for
backups or a proof against every form of file/hardware corruption.

## Reproduce acceptance

```sh
.venv/bin/python crates/server/tests/python_ingestion.py --rows 50000 --batch-size 250
```

The harness creates only temporary synthetic databases, starts its own server,
checks whole-batch rollback on duplicate CREATE, bounded ingestion and replay,
indexed overwrites/null/delete, transaction rollback, lists/vectors, graph
isolation, SIGKILL with an uncommitted indexed overwrite, restart and the final
offline consistency audit. The [measured results](benchmarks/2026-10-01-python-ingestion.md)
include 5K, 50K and 100K inputs. It always
stops its child process and removes its temporary directory.


## Next integration: DuckLake first, Arrow into Rust

Priority confirmed by Allan on 2026-10-01: client reads primarily from DuckLake;
DuckDB, Parquet and Delta Lake should share the ingestion interface. This section
is a proposed next implementation, not an installed connector or scheduled job.
The approved design already proposes Arrow Flight; the workspace does not yet
include Arrow dependencies or a Flight server.

Proposed remote path: source reader -> Arrow RecordBatch stream -> Flight DoPut
-> Rust schema/mapping validation -> existing graph transactions -> RocksDB.
Python controls jobs and mapping, while native readers and Rust handle batches.
Cypher/Bolt remains the query and general-write interface. The bulk path must
share identity, index maintenance, ontology checks, conflicts and synced-WAL
commit semantics rather than implement a separate writer.

DuckDB can export results through an [Arrow batch reader](https://duckdb.org/docs/stable/clients/python/relational_api).
Rust Arrow provides [RecordBatch](https://arrow.apache.org/rust/arrow/record_batch/struct.RecordBatch.html)
and [Flight streaming](https://arrow.apache.org/rust/arrow_flight/client/struct.FlightClient.html).
This can avoid constructing a Python dictionary for every source row and the
Bolt parameter conversion used by the current example. Actual end-to-end gains
must be measured; graph identity resolution, key encoding, indexes, WAL sync,
source I/O and compaction remain costs. Arrow does not replace RocksDB.

Within one process, the [Arrow C Data Interface](https://arrow.apache.org/docs/format/CDataInterface.html)
can share buffers without copying. Across containers/machines use Flight/IPC;
network transfer and graph-row encoding mean this is not end-to-end zero-copy.

Source-specific synchronization contracts:

- **DuckLake:** pin an initial snapshot, load its mapped columns, then consume
  [table_changes](https://ducklake.select/docs/stable/duckdb/advanced_features/data_change_feed)
  beginning after the fully acknowledged snapshot (range bounds are inclusive).
  Preserve snapshot ordering and process preimages when identities or edge
  endpoints change. A source rowid is not the business entity_id.
- **Delta Lake:** use a Delta-aware snapshot reader, not a raw Parquet glob.
  [Change Data Feed](https://docs.delta.io/delta-change-data-feed/) requires
  enablement and retained history for the interval being consumed. Validate
  the deployed reader's supported table features and schema-change behavior.
- **DuckDB:** query a consistent source view; incremental ingestion needs an
  explicit change log or reliable cursor with tie-breaking. Timestamps alone
  do not identify hard deletes.
- **Parquet:** use an immutable file manifest/version for each run. Append-only
  file discovery can ingest new data, but replacements and row deletions need
  explicit metadata or reconciliation; a missing file is not automatically a
  graph deletion.

Bound batches by rows and bytes, limit queued/in-flight batches, and pause source
reads when the graph cannot keep up. Validate Arrow-to-Value conversions, overflow,
time zones, decimals, nested values, and null versus omitted fields explicitly;
Arrow's richer type system does not automatically extend Brainwire's Value enum.

Checkpoint by source identity, target graph and mapping version, advancing only
after the graph acknowledges durable commit. Prefer an atomic batch-receipt token
in the graph transaction; that requires new engine support. Until then, ordered
at-least-once delivery plus deterministic upserts/deletes is the intended model.
Fence concurrent workers for one stream so an old replay cannot overwrite newer
state. Do not claim whole-snapshot atomic visibility when committing in batches.
If retained source history no longer covers the checkpoint, stop and perform a
controlled rebuild/reconciliation; never silently skip the missing interval.

Initial synchronization should load node identities before dependent edges.
Changes to derived joins require tracking and recomputing affected relationships.
Source deletion must follow mapping ownership and retention policy: removing a
source record must not delete an entity still supported by another source.

Planned TUI ingestion view: source/target, selected snapshot, applied checkpoint,
lag, records and bytes per second, batch queue/memory, retries and error details.
First acceptance gate: compare Python/Bolt against Arrow/Rust on the same source,
data, mappings, indexes and durability settings, including restart during a batch,
commit-before-checkpoint crash, update/delete replay and missing-history recovery.

The current acceptance script supports `--query-mib 1..256` (default 32).
Use `--rows 100000 --batch-size 250 --query-mib 256` for the recorded 100K
workload. It checks numeric segment indexes, guard-failure rollback through the
real driver, self-path analytics, and end-to-end read latency. A numeric group
with many matching rows is reported separately from a unique entity/email read.
