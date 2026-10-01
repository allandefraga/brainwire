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
`0.0.0.0:7687` plus network access controls. There is currently no
authentication or TLS; expose this MVP only inside a trusted test network.

Example using the Neo4j Python driver:

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
Database selection uses an existing named database; `brainwire db create` is
an offline administration command.

`brainwire probe --mode ready` performs a full read-only Bolt query and exits
successfully only after a default database is serving it. `--mode live` stops
after the handshake and login, so it does not put query load on the store.
Use `--address` and `--timeout-ms` to target another local listener. Offline
`brainwire db schema NAME --json` shows the persisted ontology catalog and
reports `secondary_index_status: declared_only`; property indexes declared in
the ontology are not yet physically built or used by queries.
`brainwire db metrics NAME --json` reports per-column-family RocksDB estimates
for keys, live data, memtables and level-0 files. These are offline snapshots,
not exact graph counts or total process memory.

Each connection has one scoped query worker, with at most 64 concurrent
connections. RocksDB execution runs outside Tokio's IO threads. Results are
still fully materialized in memory, and the configured query-memory budget is
not yet enforced. The supported Cypher subset is documented in
[local queries](local-cypher.md). The TUI remains synthetic.

This is a single-node server. Kubernetes and Docker Swarm packaging are
explicit deployment goals; durable volumes and restarts on either orchestrator
come before multi-machine replication. Container orchestration alone does not
replicate the graph. The [data and recovery design](design/2026-09-27-brainwire-data-and-recovery.md)
defines the later replicated-cluster and failure acceptance criteria.
