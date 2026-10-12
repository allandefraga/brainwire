# Local Bolt server

`brainwire serve` opens the named databases under `server.data_dir` and listens
on `server.listen` (default `127.0.0.1:7687`). It uses the same RocksDB store as
the embedded CLI. Only one process can hold a database; stop the server before
running offline `brainwire db` or `brainwire cypher` commands against it.

```sh
brainwire db create client_memory --ontology crates/core/tests/fixtures/graph_mapping.json
brainwire serve
```

To use a separate data directory or port, set `BRAINWIRE_SERVER_DATA_DIR` and
`BRAINWIRE_SERVER_LISTEN`, or put them in `brainwire.toml` and pass `--config`.
Container deployments must mount a retained volume at `server.data_dir` and
set memory budgets appropriate to that container. The default listen address
is loopback, so a container needs an explicit bind address such as
`0.0.0.0:7687` plus network access controls. Authentication is optional; without
`server.auth_file` the server runs unprotected. Native TLS remains unavailable;
use a trusted/encrypted transport boundary for protected shared connections.

Development-mode example using the Neo4j Python driver on isolated loopback:

```python
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    with driver.session(database="client_memory") as session:
        record = session.run("RETURN 1 AS n").single()
        print(record["n"])
```

The server preserves explicit transaction writes across `RUN` calls. `COMMIT`
persists them; `ROLLBACK`, `RESET`, a query error or connection teardown
discards them. An autocommit query persists when its result is consumed.
Storage sequence numbers are not exposed as restart-stable Bolt bookmarks.
`CALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope`
reads the durable local position captured by the current transaction. Its decimal
string is separate from a causal bookmark or a source ingestion cursor; see
[commit state](commit-state.md).
Database selection uses an existing named database; `brainwire db create` is
an offline administration command.

`brainwire probe --mode ready` performs a full read-only Bolt query and exits
successfully only after a default database is serving it. `--mode live` stops
after the handshake and login, so it does not put query load on the store.
Use `--address` and `--timeout-ms` to target another local listener. Offline
`brainwire db schema NAME --json` shows the persisted ontology catalog and
reports `secondary_index_status: scalar_equality_ready` when scalar
equality indexes are built. A legacy database with declarations but no ready
marker reports `declared_only` and uses safe label scans until its ontology is
reapplied and backfilled. Finite numeric equality is supported; list, map, range and vector indexes are pending.
It also reports temporal history and maintenance as `not_implemented` so
timeline metadata cannot be mistaken for stored ticket revisions or cleanup.
`brainwire db metrics NAME --json` reports per-column-family RocksDB estimates
for keys, live data, memtables and level-0 files. These are offline snapshots,
not exact graph counts or total process memory.

Each connection has one scoped query worker, with at most 64 concurrent
connections. RocksDB execution runs outside Tokio's IO threads. Results are
still materialized in memory, while simple reads process intermediate rows
incrementally. `memory.query` bounds conservative per-RUN accounting and the
shared server reservation pool; `CALL db.memory()` exposes pool telemetry.
`server.query_timeout_ms` sets a cooperative execution deadline (30 seconds by
default). Resource failures roll back the complete transaction. These are not
hard RSS or wall-clock ceilings. See [query limits](query-allocation-guards.md).
The supported Cypher subset is documented in
[local queries](local-cypher.md). The Graph/Query/Memory/Disk TUI demo remains
synthetic; `shell --overview` reads real named local graphs while the server is
stopped.

This is a single-node server. Kubernetes and Docker Swarm packaging are
explicit deployment goals; durable volumes and restarts on either orchestrator
come before multi-machine replication. Container orchestration alone does not
replicate the graph. The [data and recovery design](design/2026-09-27-brainwire-data-and-recovery.md)
defines the later replicated-cluster and failure acceptance criteria.

## Connected ontology inspection

`brainwire shell --connect 127.0.0.1:7687 --graph client_memory --view ontology`
reads a complete schema snapshot while this server is running. It uses the same
`CALL db.schema()` procedure available to Python Bolt clients; `r` refreshes and
failed refreshes retain an explicitly stale snapshot. See the
[connected ontology guide](connected-ontology.md) for bounds and controls.

## Optional protected connections

Set `server.auth_file` / `BRAINWIRE_SERVER_AUTH_FILE` to a service-token policy
path. Invalid configured policy fails before listening; no configured policy
retains development mode with a startup warning. See [protected Bolt](bolt-auth.md)
for graph/read/write grants, native/Python clients, restart-based rotation and
transport limits. Authenticated containers use the [secret overlays](protected-containers.md).
