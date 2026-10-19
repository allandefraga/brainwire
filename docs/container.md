# Run Brainwire in Docker

The [Compose/Kubernetes/Swarm guide](orchestrators.md) now includes runnable
single-writer templates and their local validation commands.

The image packages the **single-node** Bolt server and its administration CLI.
It runs independently of any application repository. Each application connects
over Bolt and chooses a named graph. These commands use synthetic data on
localhost. The default container profile has no replication or native TLS;
[experimental quorum replication](quorum-replication.md) is a separate profile. The [optional TLS proxy](protected-transport.md)
provides the qualified single-host encrypted Python entry path. Its base profile
is unprotected; optional
[service-token overlays](protected-containers.md) provide named-graph grants.
Use it only on a trusted/encrypted test
network. See [server limits](serve.md).

## Fastest local start

From the repository root, with Docker Engine running and Docker Compose installed,
this builds Brainwire, creates a persistent named volume, initializes the bundled
`demo` ontology once,
and starts the server:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml up --build --wait --wait-timeout 900
docker compose -p brainwire-demo -f deploy/compose.demo.yaml ps
docker compose -p brainwire-demo -f deploy/compose.demo.yaml exec brainwire brainwire probe --mode ready
```

Connect a client to `bolt://127.0.0.1:7687`, using database `demo`.
The explicit project name keeps this demo separate from other Compose projects.
The first build compiles RocksDB and can take several minutes; the 900-second
wait begins after building and waits for a healthy server. If port 7687 is busy,
prefix the startup command with `BRAINWIRE_BIND_PORT=17687` and connect to port
17687 instead. Inspect startup failures with
`docker compose -p brainwire-demo -f deploy/compose.demo.yaml logs`.

Stop the demo while retaining its named volume and graph data:

```sh
docker compose -p brainwire-demo -f deploy/compose.demo.yaml down
```

Adding `--volumes` to that command permanently deletes this demo database. Use
these commands to consume a versioned image from a private registry without
building it locally:

```sh
export BRAINWIRE_IMAGE=registry.example.com/owner/brainwire:0.1.0
docker compose -p brainwire-demo -f deploy/compose.demo.yaml pull
docker compose -p brainwire-demo -f deploy/compose.demo.yaml up --no-build --wait --wait-timeout 900
```

Log in to the registry first with `docker login`. `up` can be repeated safely;
the initializer skips an already-created `demo` graph.

Build and initialize a named Docker volume:

```sh
docker build -t brainwire:local .
docker volume create brainwire-data
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db create demo \
  --ontology /opt/brainwire/example-schema.json
```

Run the server, then connect a Bolt driver to `bolt://127.0.0.1:7687`:

```sh
docker run --detach --name brainwire-local --init --restart unless-stopped \
  --stop-timeout 60 \
  --publish 127.0.0.1:7687:7687 \
  --mount type=volume,src=brainwire-data,dst=/data \
  --memory 2g \
  --env BRAINWIRE_SERVER_DEFAULT_DATABASE=demo \
  --env BRAINWIRE_MEMORY_CACHE=256MiB \
  --env BRAINWIRE_MEMORY_MEMTABLES=32MiB \
  --env BRAINWIRE_MEMORY_QUERY=256MiB \
  brainwire:local
```

Check readiness before connecting. Repeat the probe until it exits successfully;
`docker logs brainwire-local` shows startup errors:

```sh
docker exec brainwire-local brainwire probe --mode ready
docker inspect --format '{{.State.Health.Status}}' brainwire-local
```

The image starts `brainwire serve` by default. `/data` holds all named graphs;
the graph `demo` was created offline with the bundled synthetic Customer/Company
ontology. `db create` refuses to replace an existing graph. Applying a real
ontology uses the same offline command path with a read-only schema mount. Stop the server before offline administration
so only one process opens each RocksDB directory.

## Write and query from Python

Run from any directory; the driver needs no Brainwire source checkout. This uses
the same Bolt/Cypher path as the repository's Python batch convenience API.
Create a virtual environment once, then run the example:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install neo4j==6.3.0
.venv/bin/python - <<'PYTHON'
from neo4j import GraphDatabase

with GraphDatabase.driver("bolt://127.0.0.1:7687", auth=None) as driver:
    with driver.session(database="demo") as session:
        # A bounded batch commits atomically; replay merges by stable entity_id.
        session.run("""
            UNWIND $rows AS row
            MERGE (c:Customer {entity_id:row.entity_id})
            SET c.name=row.name, c.email=row.email, c.segment=row.segment
        """, rows=[
            {"entity_id":"customer-1", "name":"Ada", "email":"ada@example.test", "segment":"beta"},
            {"entity_id":"customer-2", "name":"Grace", "email":"grace@example.test", "segment":"beta"},
        ]).consume()
        print(session.run("""
            MATCH (c:Customer {email:$email})
            RETURN c.entity_id AS id, c.name AS name
        """, email="ada@example.test").data())
        print(session.run("""
            CALL db.indexes() YIELD label, property, state
            RETURN label, property, state
        """).data())
PYTHON
```

The first result is `[{'id': 'customer-1', 'name': 'Ada'}]`. The bundled ontology
declares the email equality index; inspect its real readiness in the second
result. The driver uses direct `bolt://` connectivity. The supported language is
[the Brainwire Cypher subset](local-cypher.md); driver compatibility does not
imply the complete Neo4j feature set. For individual writes, property removal,
edges and iterable batches see [the Python convenience API](python-client.md).
That helper currently lives in the repository; it is not a published SDK.

