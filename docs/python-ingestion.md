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
secondary indexes accelerate string/Boolean equality in inline MATCH patterns.
Numeric, list and map values fall back to scans, as does an equality in WHERE.

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
included in cache accounting; do not add the two. `memory.query` is configured
but not enforced. Disk shows per-column-family estimates, memtable allocation
and level-zero file counts, not a file browser.

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
