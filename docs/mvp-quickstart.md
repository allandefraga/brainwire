# Run the local client MVP

This starts a protected **single-node shadow graph** with synthetic data. Python
uses Bolt/Cypher; the connected TUI inspects ontology, indexes and shared query
memory, plus a bounded typed neighborhood in one workspace. Use a fresh directory and unused loopback port. It does not configure
replication, native TLS or a production cutover.

## Build and start

From the Brainwire repository root, in terminal A:

```sh
cargo build --release --locked --offline -p brainwire-server
export BRAINWIRE_MVP_DIR=$(mktemp -d /tmp/brainwire-mvp.XXXXXX)
python3 -m examples.python.create_service_tokens "$BRAINWIRE_MVP_DIR/auth" \
  --graph tickets --include-observer
export BRAINWIRE_SERVER_DATA_DIR="$BRAINWIRE_MVP_DIR/data"
export BRAINWIRE_SERVER_DEFAULT_DATABASE=tickets
export BRAINWIRE_SERVER_LISTEN=127.0.0.1:17687
export BRAINWIRE_SERVER_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/policy.json"
export BRAINWIRE_SERVER_MAX_MESSAGE=8MiB
export BRAINWIRE_SERVER_MAX_DECODED_MESSAGE=16MiB
export BRAINWIRE_SERVER_IO_TIMEOUT_MS=300000
export BRAINWIRE_MEMORY_CACHE=64MiB
export BRAINWIRE_MEMORY_MEMTABLES=16MiB
export BRAINWIRE_MEMORY_QUERY=64MiB
./target/release/brainwire db create tickets --ontology examples/client/ontology.json
echo "$BRAINWIRE_MVP_DIR"
./target/release/brainwire serve
```

Save the printed directory path. Keep the server in this terminal. Existing
graphs use their retained directory rather than another `mktemp`. Never start
two owners of the same RocksDB directory. Credentials are private files and
are not printed; the observer is explicitly enabled for process memory telemetry.
The pilot profile independently limits encoded requests to 8 MiB and decoded
request allocations to 16 MiB, with a 300-second authenticated IO budget. This
is not a whole-process memory ceiling. The ten-second handshake/authentication
budgets and cooperative query deadline are separate; see
[connection limits](connection-limits.md).

## Import through Python

In terminal B, from the same repository, replace the directory below with the
one printed above:

```sh
export BRAINWIRE_MVP_DIR=/tmp/brainwire-mvp.YOUR_SUFFIX
export BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/writer.json"
python3 -m examples.client.generate_fixture "$BRAINWIRE_MVP_DIR/snapshot.jsonl" --tickets 10000
uv run --no-project --with neo4j==6.3.0 python -m examples.client.ingest_snapshot \
  "$BRAINWIRE_MVP_DIR/snapshot.jsonl" --database tickets \
  --uri bolt://127.0.0.1:17687 --source-id synthetic-snapshot-001
```

The fixture has 10,000 tickets, 20,000 comments and 20,000 HAS_COMMENT edges.
Rerunning the same immutable file/source ID resumes its durable cursor; completed
replay makes no writes. New revisions need new source IDs and chronological,
serial import. See [client source export](client-shadow.md) to prepare actual
DuckDB or version-pinned local SQLite DuckLake snapshots. Source deletion and
ticket history are not synchronized by this importer.

## Inspect while the server runs

Use the observer to access every read-only pane. The first synthetic ticket has
this exact adapter identity; retain the shell quotes:

```sh
export BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/observer.json"
./target/release/brainwire shell --connect 127.0.0.1:17687 --graph tickets \
  --node-label Ticket --entity-id '["tenant-0","zendesk-main","ticket","0"]' \
  --edge-type HAS_COMMENT --target-label TicketComment
```

No `--view` is required: the connected default is the shared workspace, starting
on Ontology. Tab/Shift+Tab or `1`/`2`/`3`/`4` switches between Ontology, Memory,
Indexes and Graph. Each pane fetches lazily, preserves its selection and sample
age, and refreshes only when `r` is pressed. Press `q` to exit. Add `--lang pt-BR`
for Portuguese; English is the default and stored identifiers do not change.

