# Run the Bolt server

`brainwire serve` opens the named graphs below `server.data_dir` and accepts
Cypher over Bolt. The default address is `127.0.0.1:7687`. Python clients and the
connected TUI use this server; they do not open its RocksDB files.

If you want Docker, start with the [Docker quickstart](container.md). For native
source builds:

```sh
cargo build --release --locked -p brainwire-server
./target/release/brainwire db create default \
  --ontology crates/core/tests/fixtures/graph_mapping.json
./target/release/brainwire serve
```

Run from the repository root for this bundled synthetic ontology. In the rest
of this guide, `brainwire` means the built binary; use
`./target/release/brainwire` if it is not on your PATH. A real graph needs its
own [ontology/catalog](online-catalog.md) rather than the example fixture.

## Configuration

Configuration comes from detected defaults, `brainwire.toml`, then environment
overrides. Inspect the effective values and sources:

```sh
brainwire config show
brainwire config init
```

`config init` creates a file and refuses to overwrite one without `--force`.
Pass `--config /path/to/brainwire.toml` to use a specific file. For another data
directory or port, set the environment before both initialization and serving:

```sh
export BRAINWIRE_SERVER_DATA_DIR=/path/to/brainwire-data
export BRAINWIRE_SERVER_LISTEN=127.0.0.1:17687
brainwire db create demo --ontology /path/to/ontology.json
brainwire serve
```

Keep the data directory on retained storage. A container must mount that
storage explicitly. `memory.cache`, `memory.memtables`, `memory.query` and
`memory.ingress` control separate components; memtables are included in cache
accounting. `config show` reports configured-budget diagnostics. These settings
do not establish a global RSS ceiling. Read [memory defaults](memory-defaults.md)
and [query allocation guards](query-allocation-guards.md) before sizing a load.

## Connect from Python

Install the driver in a virtual environment, then use a named database:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install neo4j==6.3.0
.venv/bin/python - <<'PYTHON'
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None,
                          liveness_check_timeout=30,
                          max_transaction_retry_time=0) as driver:
    with driver.session(database="default") as session:
        print(session.run("RETURN 1 AS n").single()["n"])
