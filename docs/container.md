# Run Brainwire in Docker

Use Docker Compose for the first local trial. It creates a synthetic graph
named `demo` and keeps its files in a named Docker volume. You can use a
published image without compiling Rust, or build from source. You need Docker
Engine and Docker Compose. Run these commands from a checkout of the matching
Brainwire release.

This guide uses an unauthenticated demo on host loopback. For shared or real
data, use the [protected container setup](protected-containers.md) and a
[verified encrypted connection](protected-transport.md). The native Bolt server
has no TLS. The normal Docker profiles run one database instance;
[experimental quorum replication](quorum-replication.md) has a separate setup.

## Start from a release image

A source tag can exist before its image. Check the matching GitHub release and
its workflow before using it. Private images need a one-time login:

```sh
docker login ghcr.io
```

Enter your GitHub username. At the password prompt, use a GitHub personal access
token with `read:packages`
and access to the package. Do not enter your GitHub account password. Public
images do not need this step. Then start the published version:

```sh
export BRAINWIRE_IMAGE=ghcr.io/allandefraga/brainwire:v0.15.0-beta.10 # brainwire-release-version
docker compose -p brainwire-demo -f deploy/compose.demo.yaml pull
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --no-build --wait --wait-timeout 900
```

Images use the exact release tag, for example `v0.15.0-beta.10`. <!-- brainwire-release-version -->
For reproducible deployment, pin the digest from the release receipt. A private
GHCR image requires package access in addition to repository access. Keep
credentials out of shell arguments and source files. If `pull` reports
`denied` or `unauthorized`, check the token and package access; rebuilding the
source is the alternative below.

Both commands use the same `BRAINWIRE_IMAGE`: the initializer and server must
run the same version. This trial publishes only `127.0.0.1:7687`; it does not
add authentication. Never expose the demo profile to an untrusted Docker
network or external clients.

## Build from source instead

```sh
unset BRAINWIRE_IMAGE
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  up --build --wait --wait-timeout 900
```

The first build compiles Rust and RocksDB and can take several minutes. After
the build, Compose waits up to 900 seconds for query readiness. The Bolt address
is `bolt://127.0.0.1:7687` and the database name is `demo`.

If port 7687 is busy, run the startup command with
`BRAINWIRE_BIND_PORT=17687` before `docker compose`, then connect to port 17687.
Use the same project name and Compose file for later management commands:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml ps
docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs brainwire
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire probe --mode ready
```

The initializer skips an existing `demo` graph. Restarting this Compose project
with its original volume preserves the graph. Use `up --no-build` when managing
a published image; use `up --build` for a source build.

## Write and read from Python

The database runs in Docker; Python can run on the host. Create an environment
and install the Bolt driver:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install neo4j==6.3.0
.venv/bin/python - <<'PYTHON'
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None,
                          max_transaction_retry_time=0) as driver:
    with driver.session(database="demo") as session:
        session.run("""
            UNWIND $rows AS row
            MERGE (c:Customer {entity_id:row.id})
            SET c.name=row.name, c.email=row.email
        """, rows=[
            {"id":"customer-1", "name":"Ada", "email":"ada@example.test"},
            {"id":"customer-2", "name":"Grace", "email":"grace@example.test"},
        ]).consume()
        print(session.run("""
            MATCH (c:Customer {email:$email})
            RETURN c.entity_id AS id, c.name AS name
        """, email="ada@example.test").data())
PYTHON
```

Expected output: `[{'id': 'customer-1', 'name': 'Ada'}]`.
The demo ontology includes `Customer` and an email equality index. Consume each
write result to complete the autocommit transaction. A lost commit response is
an unknown outcome; reconcile the stable IDs before retrying.

For bounded upserts and property patches, the repository also has a
[locally installable Python helper](python-package.md). It uses the Neo4j driver;
a public PyPI package is not claimed. See the [mutation contract](python-client.md)
and [supported Cypher](local-cypher.md).

## Open the TUI

Open a second terminal and run the TUI inside the running container:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --connect 127.0.0.1:7687 --graph demo
```

Use `Tab` or `1`–`4` for Ontology, Memory, Indexes and Graph; `r` refreshes,
`?` explains the current pane, and `q` exits. The connected workspace is
read-only. The Graph pane needs four selection flags; the
[connected workspace guide](connected-status.md) shows an example.

For an entirely synthetic visual demonstration, run:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  exec brainwire brainwire shell --demo
```

That demo does not inspect the running database. See [TUI modes and controls](tui-demo.md).

## Stop, restart and keep the data

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml restart brainwire
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

`down` removes the containers and retains the named volume. Run the startup
command again to reopen it. Adding `--volumes` deletes this demo's data.
A new volume on another machine is a new database, not recovery of this one.
Keep independent backups; see [checkpoints](checkpoints.md) and
[termination and recovery](termination-recovery.md).

For an upgrade, take and verify a backup first, keep the same project/volume,
then set `BRAINWIRE_IMAGE` to the new exact tag and repeat `pull` and
`up --no-build`. Check the release notes for volume compatibility. Switching
back to an older image is not a safe rollback plan after a volume-format change.

