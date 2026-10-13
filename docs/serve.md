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
use the optional [single-host TLS proxy](protected-transport.md) or another
qualified encrypted boundary for protected shared connections.

Development-mode example using the Neo4j Python driver on isolated loopback:

```python
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None,
                          liveness_check_timeout=30) as driver:
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
connections. Negotiating the 20-byte handshake and version reply has one absolute
10-second deadline; silent/slow-drip peers cannot retain a connection slot forever
before negotiation. After negotiation, authentication has a separate absolute
10-second IO budget, retained through HELLO, partial messages, NOOPs and the login
reply. LOGOFF starts a fresh authentication budget. Expiry closes the session and
releases its slot; synchronous parsing, engine work and cleanup can delay
enforcement. After authentication, `server.io_timeout_ms` /
`BRAINWIRE_SERVER_IO_TIMEOUT_MS` gives complete request reads and response writes
separate absolute IO budgets, defaulting to 300,000 milliseconds. NOOPs and partial
bytes do not renew the request budget; complete requests and response delivery
start the next budget. Expiry closes idle explicit transactions and rolls back
their pending changes. This does not bound synchronous engine initialization,
execute native query cancellation or provide transaction drain. RocksDB execution
runs outside Tokio's IO threads. Response encoding handles one frame at a time,
but session results are still materialized in memory; simple reads process
intermediate rows incrementally. `memory.query` bounds conservative per-RUN accounting and the
shared server reservation pool; `CALL db.memory()` exposes pool telemetry.
`server.query_timeout_ms` sets a cooperative execution deadline (30 seconds by
default). Resource failures roll back the complete transaction. These are not
hard RSS or wall-clock ceilings. See [query limits](query-allocation-guards.md).
`server.max_message` / `BRAINWIRE_SERVER_MAX_MESSAGE` limits assembled PackStream
payload bytes **per request**, excluding chunk headers and NOOPs. Multiple valid
messages in one TCP read each have their own cap. An announced chunk exceeding
the remaining budget is rejected before waiting for its body; trailing bytes
after a PackStream request are a fatal protocol violation. The default is
256 MiB; this is not a process-memory cap or a recommendation for ingestion batch
size. `server.max_decoded_message` / `BRAINWIRE_SERVER_MAX_DECODED_MESSAGE`
separately limits PackStream owned payloads and container backing storage before
request dispatch, defaulting to 64 MiB per request. Compact encoded containers
can exceed this decoded budget; rejection is fatal and closes the session. These
per-connection allowances are not an aggregate RSS cap. Keep the separate bounded
Python input defaults for the pilot. See [connection limits](connection-limits.md)
for configuration, idle transactions and Python pool reuse.
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
Connected `--view memory` and `--view indexes` inspect real telemetry and configured
index state; see [connected status](connected-status.md). Memory requires instance
administration permission as well as graph read access.

## Optional protected connections

Set `server.auth_file` / `BRAINWIRE_SERVER_AUTH_FILE` to a service-token policy
path. Invalid configured policy fails before listening; no configured policy
retains development mode with a startup warning. See [protected Bolt](bolt-auth.md)
for graph/read/write grants, native/Python clients, restart-based rotation and
transport limits. Authenticated containers use the [secret overlays](protected-containers.md).
The [TLS proxy guide](protected-transport.md) qualifies strict Python certificate
checks and pool reuse across the proxy's shorter 60-second idle timeout. Native
SIGTERM remains abrupt; see [termination recovery](termination-recovery.md).