PYTHON
```

This is the isolated, unauthenticated loopback demo. Shared or remote connections
need a policy and encrypted transport. For writes and batches, see the
[Python helper](python-package.md), [mutation contract](python-client.md) and
[supported Cypher](local-cypher.md). Compatible Bolt drivers do not imply full
Neo4j language or administration compatibility.

## Use online and offline commands correctly

Only one process can own a RocksDB directory. Stop `serve` before using offline
administration or local queries against that same data:

| Server running | Server stopped / exclusive access |
| --- | --- |
| Python Bolt queries and mutations | `brainwire db create NAME --ontology FILE` |
| Connected `brainwire shell --connect IP:PORT --graph NAME` | `brainwire db check NAME` |
| `CALL db.schema()`, `db.indexes()`, `db.memory()` | `brainwire db schema NAME --json`, `db metrics NAME --json` |
| Admin-only online `CALL db.checkpoint(name)` when configured | `brainwire cypher --database DIRECTORY --query QUERY` |
| Online supported catalog/index procedures | `brainwire shell --overview` |

`db create` is an offline command and refuses to replace an existing graph.
Database selection over Bolt chooses an existing named graph. Online checkpoint
creation requires `server.checkpoint_dir` plus administrator permissions; see
[checkpoints](checkpoints.md).

The schema command reports actual persisted declarations and index readiness.
Scalar equality and `IN`, plus supported string range/prefix predicates, can use
indexes; other predicates may scan. See [indexes](indexes.md). Timeline/catalog
metadata does not create stored revisions or time-travel reads; see
[temporal data](temporal-data.md).

`db metrics` is an offline snapshot of RocksDB estimates such as keys, live data,
memtables and level-0 files. It is not an exact graph count or total process
memory measurement.

## Health checks

```sh
brainwire probe --mode ready
brainwire probe --mode live --address 127.0.0.1:17687 --timeout-ms 2000
```

| Mode | Checks |
| --- | --- |
| `handshake` | Bolt negotiation and HELLO; works without a credential, no database query |
| `live` | Negotiation and login, no query |
| `ready` | Login and a read-only `RETURN 1` through the default database |

A protected `live` or `ready` check uses `BRAINWIRE_BOLT_AUTH_FILE`. Probe addresses
are numeric `IP:PORT`; total timeout accepts 1–30,000 ms. Readiness is not a
certificate of workload capacity or disk integrity. Docker's image health check
uses `handshake`; the demo Compose profile overrides it with `ready`.

On Unix, native Bolt clients reject credential files writable by group or others.
Read-only secret mounts remain supported. Use a private owner-readable file such
as mode `0600` where possible; accepting a readable file is not a confidentiality
guarantee. This check applies to the opened file, including mounted symlinks.

## Inspect with the TUI

```sh
brainwire shell --connect 127.0.0.1:7687 --graph default
```

Use `Tab` or `1`–`4` for Ontology, Memory, Indexes and Graph, `r` to refresh,
`?` for help and `q` to quit. The Graph pane needs a source and outgoing type
selection. See [the connected workspace](connected-status.md).

Memory reads query-reservation counters and requires instance-administrator
permission as well as graph read access. Ordinary graph readers can still use
other authorized panes. The connected TUI is read-only; `shell --demo` is a
separate synthetic demonstration and `shell --overview` requires the server to
be stopped. See [all TUI modes](tui-demo.md).

## Transactions and interrupted writes

An explicit transaction may contain several `RUN` calls. `COMMIT` persists its
writes; `ROLLBACK`, `RESET`, query error or connection teardown discards pending
writes. An autocommit write completes when its result is consumed.

If a client loses the connection after submitting COMMIT, the outcome can be
unknown: the write may already be durable. Do not automatically repeat a mutation
without reconciling stable identities or a durable source checkpoint. Explicit
request IDs have a separate experimental quorum contract; see
[commit state](commit-state.md) and [quorum replication](quorum-replication.md).

`CALL db.commitState()` returns the local durable sequence captured by the
transaction. It is not a causal bookmark, source cursor, transaction-status API
or historical snapshot ID.

SIGTERM/SIGINT stop new work and begin a bounded drain. The default
`server.shutdown_grace_ms` is 8,000 ms; container/orchestrator stop grace should
allow it to finish. A timed-out network response may follow a commit that already
completed. See [termination and recovery](termination-recovery.md).

## Authentication and transport

Without a policy, the server permits loopback and rejects non-loopback listeners
unless `--allow-unauthenticated-remote` is explicitly supplied. That override is
for isolated synthetic development only; the Docker demo uses it behind a host
loopback port mapping.

For shared access, set `server.auth_file` or `BRAINWIRE_SERVER_AUTH_FILE` to a
private service-token policy file. A configured policy must be valid before the
server listens. Use [protected Bolt](bolt-auth.md) for grants and credentials,
[protected containers](protected-containers.md) for secret mounts, and
[protected transport](protected-transport.md) for the optional TLS proxy.
Native Bolt and native TUI TLS are not implemented.

A Docker network, Kubernetes Service or Swarm overlay does not provide the
authentication/encrypted boundary by itself. The standard deployment templates
run one instance; extra pods are not automatic replicas.

## Resource and timeout boundaries

| Setting | Default / role |
| --- | --- |
| Connections | At most 64 admitted sessions |
| Handshake | Absolute 10-second IO deadline |
| Authentication | Separate absolute 10-second IO deadline; LOGOFF starts another |
| `server.io_timeout_ms` | 300,000 ms for each complete authenticated request read and response write |
| `server.query_timeout_ms` | 30,000 ms cooperative query execution deadline |
| `server.max_message` | 256 MiB assembled payload per request |
| `server.max_decoded_message` | 64 MiB accounted owned decoded storage per request |
| `memory.ingress` | Shared admission of accounted request storage across connections |
| `memory.query` | Shared query reservation pool plus per-RUN accounting |

Partial bytes and NOOPs do not renew an IO deadline. Complete request processing
and response delivery begin the next request budget. Expiry closes the session
and discards still-pending writes. Synchronous parsing, engine/storage work and
cleanup can delay enforcement; these are not preemptive wall-clock guarantees.

The decoder rejects malformed, trailing or oversized requests before dispatch.
Encoded bytes and decoded containers need different limits; a small encoded
list can require much more memory after decoding. These defaults are not a
recommended batch size. Response encoding uses bounded frames, but result rows
can still be materialized before sending. Native allocations, cache behavior
and other components remain outside a proven process-wide RSS cap.

See [connection limits](connection-limits.md), [memory diagnostics](memory-defaults.md)
and the [production readiness roadmap](production-readiness-roadmap.md) for
qualification work and the currently open limits.