Indexes support selection/scrolling. Memory explains configured/current/peak/
rejected **query reservations**, not RSS/cache. Graph samples at most 20 outgoing
`HAS_COMMENT` relationship rows for this fixed ticket, with real target IDs;
**MORE AVAILABLE** means an additional matching row was observed. It is not full
degree or a whole-graph rendering. Up/Down and PgUp/PgDn select the sampled rows.
This fixture has two comments per ticket, so its initial sample has two rows.
The four Graph flags are fixed for the terminal session; leaving them out gives
a navigable Graph configuration error while other panes remain usable.

Use a regular reader to test the graph-only permission boundary:

```sh
export BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/reader.json"
./target/release/brainwire shell --connect 127.0.0.1:17687 --graph tickets \
  --node-label Ticket --entity-id '["tenant-0","zendesk-main","ticket","0"]' \
  --edge-type HAS_COMMENT --target-label TicketComment
```

Memory will show an administrator-permission error, while the other three panes
remain available. An interactive workspace initial failure stays navigable; a
later failure retains an explicitly stale sample. Successful refresh clears the
error. Refresh remains synchronous and may wait for the fetch deadline. Use
`--snapshot --width 120 --height 30` for the initial Ontology frame without a
terminal; a failed initial snapshot exits nonzero. Explicit `--view ontology`,
`--view memory`, `--view indexes` and `--view graph` still open standalone panes.
See [connected bounds and permissions](connected-status.md) and the
[real 100,000-node workspace captures](design/assets/2026-10-13-beta/README.md).

For reads/writes, use Python's driver with `auth_from_env()` and an explicit
session database. The snapshot importer already configures
`liveness_check_timeout=30`; keep that setting for applications that reuse pools:

```python
from neo4j import GraphDatabase
from examples.python.connection import auth_from_env

with GraphDatabase.driver(
    "bolt://127.0.0.1:17687",
    auth=auth_from_env(),
    liveness_check_timeout=30,
    connection_timeout=5,
    max_transaction_retry_time=0,
) as driver:
    with driver.session(database="tickets") as session:
        print(session.run("RETURN 1 AS ok").single()["ok"])
```

Liveness checks help avoid reuse of expired idle sockets; they do not make writes
idempotent. Reconcile durable IDs/checkpoints after a missing acknowledgement.
[Protected Python examples](bolt-auth.md) and [Cypher functions/limits](local-cypher.md)
show the supported API. Native clients take an IP:port; Python takes a Bolt URL.
The native server and TUI still use plaintext Bolt. Keep this quickstart on trusted
loopback. For Python through a verified TLS endpoint, follow the
[optional single-host TLS guide](protected-transport.md), with `bolt+s` and strict
CA/hostname checks; the native TUI does not yet support that TLS endpoint.

## Stop, audit and recover

Stop the server in terminal A before offline commands, keeping its exported
environment. Ordinary stop is not a native application-drain guarantee; pending
transactions are discarded on crash/reopen and committed WAL data is retained.

```sh
./target/release/brainwire db check tickets --json
./target/release/brainwire db stats tickets --json
./target/release/brainwire db checkpoint tickets --destination "$BRAINWIRE_MVP_DIR/checkpoint"
./target/release/brainwire serve
```

The checkpoint destination must be new. Restoring requires a stopped server and
a fresh store; do not copy live RocksDB files or overwrite an active graph. See
[checkpoint/commit receipts](commit-state.md) and the
[single-node container guide](container.md). Running Python import again after
restart verifies resume against the same source digest/mapping.

For a disposable automated rehearsal, including forced termination and exact
audits, run the [MVP acceptance harness](benchmarks/2026-10-12-mvp.md). The
[October beta roadmap](2026-10-pilot-roadmap.md) separates implemented capabilities
from the source, workload and deployment gates still required for client.

Build a local Linux image for the current checkout with
`docker build -t brainwire:local .` and follow the
[protected container guide](protected-containers.md) for credentials, retained
storage and loopback exposure. Native `cypher --database` takes a store directory;
administrative `db` and connected `--graph` commands take configured graph names.