For another application container on the same Docker network, connect to the
Brainwire service name (for example `bolt://brainwire:7687` in Compose).
`127.0.0.1` inside that application container addresses the application itself.
The database stays a separate service with its own persistent volume; the
application does not need to compile or embed it.

To connect services launched by **separate Compose projects**, create and share
an external Docker network. This development example uses the synthetic demo
profile, which has authentication disabled; use only synthetic data on this
trusted local network:

```sh
docker network create brainwire-integration
BRAINWIRE_SHARED_NETWORK=brainwire-integration \
  docker compose -p brainwire-demo \
  -f deploy/compose.demo.yaml -f deploy/compose.shared-network.yaml \
  up --build --wait --wait-timeout 900
```

Attach the consumer service to the same external network in its own Compose
file and configure its Bolt URI as `bolt://brainwire:7687`:

```yaml
services:
  consumer:
    environment:
      BRAINWIRE_BOLT_URI: bolt://brainwire:7687
    networks:
      - default
      - brainwire-integration

networks:
  brainwire-integration:
    external: true
    name: brainwire-integration
```

The Compose service name `brainwire` resolves through the network alias. Before
using real application data, switch to the protected service-token profile and
an encrypted/trusted transport boundary; this network overlay alone provides
neither authentication nor encryption. See [protected containers](protected-containers.md).

## Restart, inspect and stop

```sh
docker restart --timeout 60 brainwire-local
docker exec brainwire-local brainwire probe --mode ready
# Stop before inspecting the store with an offline administration command.
docker stop --timeout 60 brainwire-local
docker run --rm --mount type=volume,src=brainwire-data,dst=/data \
  brainwire:local db check demo
docker start brainwire-local
```

Re-run the Python example after readiness: the same two identities remain,
without duplicate nodes. Removing the container (`docker rm -f brainwire-local`)
does not remove the explicitly named volume. `docker volume rm brainwire-data`
permanently deletes this test database after its containers are removed.

## Application delivery

The current runnable path is to clone this independent repository, check out a
chosen revision and build `brainwire:local`. No published registry image is
promised by this guide. For private distribution, build and publish one versioned
image in the database repository's release process, then have application
operators pull that exact image tag or digest. Applications supply configuration,
credentials, ontology and retained storage; they do not build their own modified
database. The [Compose/Kubernetes/Swarm templates](orchestrators.md) are the
starting point for this packaging. Choose the protected token/TLS overlays for
remote access. This document does not publish an image or change a cluster.

## Storage and recovery boundaries

The container runs as UID/GID 10001 and expects its volume to be writable by
that user. The image does not declare an anonymous `VOLUME`: every deployment
must explicitly name or mount its durable storage. A fresh local volume is a
fresh database. Do not treat a container restart on a different machine with a
new local volume as recovery of the old member.

The image has a Docker health check that runs `brainwire probe --mode ready`.
That command negotiates Bolt, logs on and executes `RETURN 1` against the
default database. `brainwire probe --mode live` checks the session path without
running a query. The health check has a 180-second startup grace period; larger
recovery tests may need a longer one. The default probe targets
`127.0.0.1:7687`; override it if the container listener port changes.

For a restart check, write synthetic data, kill the container, then start a new
container with the **same** `brainwire-data` volume and read it back. The
[driver acceptance script](../crates/server/tests/mvp_demo.py) has `write` and
`read` modes for this purpose. The repeatable
[container restart test](../crates/server/tests/container_restart.py) creates
its own temporary volume, writes 10,000 synthetic Customers, performs five
kill/recreate cycles, interrupts uncommitted indexed overwrites, checks stale
index removal and runs an offline audit before removing its test resources:

```sh
python crates/server/tests/container_restart.py
```

This tests process/container recovery, not machine loss. The example ontology
is bundled only to make local test setup reproducible; use an appropriate
ontology for real graphs.

Before Kubernetes or Docker Swarm deployment, configure explicit startup,
readiness and liveness probes using these modes. Package stable member
identities and durable per-member volumes; run the failure matrix in the
[data and recovery design](design/2026-09-27-brainwire-data-and-recovery.md).
On Docker Swarm, service placement must keep each member bound to the correct
storage, or use a storage system that can reattach that member's volume. On
Kubernetes, a StatefulSet and persistent volume claims are the intended first
single-node pattern. Multi-machine high availability requires Brainwire's own
replication protocol and quorum tests in addition to either orchestrator.

The Dockerfile uses BuildKit cache mounts for Cargo downloads and build output.
The executable is copied outside the mount before the runtime image is built;
no compiler or writable build cache is packaged in the final image. A clean
first build still compiles RocksDB C++, while later local rebuilds reuse it.

The builder sets `RUSTUP_TOOLCHAIN=1.98.1` to match its base image, so the
repository's development `stable` channel cannot silently select another Rust
release. `CARGO_BUILD_JOBS` defaults to 2 to limit concurrent RocksDB C++ compiler
memory; builders with sufficient free resources can override it using
`docker build --build-arg CARGO_BUILD_JOBS=4 -t brainwire:local .`.

The locked shared target cache preserves compiled dependencies, including RocksDB.
Each uncached source build runs `cargo clean --release --workspace` first so
workspace artifacts from another build cannot appear newer than copied sources.
This prevents stale cross-crate APIs while retaining the expensive native cache.
