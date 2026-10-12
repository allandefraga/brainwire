# Run the local client MVP

This starts a protected **single-node shadow graph** with synthetic data. Python
uses Bolt/Cypher; the connected TUI inspects ontology, indexes and shared query
memory. Use a fresh directory and unused loopback port. It does not configure
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

```sh
export BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/reader.json"
./target/release/brainwire shell --connect 127.0.0.1:17687 --graph tickets --view ontology
./target/release/brainwire shell --connect 127.0.0.1:17687 --graph tickets --view indexes
export BRAINWIRE_BOLT_AUTH_FILE="$BRAINWIRE_MVP_DIR/auth/observer.json"
./target/release/brainwire shell --connect 127.0.0.1:17687 --graph tickets --view memory --lang pt-BR
```

Press `r` to refresh and `q` to exit. Indexes support selection/scrolling. Memory
explains configured/current/peak/rejected **query reservations**, not RSS/cache.
An initial fetch failure exits; a later failure keeps an explicitly stale sample.
Use `--snapshot --width 120 --height 30` for a plain frame without a terminal.
Other real panels remain offline. [Connected bounds and permissions](connected-status.md).

For reads/writes, use Python's driver with `auth_from_env()` and an explicit
`driver.session(database="tickets")`; [protected Python examples](bolt-auth.md)
and [Cypher functions/limits](local-cypher.md) show the supported API. Native
clients take an IP:port, whereas Python takes `bolt://host:port`.

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

The locally tested Linux image is `brainwire:mvp-20261012`; its exact image ID is
recorded in the acceptance receipt. Rebuild it with
`docker build -t brainwire:mvp-20261012 .` and follow the
[protected container guide](protected-containers.md) for credentials, retained
storage and loopback exposure. Native `cypher --database` takes a store directory;
administrative `db` and connected `--graph` commands take configured graph names.
