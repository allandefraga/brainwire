# Sync a DuckDB table

The example `examples.python.duckdb_sync` imports one **base table from a local
DuckDB file** into one standalone Brainwire graph. Its first run loads the
snapshot; later runs compare a new snapshot with the last confirmed one and
write only differences. This is a snapshot comparison, not a native change
feed: it reads the complete table each time.

Start with a rebuildable graph and a tested backup. This is an experimental
single-source pilot, not a general production CDC connector.

## 1. Prepare a graph and mapping

Use a graph whose ontology declares the mapped label and an `ImportCheckpoint`
node with `snapshot`, `lease`, `contract_fingerprint`, and `batch` properties.
For example, an ontology for the sample below includes:

```json
{
  "version": "1",
  "nodes": [
    {"label": "Customer", "properties": ["name", "segment"]},
    {"label": "ImportCheckpoint", "properties": ["snapshot", "lease", "contract_fingerprint", "batch"]}
  ],
  "edges": []
}
```

Create the graph before starting the server, or declare its schema online. See
[Docker and volumes](container.md) and [online catalog](online-catalog.md).
Save a mapping as `customers.json`:

```json
{"table":"main.customers","label":"Customer","id":"id",
 "properties":{"name":"name","segment":"segment"}}
```

The ID column must produce non-null, unique IDs after conversion to strings.
Confirm this mapping has **exclusive ownership of its target identities**: the
sync can overwrite matching nodes, and it cannot prove that another producer
is not using them. Use a dedicated graph or non-overlapping IDs.

## 2. Run the first sync

From the repository checkout, set up Python and the optional reader:

```sh
python3 scripts/setup_python.py
.venv-brainwire/bin/python -m pip install duckdb==1.5.5
mkdir -p ./sync-state
chmod 700 ./sync-state
.venv-brainwire/bin/python -m examples.python.duckdb_sync customers.json \
  --source /path/to/source.duckdb --journal ./sync-state/customers.sqlite \
  --uri bolt://127.0.0.1:7687 --database mygraph \
  --source-id customers --source-identity source-generation-1 --exclusive
```

Configure credentials as described in [Python connections](python-client.md)
for authenticated servers. The example supports one local table per mapping;
run node mappings before relationship mappings. It rejects views and external
file access. The persistent journal contains source data: keep its directory
private and back it up alongside the graph.

The reader may be unable to open a DuckDB file while another process holds its
write lock. Run the sync after the producer closes it, or publish consistent
snapshot copies at the same stable path.

## 3. Sync again

Run the same command after the DuckDB table changes. The journal freezes a
pending snapshot before graph writes and records batch progress in the graph.
After an interrupted run, rerun the same command to finish the frozen snapshot
before reading newer source data. Do not delete or replace the journal to reset
an existing source. A lost or mismatched journal needs reconciliation; changing
the source path, Bolt URI, mapping or source generation also requires an
intentional new contract or reconciliation.

By default, missing source IDs are **not deleted**. To remove IDs previously
seen in the confirmed snapshot, use `--authoritative` from the first run and on
every subsequent run. That option is bound to the source contract. An empty
authoritative snapshot also requires `--allow-empty-authoritative`. Node
removal does not detach relationships: delete the relevant edges first or the
batch fails. It does not protect against another writer modifying the same IDs.

A batch is atomic, not the entire sync. A confirmed earlier batch remains after
failure. A successful graph commit whose reply was lost is reconciled against
the checkpoint when resumed. The lease guard applies only to cooperating
clients using this standalone implementation; it is not replicated fencing.
Use separate source IDs and journals for distinct mappings.

## Bounds and limitations

The default limits are two million rows and 2 GiB of normalized snapshot
content, with 512 KiB per row and 1 MiB per graph batch; `--max-rows` and
`--max-snapshot-bytes` can lower the first two. DuckDB is configured with a
256 MiB memory budget, two threads, no spill and no external access. These are
not an RSS or disk-usage guarantee: values are materialized before their size
is checked, Python and SQLite use additional memory and disk, and large source
queries may fail. A local POSIX filesystem is required for the journal lock.

The state contract is versioned: an incompatible change to batch planning
requires reconciliation, not blind replay. This example does not provide a
shared multi-machine lease, native CDC, graph-wide atomic cutover or automatic
recovery after loss or rollback of both the graph and journal.

## Local validation

One [synthetic 100,000-row acceptance run](benchmarks/2026-11-14-duckdb-sync-100k.json)
completed in 44.177 seconds with a native debug build. That time covers the
whole test workflow, including multiple syncs, simulated lost replies, recovery
checks and an offline audit; it is not an ingestion-throughput measurement.
Peak RSS was not collected, and the run does not qualify power loss, a published
image or multi-machine deployment. To repeat with a local debug binary:

```sh
.venv-brainwire/bin/python -m examples.python.duckdb_acceptance \
  --binary target/debug/brainwire --rows 100000
```