## Start a local demo with one command

After building `brainwire:local` with `docker build -t brainwire:local .`, you
can start the synthetic demo with one command. This exposes an unauthenticated
server on host loopback only and is intended for local experiments:

```sh
docker run -d --name brainwire-local -p 127.0.0.1:7687:7687 -v brainwire-local-data:/data -e BRAINWIRE_SERVER_DEFAULT_DATABASE=demo --entrypoint /bin/sh brainwire:local -ec 'if [ ! -f /data/demo/CURRENT ]; then brainwire db create demo --ontology /opt/brainwire/example-schema.json; fi; exec brainwire serve --allow-unauthenticated-remote'
```

The named volume keeps the graph when the container stops. For shared or real
data, follow the [protected container guide](protected-containers.md).
Validate this bootstrap with the exact image and environment you use. A demo
startup alone does not qualify production operation or recovery.

## Use your own ontology and a single container

Compose is the shortest setup. The equivalent separate steps make the storage
and initialization explicit:

```sh
docker build -t brainwire:local .
docker volume create brainwire-data
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db create demo --ontology /opt/brainwire/example-schema.json
docker run --detach --name brainwire-local --init --restart unless-stopped \
  --stop-timeout 60 --publish 127.0.0.1:7687:7687 \
  --mount type=volume,src=brainwire-data,dst=/data --memory 2g \
  --env BRAINWIRE_SERVER_DEFAULT_DATABASE=demo \
  --env BRAINWIRE_MEMORY_CACHE=256MiB \
  --env BRAINWIRE_MEMORY_MEMTABLES=32MiB \
  --env BRAINWIRE_MEMORY_QUERY=256MiB \
  brainwire:local serve --allow-unauthenticated-remote
```

To create a real graph, mount your ontology JSON read-only and pass its container
path to `db create`. That command refuses to replace an existing graph. The
container runs as UID/GID 10001; bind-mounted data must be writable by that user.
All named graphs live below `/data`. The image includes the CLI and license at
`/usr/share/licenses/brainwire/LICENSE`.

Stop the server before opening the same files with an offline command:

```sh
docker stop --timeout 60 brainwire-local
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db check demo
docker start brainwire-local
```

The `--memory` Docker limit constrains the container. Brainwire component budgets
are separate estimates; they do not prove a process RSS ceiling or no-OOM
behavior. Read [memory defaults and diagnostics](memory-defaults.md).

## Connect another application container

On the same Compose network, use `bolt://brainwire:7687`. `127.0.0.1` inside
another container points to that container, not to Brainwire. For separate
Compose projects, the [shared network overlay](../deploy/compose.shared-network.yaml)
uses an explicit external network:

```sh
docker network create brainwire-integration
BRAINWIRE_SHARED_NETWORK=brainwire-integration \
  docker compose -p brainwire-demo -f deploy/compose.demo.yaml \
  -f deploy/compose.shared-network.yaml up --build --wait --wait-timeout 900
```

Attach the consumer to that network in its own Compose configuration. The demo
is still unauthenticated; the network itself adds no authentication or encryption.
Use the [protected profiles](protected-containers.md) before sharing real data.

## Before connecting a real application

Use the [protected container setup](protected-containers.md) to mount a server
auth policy and a separate health credential; leave the development override
disabled. Keep the host listener on loopback or an explicitly trusted network.
For traffic outside that boundary, configure and verify the
[TLS proxy](protected-transport.md). Native Bolt TLS is not available, and the
proxy-to-database connection remains plaintext on its internal bridge.

The Compose profiles run one active database on one retained volume. They do
not provide machine-loss failover. The experimental beta still needs
representative sustained memory and recovery qualification before it can be
treated as production-ready. Start with rebuildable data and a tested backup.

## Health checks and deployment

| Check | What it proves |
| --- | --- |
| `brainwire probe --mode handshake` | Bolt negotiation and HELLO answer; no credential or database query |
| `brainwire probe --mode live` | Negotiation and login succeed; no query |
| `brainwire probe --mode ready` | The default database serves `RETURN 1` |

The Dockerfile's built-in health check uses **handshake**. The demo Compose file
overrides it with **ready**. Configure the appropriate probe and credentials for
your deployment; a responding listener alone does not establish query readiness.
Use `--address` if the listener changes and `--timeout-ms` to bound a probe.
An `unhealthy` status does not itself restart a standalone Docker container;
`restart: unless-stopped` restarts a stopped process. Check logs and readiness
after every upgrade instead of relying only on the container being `running`.

[Docker/Compose, Kubernetes and Swarm deployment](orchestrators.md) covers the
current single-instance templates. Keep the correct persistent volume attached
to its instance. Multiple containers do not create graph replicas automatically.
The [container restart harness](../crates/server/tests/container_restart.py)
uses its own synthetic data to check retained-volume recovery; that test does
not establish machine-loss recovery.
